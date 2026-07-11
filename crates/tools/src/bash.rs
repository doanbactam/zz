//! `bash` tool — runs shell commands via `tokio::process::Command` (default,
//! non-interactive) or a pseudo-terminal (interactive / PTY mode).
//!
//! applies sandbox restrictions (Landlock + seccomp on Linux)
//! in the child process via `pre_exec` hook. Parent (tokio runtime) is
//! never restricted — only the child inherits sandbox rules.
//!
//! (cycle-112-pty): adds interactive PTY execution (parity F13 vs
//! Codex — foreground TTY, background tasks, kill/attach) on top of the
//! existing non-interactive path. PTY mode is built on `portable_pty`
//! (already a workspace dependency). `openpty` creates a *virtual* pty
//! pair, so no real TTY/terminal is required — safe to use headless/CI.

use crate::{Tool, ToolCategory, get_str};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::Command;
use zerozero_sandbox::{
    NetPolicy, SandboxPolicy, build_net_command, bwrap_available, userns_enabled,
};

const TIMEOUT_SECS: u64 = 30;
/// Maximum number of completed background tasks to retain in memory.
/// Older completed entries are evicted when this limit is exceeded.
const MAX_COMPLETED_TASKS: usize = 50;

/// Resolved set of options for a single bash invocation. Separated out of
/// `execute` so it can be unit-tested without spawning processes
/// (parameters parsed, not executed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashSpec {
    pub command: String,
    pub interactive: bool,
    pub run_in_background: bool,
    pub timeout_secs: u64,
}

/// Target operating system for command construction (cross-platform
/// exec hardening). A pure enum so the shell-selection logic can be unit
/// tested without `#[cfg(target_os=...)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    Windows,
    Unix,
}

impl TargetOs {
    /// Best-effort detection of the current OS at runtime. Falls back to
    /// Unix semantics when the target family is unknown (Linux/macOS/BSD
    /// all use POSIX `sh -c`).
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// Build a [`Command`] that runs `cmd` through the platform-appropriate
/// shell:
///
/// * **Windows** → `cmd.exe /c <cmd>`
/// * **Unix** (Linux/macOS/BSD) → `sh -c <cmd>`
///
/// This is the pure, spawn-free constructor extracted from the executor so
/// the per-OS command line can be asserted directly (no process is spawned).
pub fn build_shell_command(os: TargetOs, cmd: &str) -> Command {
    match os {
        TargetOs::Windows => {
            let mut c = Command::new("cmd.exe");
            c.arg("/c").arg(cmd);
            c
        }
        TargetOs::Unix => {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd);
            c
        }
    }
}

impl BashSpec {
    /// Parse a `bash` tool-call `args` JSON into a `BashSpec`.
    ///
    /// `interactive`, `run_in_background`, and `timeout_secs` default
    /// sensibly (`false`, `false`, `TIMEOUT_SECS`) when omitted.
    pub fn from_args(args: &serde_json::Value) -> anyhow::Result<Self> {
        let command = get_str(args, "command")?;
        let interactive = args
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let run_in_background = args
            .get("run_in_background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(TIMEOUT_SECS);
        Ok(Self {
            command,
            interactive,
            run_in_background,
            timeout_secs,
        })
    }
}

/// State of a background task spawned via `run_in_background: true`.
#[derive(Default)]
struct BackgroundTask {
    output: String,
    exit_code: Option<i32>,
    done: bool,
}

pub struct BashTool {
    sandbox: Arc<SandboxPolicy>,
    net_policy: Arc<NetPolicy>,
    /// Registry of in-flight / completed background tasks, keyed by task_id.
    background_tasks: Arc<Mutex<HashMap<String, Arc<Mutex<BackgroundTask>>>>>,
    /// Monotonic counter for generating task_ids.
    task_counter: Arc<Mutex<u64>>,
}

impl Clone for BashTool {
    fn clone(&self) -> Self {
        Self {
            sandbox: self.sandbox.clone(),
            net_policy: self.net_policy.clone(),
            background_tasks: self.background_tasks.clone(),
            task_counter: self.task_counter.clone(),
        }
    }
}

impl BashTool {
    pub fn new(sandbox: Arc<SandboxPolicy>) -> Self {
        Self {
            sandbox,
            net_policy: Arc::new(NetPolicy::None),
            background_tasks: Arc::new(Mutex::new(HashMap::new())),
            task_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Set the network policy applied to spawned commands .
    pub fn with_net_policy(mut self, policy: Arc<NetPolicy>) -> Self {
        self.net_policy = policy;
        self
    }

    /// Fetch the current `(done, output, exit_code)` of a background task.
    /// Returns `None` if the `task_id` is unknown.
    pub fn background_status(&self, task_id: &str) -> Option<(bool, String, Option<i32>)> {
        let tasks = self.background_tasks.lock().unwrap();
        tasks.get(task_id).map(|t| {
            let t = t.lock().unwrap();
            (t.done, t.output.clone(), t.exit_code)
        })
    }

    /// Remove a completed background task from the registry.
    /// Returns `true` if the task was found and removed.
    /// No-op (returns `false`) if the task is still running or unknown.
    pub fn remove_background_task(&self, task_id: &str) -> bool {
        let mut tasks = self.background_tasks.lock().unwrap();
        if let Some(entry) = tasks.get(task_id) {
            if entry.lock().unwrap().done {
                tasks.remove(task_id);
                return true;
            }
        }
        false
    }

    /// Evict oldest completed tasks so that at most `MAX_COMPLETED_TASKS`
    /// completed entries remain. In-flight (not done) tasks are never evicted.
    fn evict_excess_completed(&self) {
        let mut tasks = self.background_tasks.lock().unwrap();
        let completed: Vec<(String, usize)> = tasks
            .iter()
            .filter_map(|(id, entry)| {
                if entry.lock().unwrap().done {
                    let n: usize = id
                        .strip_prefix("task-")
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    Some((id.clone(), n))
                } else {
                    None
                }
            })
            .collect();
        if completed.len() <= MAX_COMPLETED_TASKS {
            return;
        }
        let to_remove = completed.len() - MAX_COMPLETED_TASKS;
        let mut sorted = completed;
        sorted.sort_by_key(|(_, n)| *n);
        for (id, _) in sorted.into_iter().take(to_remove) {
            tasks.remove(&id);
        }
    }

    /// Spawn `spec` in the background and return its `task_id` immediately.
    /// The task runs on a detached tokio task; its result can be polled via
    /// [`BashTool::background_status`].
    async fn spawn_background(&self, spec: BashSpec) -> anyhow::Result<String> {
        let task_id = {
            let mut c = self.task_counter.lock().unwrap();
            *c += 1;
            format!("task-{c}")
        };
        // Evict excess completed tasks before inserting a new one.
        self.evict_excess_completed();
        let entry = Arc::new(Mutex::new(BackgroundTask::default()));
        self.background_tasks
            .lock()
            .unwrap()
            .insert(task_id.clone(), entry.clone());

        let tool = self.clone();
        let command = spec.command.clone();
        let interactive = spec.interactive;
        let timeout = Duration::from_secs(spec.timeout_secs);
        let entry_for_task = entry;

        tokio::spawn(async move {
            let result = if interactive {
                match tokio::task::spawn_blocking(move || run_pty(&command, timeout)).await {
                    Ok(r) => r,
                    Err(e) => Err(anyhow::anyhow!("background pty join error: {e}")),
                }
            } else {
                tool.run_noninteractive(command, timeout).await
            };
            let mut t = entry_for_task.lock().unwrap();
            match result {
                Ok(s) => {
                    t.output = s;
                    t.exit_code = Some(0);
                }
                Err(e) => {
                    t.output = format!("error: {e}");
                    t.exit_code = Some(-1);
                }
            }
            t.done = true;
        });

        Ok(serde_json::json!({ "task_id": task_id }).to_string())
    }

    /// Default non-interactive execution path (`sh -c`, piped stdout/stderr,
    /// optional sandbox + network isolation). Returns the formatted
    /// `stdout:/nstderr:/nexit_code:` string used throughout the codebase.
    async fn run_noninteractive(
        &self,
        command: String,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        // Network namespace isolation : if policy is None and
        // bwrap+userns are available AND the sandbox policy is unrestricted
        // (FullAccess), run the command inside an isolated network namespace
        // (loopback only, no outbound). For restricting sandboxes
        // (WorkspaceWrite/ReadOnly) we deliberately skip bwrap and fall
        // through to the seccomp pre_exec path below — applying the seccomp
        // socket-block to `bwrap` itself breaks bwrap's own setup.
        if matches!(*self.net_policy, NetPolicy::None)
            && matches!(*self.sandbox, SandboxPolicy::FullAccess)
            && bwrap_available()
            && userns_enabled()
        {
            let mut isolated = build_net_command(
                std::process::Command::new("sh").arg("-c").arg(&command),
                &self.net_policy,
                &self.sandbox,
            );
            return match tokio::time::timeout(
                timeout,
                tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
                    let output = isolated
                        .output()
                        .map_err(|e| anyhow::anyhow!("isolated bash failed: {e}"))?;
                    let code = output.status.code().unwrap_or(-1);
                    Ok(format!(
                        "stdout: {}\nstderr: {}\nexit_code: {code}",
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    ))
                }),
            )
            .await
            {
                Ok(inner) => {
                    inner.map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))?
                }
                Err(_) => {
                    let secs = timeout.as_secs();
                    anyhow::bail!("command timed out after {secs}s")
                }
            };
        }

        let mut cmd = build_shell_command(TargetOs::current(), &command);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Apply sandbox in child process via pre_exec (Linux only).
        // Parent (tokio runtime) is never restricted.
        #[cfg(target_os = "linux")]
        {
            if !matches!(*self.sandbox, SandboxPolicy::FullAccess) {
                let hook = zerozero_sandbox::make_pre_exec_hook(self.sandbox.clone());
                unsafe {
                    cmd.pre_exec(hook);
                }
            }
        }

        let mut child = cmd.spawn()?;

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            use tokio::io::AsyncReadExt;
            let _ = tokio::io::BufReader::new(stdout)
                .read_to_end(&mut buf)
                .await;
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            use tokio::io::AsyncReadExt;
            let _ = tokio::io::BufReader::new(stderr)
                .read_to_end(&mut buf)
                .await;
            buf
        });

        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => {
                let status = status?;
                let stdout_bytes = stdout_task.await.unwrap_or_default();
                let stderr_bytes = stderr_task.await.unwrap_or_default();
                let stdout = String::from_utf8_lossy(&stdout_bytes);
                let stderr = String::from_utf8_lossy(&stderr_bytes);
                let code = status.code().unwrap_or(-1);
                Ok(format!(
                    "stdout: {stdout}\nstderr: {stderr}\nexit_code: {code}"
                ))
            }
            Err(_) => {
                let secs = timeout.as_secs();
                let _ = child.kill().await;
                anyhow::bail!("command timed out after {secs}s");
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command and return stdout, stderr, and exit code. \
         Commands have a 30-second timeout by default. Set `interactive: true` \
         to run inside a PTY (for TTY-aware programs), `run_in_background: true` \
         to spawn and get a task_id, and `timeout_secs` to override the timeout."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "interactive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Run the command inside a PTY (interactive TTY). \
                        Use for TTY-aware programs (editors, pagers, prompts). \
                        Default false (non-interactive sh -c)."
                },
                "run_in_background": {
                    "type": "boolean",
                    "default": false,
                    "description": "Spawn the command in the background and return a \
                        task_id immediately. Poll status with `background_status`. \
                        Default false."
                },
                "timeout_secs": {
                    "type": "integer",
                    "default": 30,
                    "description": "Timeout in seconds for the command. Overrides the \
                        default 30s. Applies to foreground and background runs."
                }
            },
            "required": ["command"]
        })
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Exec
    }

    fn when_to_use(&self) -> Option<&str> {
        Some("you need to run a shell command (build, test, git, scripts, process control)")
    }

    fn when_not_to_use(&self) -> Option<&str> {
        Some(
            "you only need to read a file (use `read_file`), search text (use \
             `grep`), or find files by name (use `glob`) — those are safer \
             and faster than shelling out",
        )
    }

    fn examples(&self) -> Vec<String> {
        vec![
            r#"{"command": "cargo build"}"#.to_string(),
            r#"{"command": "cargo test --workspace", "timeout_secs": 120}"#.to_string(),
            r#"{"command": "npm run dev", "run_in_background": true}"#.to_string(),
        ]
    }

    fn error_hints(&self) -> Vec<&str> {
        vec![
            "destructive commands (rm -rf, drop table, force push) require approval — don't try to bypass",
            "if a command hangs, increase timeout_secs or use run_in_background",
            "combine commands with && or ; instead of making multiple calls when they must run in order",
        ]
    }

    async fn execute(&self, args: &serde_json::Value) -> anyhow::Result<String> {
        let spec = BashSpec::from_args(args)?;

        if spec.run_in_background {
            return self.spawn_background(spec).await;
        }

        if spec.interactive {
            let command = spec.command.clone();
            let timeout = Duration::from_secs(spec.timeout_secs);
            return tokio::task::spawn_blocking(move || run_pty(&command, timeout))
                .await
                .map_err(|e| anyhow::anyhow!("pty join error: {e}"))?;
        }

        // Non-interactive (preserves historical behavior exactly).
        self.run_noninteractive(spec.command, Duration::from_secs(spec.timeout_secs))
            .await
    }
}

/// Run a command interactively inside a pseudo-terminal, capturing all
/// output (stdout+stderr merged on the pty) until the child exits or the
/// timeout elapses. Used for `interactive: true` bash invocations to support
/// TTY-aware programs.
///
/// `openpty` creates a *virtual* pty pair — no real TTY/terminal is required,
/// so this is safe to call in CI / headless environments. Sandbox + network
/// isolation are intentionally NOT applied here (PTY mode is foreground
/// interactive; isolation is a non-interactive concern — see).
///
/// This is a free function (no `self`) so it can be unit-tested directly.
fn run_pty(command: &str, timeout: Duration) -> anyhow::Result<String> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Read;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| anyhow::anyhow!("openpty failed: {e}"))?;

    let mut cmd = CommandBuilder::new("sh");
    cmd.arg("-c");
    cmd.arg(command);

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| anyhow::anyhow!("spawn via pty failed: {e}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| anyhow::anyhow!("clone pty reader failed: {e}"))?;

    // A dedicated reader thread drains the pty until EOF (child + slave
    // closed). The watchdog below enforces the timeout by killing the child,
    // which closes the slave and unblocks the reader.
    let output = Arc::new(Mutex::new(Vec::<u8>::new()));
    let reader_out = output.clone();
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => reader_out.lock().unwrap().extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader_thread.join();
                    anyhow::bail!("command timed out after {}s", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                let _ = reader_thread.join();
                anyhow::bail!("wait failed: {e}");
            }
        }
    }
    let _ = reader_thread.join();
    let _ = child.wait();

    let bytes = output.lock().unwrap();
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_has_interactive_and_timeout_fields() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let schema = tool.parameters_schema();
        let props = &schema["properties"];
        assert!(
            props.get("interactive").is_some(),
            "missing interactive field"
        );
        assert_eq!(props["interactive"]["type"], "boolean");
        assert!(
            props.get("run_in_background").is_some(),
            "missing run_in_background field"
        );
        assert_eq!(props["run_in_background"]["type"], "boolean");
        assert!(
            props.get("timeout_secs").is_some(),
            "missing timeout_secs field"
        );
        assert_eq!(props["timeout_secs"]["type"], "integer");
        // command remains required
        assert_eq!(schema["required"], serde_json::json!(["command"]));
    }

    #[test]
    fn test_spec_from_args_defaults() {
        let spec = BashSpec::from_args(&serde_json::json!({"command": "echo hi"})).unwrap();
        assert_eq!(spec.command, "echo hi");
        assert!(!spec.interactive);
        assert!(!spec.run_in_background);
        assert_eq!(spec.timeout_secs, TIMEOUT_SECS);
    }

    #[test]
    fn test_spec_from_args_overrides() {
        let spec = BashSpec::from_args(&serde_json::json!({
            "command": "ls",
            "interactive": true,
            "run_in_background": true,
            "timeout_secs": 7
        }))
        .unwrap();
        assert_eq!(spec.command, "ls");
        assert!(spec.interactive);
        assert!(spec.run_in_background);
        assert_eq!(spec.timeout_secs, 7);
    }

    #[test]
    fn test_spec_from_args_missing_command_errors() {
        assert!(BashSpec::from_args(&serde_json::json!({"interactive": true})).is_err());
    }

    #[tokio::test]
    async fn test_bash_echo_full_access() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"command": "echo hello"});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("hello"));
        assert!(result.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn test_bash_exit_code_full_access() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"command": "exit 42"});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("exit_code: 42"));
    }

    #[tokio::test]
    async fn test_bash_stderr_full_access() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"command": "echo error_msg >&2"});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("error_msg"));
    }

    #[tokio::test]
    async fn test_bash_workspace_write_echo() {
        // Echo should work even under sandbox — no fs write, no network.
        let dir = std::env::temp_dir();
        let tool = BashTool::new(Arc::new(SandboxPolicy::WorkspaceWrite {
            workspace_dir: dir,
        }));
        let args = serde_json::json!({"command": "echo sandboxed_ok"});
        let result = tool.execute(&args).await.unwrap();
        assert!(result.contains("sandboxed_ok"));
        assert!(result.contains("exit_code: 0"));
    }

    #[tokio::test]
    async fn test_bash_noninteractive_timeout_override() {
        // Non-interactive path still honors a custom timeout_secs (short).
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"command": "sleep 30", "timeout_secs": 1});
        let result = tool.execute(&args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn test_run_pty_echo() {
        // openpty creates a virtual pty — no real TTY needed, safe in CI.
        let out =
            tokio::task::spawn_blocking(|| run_pty("echo pty_hello_112", Duration::from_secs(5)))
                .await
                .unwrap()
                .unwrap();
        assert!(out.contains("pty_hello_112"));
    }

    #[tokio::test]
    async fn test_run_pty_timeout() {
        let res = tokio::task::spawn_blocking(|| run_pty("sleep 30", Duration::from_secs(1)))
            .await
            .unwrap();
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("timed out"));
    }

    // --- cross-platform shell command construction ---

    #[test]
    fn test_build_shell_command_unix_uses_sh_c() {
        let cmd = build_shell_command(TargetOs::Unix, "echo hi");
        assert_eq!(cmd.as_std().get_program(), "sh");
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        assert_eq!(
            args,
            vec![std::ffi::OsStr::new("-c"), std::ffi::OsStr::new("echo hi")]
        );
    }

    #[test]
    fn test_build_shell_command_windows_uses_cmd_c() {
        let cmd = build_shell_command(TargetOs::Windows, "dir");
        assert_eq!(cmd.as_std().get_program(), "cmd.exe");
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        assert_eq!(
            args,
            vec![std::ffi::OsStr::new("/c"), std::ffi::OsStr::new("dir")]
        );
    }

    #[test]
    fn test_target_os_current_matches_build() {
        // Whatever OS we compile on, build_shell_command(current()) must use
        // a real shell binary for that platform.
        let cmd = build_shell_command(TargetOs::current(), "true");
        let prog = cmd.as_std().get_program();
        if cfg!(target_os = "windows") {
            assert_eq!(prog, "cmd.exe");
        } else {
            assert_eq!(prog, "sh");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_background_task_runs_and_completes() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        let args = serde_json::json!({"command": "echo bg_ok_112", "run_in_background": true});
        let res = tool.execute(&args).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&res).unwrap();
        let task_id = v["task_id"].as_str().unwrap().to_string();
        assert!(task_id.starts_with("task-"));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some((done, _, _)) = tool.background_status(&task_id) {
                if done {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let (done, out, code) = tool.background_status(&task_id).unwrap();
        assert!(done, "background task should have completed");
        assert!(out.contains("bg_ok_112"));
        assert_eq!(code, Some(0));
    }

    #[test]
    fn test_remove_background_task_completed() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        // Manually insert a completed task.
        {
            let mut tasks = tool.background_tasks.lock().unwrap();
            tasks.insert(
                "task-1".to_string(),
                Arc::new(Mutex::new(BackgroundTask {
                    output: "done".to_string(),
                    exit_code: Some(0),
                    done: true,
                })),
            );
        }
        assert!(tool.remove_background_task("task-1"));
        assert!(tool.background_status("task-1").is_none());
    }

    #[test]
    fn test_remove_background_task_still_running() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        {
            let mut tasks = tool.background_tasks.lock().unwrap();
            tasks.insert(
                "task-1".to_string(),
                Arc::new(Mutex::new(BackgroundTask {
                    output: String::new(),
                    exit_code: None,
                    done: false,
                })),
            );
        }
        // Should not remove a running task.
        assert!(!tool.remove_background_task("task-1"));
        assert!(tool.background_status("task-1").is_some());
    }

    #[test]
    fn test_evict_excess_completed() {
        let tool = BashTool::new(Arc::new(SandboxPolicy::FullAccess));
        // Insert MAX_COMPLETED_TASKS + 5 completed tasks.
        {
            let mut tasks = tool.background_tasks.lock().unwrap();
            for i in 1..=(MAX_COMPLETED_TASKS + 5) {
                tasks.insert(
                    format!("task-{i}"),
                    Arc::new(Mutex::new(BackgroundTask {
                        output: format!("out-{i}"),
                        exit_code: Some(0),
                        done: true,
                    })),
                );
            }
            // Also insert one running task — must NOT be evicted.
            tasks.insert(
                "task-999".to_string(),
                Arc::new(Mutex::new(BackgroundTask {
                    output: String::new(),
                    exit_code: None,
                    done: false,
                })),
            );
        }
        tool.evict_excess_completed();
        let tasks = tool.background_tasks.lock().unwrap();
        // Running task must still be present.
        assert!(tasks.contains_key("task-999"));
        // Completed tasks should be capped at MAX_COMPLETED_TASKS.
        let completed_count = tasks.iter().filter(|(_, e)| e.lock().unwrap().done).count();
        assert_eq!(completed_count, MAX_COMPLETED_TASKS);
        // Oldest tasks (task-1..task-5) should have been evicted.
        for i in 1..=5 {
            assert!(!tasks.contains_key(&format!("task-{i}")));
        }
        // Newer tasks should still be present.
        assert!(tasks.contains_key(&format!("task-{}", MAX_COMPLETED_TASKS + 5)));
    }
}
