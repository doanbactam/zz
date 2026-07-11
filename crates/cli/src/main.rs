use clap::{Parser, Subcommand};
use std::sync::Arc;
use zerozero_compaction::CompactionConfig;
use zerozero_core::{PermissionRule, ZeroZeroConfig};
use zerozero_llm::{
    AnthropicProvider, AuthStore, Effort, GeminiProvider, KeySource, OpenAIProvider, Provider,
    ProviderKind, auth_path, has_api_key, key_source, provider_ids, provider_spec,
    resolve_api_key_for_spec, resolve_base_url, resolve_model, resolve_provider_id,
};
use zerozero_sandbox::{ApprovalPolicy, NetPolicy, SandboxPolicy};
use zerozero_session::SessionStore;
use zerozero_tools::ToolRegistry;

mod jobs;
mod remote;

/// Build the argv for the detached background child: everything after the
/// program name, with `--background` stripped so the child runs the
/// foreground path (and flips the job via `ZZ_BACKGROUND_JOB_ID`). Pure so it
/// can be unit-tested without spawning a process.
fn background_child_args() -> Vec<String> {
    std::env::args()
        .skip(1)
        .filter(|a| a != "--background")
        .collect()
}

/// Flip a background job's status/result in the durable store .
/// Called by a `zz exec` process that was launched in the background (it
/// carries `ZZ_BACKGROUND_JOB_ID`). Best-effort: errors are swallowed so a
/// failing job update never masks the real run outcome.
fn update_job_result(job_id: &str, outcome: anyhow::Result<()>) {
    let store = jobs::JobStore::new(jobs::JobStore::default_path());
    let _ = store.load();
    let mut job = store.get(job_id).unwrap_or_else(|| jobs::Job {
        id: job_id.to_string(),
        prompt: String::new(),
        created_at: String::new(),
        status: jobs::JobStatus::Running,
        result: None,
        session_id: None,
    });
    match outcome {
        Ok(()) => {
            job.status = jobs::JobStatus::Done;
            job.result = Some("completed".to_string());
        }
        Err(e) => {
            job.status = jobs::JobStatus::Failed;
            job.result = Some(format!("error: {e}"));
        }
    }
    let _ = store.upsert(job);
}

#[derive(Parser)]
#[command(name = "zz", version, about = "ZeroZero — CLI Coding Agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run in headless mode: emit JSON lines events for CI/scripting.
    Exec(Box<ExecArgs>),
    /// List saved sessions.
    Sessions(SessionsArgs),
    /// Show or export a single session's transcript parity with
    /// Codex `session show` / `history`).
    Session(SessionArgs),
    /// Run multiple agents in parallel.
    Multi(MultiArgs),
    /// Run eval suite (structure validation or agent mode).
    Eval(EvalArgs),
    /// Rewind a session to a previous checkpoint .
    Rewind(RewindArgs),
    /// Resume a previous session (list, or reopen the latest / a specific id)..
    Resume(ResumeArgs),
    /// Print version and build info.
    Version,
    /// Print diagnostics: provider/model/key status, config, sandbox, tools
    /// parity with Codex `doctor`).
    Doctor,
    /// Show or modify the central config file (`~/.config/zerozero/config.toml`).
    Config(Box<ConfigArgs>),
    /// MCP server mode — expose ZeroZero's tools to external MCP clients
    /// over JSON-RPC 2.0 (stdio by default, optional TCP port).
    #[command(subcommand)]
    Mcp(McpSubcommand),
    /// Run the agent engine as a long-lived service and accept a remote TUI
    /// client over a line-delimited JSON protocol .
    Serve(ServeCliArgs),
    /// Connect a provider API key, or a remote TUI client to `zz serve`.
    ///
    /// - `zz connect` / `zz connect xai` — save an API key (OpenCode-style)
    /// - `zz connect 127.0.0.1:7712` — remote TUI client
    Connect(ConnectArgs),
    /// List and inspect background jobs started with `zz exec --background`
    /// parity with Codex "Background mode").
    Jobs(JobsArgs),
    /// Initialize ZeroZero project config (creates .zerozero/skills, commands, plugins, .env.example).
    Init,
    /// Manage provider API keys (OpenCode-style auth store — parity with `/connect`).
    ///
    /// Keys are stored in `~/.config/zerozero/auth.json` (override: `ZZ_AUTH_PATH`).
    /// Environment variables always win over the store.
    Auth(AuthArgs),
}

/// `zz auth` — list / login / logout provider credentials.
#[derive(clap::Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: Option<AuthCommand>,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// List providers and whether a key is available (env or auth store).
    List,
    /// Save an API key for a provider (OpenCode `/connect` equivalent).
    Login {
        /// Provider id: xai, openai, anthropic, gemini, openrouter, groq, …
        provider: String,
        /// API key. If omitted, read from stdin (one line) or prompt.
        #[arg(long = "key", short = 'k')]
        key: Option<String>,
    },
    /// Remove a stored API key for a provider.
    Logout {
        /// Provider id to remove from the auth store.
        provider: String,
    },
    /// Alias for `login` (non-interactive friendly name).
    Set {
        provider: String,
        /// API key value.
        key: String,
    },
    /// Print the auth store path.
    Path,
}

#[derive(clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Show the resolved configuration (config file merged with env/CLI).
    Show,
    /// Activate a named profile and persist it as the active profile.
    Use {
        /// Profile name to activate.
        name: String,
    },
    /// Enable, disable, or list persistent feature flags in the config file.
    Feature {
        /// Feature flag name (for `enable`/`disable`).
        name: Option<String>,
        /// Action: `enable`/`disable`/`list`.
        action: Option<String>,
    },
    /// List, add, or remove permission rules in the config file
    /// (Codex-style `permissions` allow/deny list).
    Permissions {
        /// Action: `list`/`add`/`remove`.
        action: Option<String>,
        /// Rule string (for `add`/`remove`), e.g. `Deny(Bash(rm -rf *))`.
        name: Option<String>,
    },
    /// Persist a top-level setting (model/provider/approval) to the user
    /// config file parity with Codex `configure`).
    Set {
        /// Setting key: `model` | `provider` | `approval`.
        key: String,
        /// New value to persist.
        value: String,
    },
}

/// `zz sessions` — list saved sessions adds `--json` for
/// machine-readable output, parity with Codex session listing).
#[derive(clap::Args)]
struct SessionsArgs {
    /// Emit a JSON array instead of a human table.
    #[arg(long)]
    json: bool,
}

/// `zz session` — inspect a single saved session .
#[derive(clap::Args)]
struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

/// Subcommands of `zz session`.
#[derive(Subcommand)]
enum SessionCommand {
    /// Print a session's full transcript as readable markdown.
    Show {
        /// Session ID to render.
        id: String,
    },
    /// Print a session's raw transcript as JSON.
    Export {
        /// Session ID to export.
        id: String,
    },
    /// Delete a single saved session by id parity with
    /// Codex `session delete`).
    Delete {
        /// Session ID to delete.
        id: String,
    },
    /// Delete all saved sessions . Use with care.
    Prune,
}

#[derive(clap::Args)]
struct ExecArgs {
    /// Initial instructions. If omitted or "-", read from stdin.
    #[arg(value_name = "PROMPT")]
    prompt: Option<String>,
    /// Pretty-print JSON output (each event on multiple lines).
    /// Kept for backward-compat; prefer `--output json-pretty`.
    #[arg(long)]
    json_pretty: bool,
    /// Output style for headless mode: `jsonl` (default, one compact JSON
    /// object per line), `json-pretty` (pretty-printed JSON), `snippet`
    /// (final answer text only), or `parser` (machine-parseable EVENT lines).
    #[arg(long = "output", value_name = "STYLE", default_value = "jsonl")]
    style: String,
    /// Max agent turns (overrides ZZ_MAX_TURNS env var).
    #[arg(long, value_name = "N")]
    max_turns: Option<u32>,
    /// Sandbox mode: workspace-write, read-only, full-access (overrides ZZ_SANDBOX).
    #[arg(long, value_name = "MODE")]
    sandbox: Option<String>,
    /// Approval policy: on-request, never, untrusted (overrides ZZ_APPROVAL).
    #[arg(long, value_name = "POLICY")]
    approval: Option<String>,
    /// Model name (overrides ZZ_MODEL env var).
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// LLM provider: xai, openai, anthropic, gemini, openrouter, groq, deepseek, …
    /// (overrides ZZ_PROVIDER). See `zz auth list`.
    #[arg(long, value_name = "PROVIDER")]
    provider: Option<String>,
    /// Custom system prompt to inject (in addition to skills).
    #[arg(long, value_name = "TEXT")]
    system_prompt: Option<String>,
    /// Load a specific skill by name (instead of all skills).
    /// Can be specified multiple times.
    #[arg(long, value_name = "NAME")]
    skill: Vec<String>,
    /// Include file contents in the prompt context (Codex `--include`).
    /// May be passed multiple times; each file's contents are appended
    /// under an "Included files" section.
    #[arg(long, value_name = "FILE")]
    include: Vec<String>,
    /// Skip loading skills entirely.
    #[arg(long)]
    no_skills: bool,
    /// Quiet mode — only output final agent message and errors.
    #[arg(long, short = 'q')]
    quiet: bool,
    /// Write the rendered output to a file (in addition to stdout).
    #[arg(long = "tee-file", value_name = "FILE")]
    output_file: Option<String>,
    /// Dry run — show configuration without calling the LLM API.
    #[arg(long)]
    dry_run: bool,
    /// Continue a previous session by ID.
    #[arg(long = "continue", value_name = "SESSION_ID")]
    continue_session: Option<String>,
    /// Branch from a previous session — creates a new session with prior context.
    #[arg(long, value_name = "SESSION_ID")]
    branch: Option<String>,
    /// Plan mode — agent explores read-only, mutating tools are blocked (also ZZ_PLAN=1).
    #[arg(long)]
    plan: bool,
    /// Reasoning effort level: none, low, medium, high (overrides ZZ_EFFORT).
    #[arg(long, value_name = "LEVEL")]
    effort: Option<String>,
    /// Network policy for the bash tool: `none` (default, isolated network
    /// namespace — no outbound) or comma-separated allowlist of domains.
    #[arg(long, value_name = "POLICY", default_value = "none")]
    allow_net: String,
    /// Ask mode: prompt for confirmation before EVERY tool call (parity
    /// `--ask`). Agent proposes actions, user approves each (y/n).
    #[arg(long)]
    ask: bool,
    /// Use a named profile from `config.toml` (overrides base config settings).
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
    /// Run in the background: return a job id immediately and detach the
    /// agent; inspect with `zz jobs` / `zz jobs show <id>` .
    #[arg(long)]
    background: bool,
}

#[derive(clap::Args)]
struct MultiArgs {
    /// Comma-separated prompts for parallel agents.
    #[arg(value_name = "PROMPTS")]
    prompts: String,
}

#[derive(clap::Args)]
struct EvalArgs {
    /// Run zz exec on each task (requires API key). If omitted, only
    /// validate eval structure.
    #[arg(long)]
    agent: bool,
    /// Run a specific task by name. If omitted, run all tasks.
    #[arg(value_name = "TASK")]
    task: Option<String>,
}

#[derive(clap::Args)]
struct RewindArgs {
    /// Session ID to rewind.
    session_id: String,
    /// Rewind to this checkpoint seq (must be a user message).
    /// If omitted, list available checkpoints.
    #[arg(long, value_name = "SEQ")]
    to: Option<i64>,
}

/// `zz resume` arguments F14).
#[derive(clap::Args)]
struct ResumeArgs {
    /// Resume the most recent session.
    #[arg(long)]
    last: bool,
    /// Resume a specific session by id.
    #[arg(long, value_name = "ID")]
    id: Option<String>,
    /// Print the session's plan + approvals, then exit (no TUI).
    #[arg(long)]
    inspect: bool,
}

/// `zz mcp` subcommands.
#[derive(Subcommand)]
enum McpSubcommand {
    /// Serve ZeroZero's tools over JSON-RPC 2.0 (stdio by default, or a
    /// TCP port via `--port`).
    Serve(ServeArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Optional TCP port to listen on instead of stdio (localhost only,
    /// single client). When omitted, stdio transport is used.
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,
}

/// `zz serve` arguments  — app-server mode.
#[derive(clap::Args)]
struct ServeCliArgs {
    /// TCP port to listen on for a single remote client (default 7712).
    #[arg(long, value_name = "PORT", default_value_t = 7712)]
    port: u16,
    /// Remote approval policy : `on-ask` prompts for every tool call;
    /// `auto-edit` only prompts for destructive commands (auto-approves edits).
    #[arg(long, value_name = "MODE", default_value = "on-ask")]
    approval: ServeApproval,
}

/// Remote approval mode for `zz serve` .
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum ServeApproval {
    OnAsk,
    AutoEdit,
}

impl From<ServeApproval> for zerozero_sandbox::ApprovalPolicy {
    fn from(m: ServeApproval) -> Self {
        match m {
            ServeApproval::OnAsk => Self::OnAsk,
            ServeApproval::AutoEdit => Self::AutoEdit,
        }
    }
}

/// `zz connect` arguments.
///
/// Dual-purpose (OpenCode parity + remote client):
/// - no arg / provider id → API key login (`zz auth login`)
/// - `host:port` → remote TUI client to `zz serve`
#[derive(clap::Args)]
struct ConnectArgs {
    /// Provider id (`xai`, `openai`, …) **or** remote address (`127.0.0.1:7712`).
    /// Omit to log in the default provider interactively.
    #[arg(value_name = "PROVIDER_OR_ADDR")]
    target: Option<String>,
    /// API key when connecting a provider (same as `zz auth login -k`).
    #[arg(long = "key", short = 'k')]
    key: Option<String>,
}

/// `zz jobs` arguments . `zz jobs` lists; `zz jobs show <id>`
/// shows a single job.
#[derive(clap::Args)]
struct JobsArgs {
    #[command(subcommand)]
    cmd: Option<JobsCmd>,
}

#[derive(clap::Subcommand)]
enum JobsCmd {
    /// Show a single background job by id.
    Show {
        /// Job id (e.g. `job-19f45f99763`).
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Print the captured stdout/stderr of a background job.
    Log {
        /// Job id (e.g. `job-19f45f99763`).
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Remove all jobs and their logs (cannot be undone).
    Clear,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present (ignored if not found). Secrets in .env
    // are excluded from git via .gitignore.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    match cli.command {
        None => {
            // Default: open the TUI (async streaming).
            run_tui().await?;
        }
        Some(Command::Exec(args)) => {
            let prompt = read_prompt(args.prompt)?;
            let prompt = apply_include_files(prompt, &args.include)?;
            if args.background {
                // true async detach. Re-exec `zz` as a detached child
                // process running the SAME prompt in foreground (so all
                // provider/session/sandbox logic is unchanged). The parent
                // records a Running job, hands the child its id via the
                // ZZ_BACKGROUND_JOB_ID env var, redirects the child's output to
                // a per-job log, prints the job id, and returns immediately.
                // The child flips the job to Done/Failed on completion (see the
                // normal Exec path below). No `tokio::spawn` -> no `!Send`
                // SessionStore issue.
                let job_id = jobs::JobStore::new_id();
                let store = jobs::JobStore::new(jobs::JobStore::default_path());
                let _ = store.load();
                let job = jobs::Job {
                    id: job_id.clone(),
                    prompt: prompt.clone(),
                    created_at: jobs::chrono_like_now(),
                    status: jobs::JobStatus::Running,
                    result: None,
                    session_id: args.continue_session.clone(),
                };
                let _ = store.upsert(job);

                // Reconstruct argv minus `--background` so the child runs the
                // foreground path (and thus flips the job via the env var).
                let exe =
                    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("zz"));
                let child_args = background_child_args();
                let log_path = jobs::JobStore::log_path(&job_id);
                if let Some(parent) = log_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let log_file = std::fs::File::create(&log_path)
                    .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap());
                match std::process::Command::new(exe)
                    .args(&child_args)
                    .env("ZZ_BACKGROUND_JOB_ID", &job_id)
                    .stdout(
                        log_file
                            .try_clone()
                            .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap()),
                    )
                    .stderr(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&log_path)
                            .unwrap_or_else(|_| std::fs::File::open("/dev/null").unwrap()),
                    )
                    .spawn()
                {
                    Ok(_child) => {
                        println!("background job started: {job_id}");
                        println!("  log: {}", log_path.display());
                    }
                    Err(e) => {
                        // If we can't detach, fall back to running in foreground
                        // and still record the result.
                        eprintln!(
                            "warning: failed to spawn background job ({e}); running in foreground"
                        );
                        let outcome = run_exec(
                            prompt,
                            args.json_pretty,
                            &args.style,
                            args.max_turns,
                            args.sandbox,
                            args.approval,
                            args.model,
                            args.provider,
                            args.system_prompt,
                            args.skill,
                            args.no_skills,
                            args.quiet,
                            args.output_file,
                            args.dry_run,
                            args.continue_session,
                            args.branch,
                            args.plan,
                            args.effort,
                            Some(args.allow_net),
                            args.ask,
                            args.profile.as_deref(),
                        )
                        .await;
                        update_job_result(&job_id, outcome);
                    }
                }
            } else {
                let outcome = run_exec(
                    prompt,
                    args.json_pretty,
                    &args.style,
                    args.max_turns,
                    args.sandbox,
                    args.approval,
                    args.model,
                    args.provider,
                    args.system_prompt,
                    args.skill,
                    args.no_skills,
                    args.quiet,
                    args.output_file,
                    args.dry_run,
                    args.continue_session,
                    args.branch,
                    args.plan,
                    args.effort,
                    Some(args.allow_net),
                    args.ask,
                    args.profile.as_deref(),
                )
                .await;
                // if this process is a background child, flip the job.
                if let Ok(job_id) = std::env::var("ZZ_BACKGROUND_JOB_ID") {
                    update_job_result(&job_id, outcome);
                } else {
                    outcome?;
                }
            }
        }
        Some(Command::Jobs(args)) => {
            let store = jobs::JobStore::new(jobs::JobStore::default_path());
            let _ = store.load();
            match args.cmd {
                Some(JobsCmd::Show { id }) => {
                    let job = store
                        .get(&id)
                        .ok_or_else(|| anyhow::anyhow!("no job with id {id}"))?;
                    println!("{}", serde_json::to_string_pretty(&job)?);
                }
                Some(JobsCmd::Log { id }) => match store.read_log(&id) {
                    Ok(contents) => print!("{contents}"),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        anyhow::bail!("no log for job {id}");
                    }
                    Err(e) => return Err(e.into()),
                },
                Some(JobsCmd::Clear) => {
                    store.clear()?;
                    println!("cleared all background jobs");
                }
                None => {
                    let jobs = store.list();
                    if jobs.is_empty() {
                        println!("no background jobs");
                    } else {
                        for job in jobs {
                            println!(
                                "{}  {}  {}",
                                job.id,
                                job.status.as_str(),
                                job.prompt.chars().take(60).collect::<String>()
                            );
                        }
                    }
                }
            }
        }
        Some(Command::Sessions(args)) => {
            run_sessions_list(args.json)?;
        }
        Some(Command::Multi(args)) => {
            run_multi(args.prompts).await?;
        }
        Some(Command::Eval(args)) => {
            run_eval(args.agent, args.task)?;
        }
        Some(Command::Session(args)) => {
            run_session_command(args.command).await?;
        }
        Some(Command::Rewind(args)) => {
            run_rewind(args.session_id, args.to)?;
        }
        Some(Command::Resume(args)) => {
            run_resume(args.last, args.id, args.inspect)?;
        }
        Some(Command::Version) => {
            run_version();
        }
        Some(Command::Doctor) => {
            run_doctor();
        }
        Some(Command::Config(args)) => {
            run_config_command(args.command).await?;
        }
        Some(Command::Init) => {
            run_init()?;
        }
        Some(Command::Auth(args)) => {
            run_auth(args)?;
        }
        Some(Command::Mcp(args)) => match args {
            McpSubcommand::Serve(serve_args) => {
                run_mcp_serve(serve_args.port).await?;
            }
        },
        Some(Command::Serve(args)) => {
            remote::run_serve(args.port, args.approval.into()).await?;
        }
        Some(Command::Connect(args)) => {
            run_connect_command(args).await?;
        }
    }
    Ok(())
}

/// Initialize ZeroZero project configuration.
/// Creates:
///   .zerozero/skills/      — project skills (SKILL.md packages → `/name` slash)
///   .zerozero/commands/    — flat markdown slash commands (Grok/Claude layout)
///   .zerozero/plugins.toml — plugins config
///   .env.example           — example env file (if not exists)
fn run_init() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;

    // Create .zerozero/ root + skills + commands (Grok-style slash sources).
    let zz_root = cwd.join(".zerozero");
    let skills_dir = zz_root.join("skills");
    let commands_dir = zz_root.join("commands");
    for dir in [&zz_root, &skills_dir, &commands_dir] {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            println!("Created: {}", dir.display());
        } else {
            println!("Exists:  {}", dir.display());
        }
    }

    // Create .zerozero/skills/example/SKILL.md
    let example_skill_dir = skills_dir.join("example");
    if !example_skill_dir.exists() {
        std::fs::create_dir_all(&example_skill_dir)?;
        let skill_content = "---\nname: example\ndescription: An example skill\nkeywords: example, demo\nuser-invocable: true\nargument-hint: task description\n---\n# Example Skill\n\nThis is an example skill. Invoke with `/example <task>` in the TUI.\nEdit or replace it with your own.\n";
        std::fs::write(example_skill_dir.join("SKILL.md"), skill_content)?;
        println!("Created: {}", example_skill_dir.join("SKILL.md").display());
    }

    // Create .zerozero/commands/hello.md — flat slash command sample
    let hello_cmd = commands_dir.join("hello.md");
    if !hello_cmd.exists() {
        let content = "---\nname: hello\ndescription: Sample flat slash command\nargument-hint: name\n---\n# Hello command\n\nGreet the user. This file lives under `.zerozero/commands/` and appears as `/hello`.\n";
        std::fs::write(&hello_cmd, content)?;
        println!("Created: {}", hello_cmd.display());
    }

    // Create .zerozero/plugins.toml (with example commented out)
    let plugins_file = zz_root.join("plugins.toml");
    if !plugins_file.exists() {
        let content = "# ZeroZero plugins config\n# Add plugins below:\n\n# [[plugins]]\n# name = \"my-tool\"\n# description = \"My custom tool\"\n# command = \"python3\"\n# args = [\"my_tool.py\"]\n";
        std::fs::write(&plugins_file, content)?;
        println!("Created: {}", plugins_file.display());
    } else {
        println!("Exists:  {}", plugins_file.display());
    }

    // Create .env.example (if not exists)
    let env_example = cwd.join(".env.example");
    if !env_example.exists() {
        let content = "# ZeroZero — .env example\n# Prefer: zz auth login xai --key xai-...\n# Keys also load from ~/.config/zerozero/auth.json\n\nXAI_API_KEY=xai-...\n# ZZ_PROVIDER=openai\n# OPENAI_API_KEY=sk-...\n# ZZ_PROVIDER=anthropic\n# ANTHROPIC_API_KEY=sk-ant-...\n# ZZ_PROVIDER=gemini\n# GEMINI_API_KEY=...\n# ZZ_PROVIDER=openrouter\n# OPENROUTER_API_KEY=...\n# ZZ_PROVIDER=groq\n# GROQ_API_KEY=...\n# ZZ_PROVIDER=ollama\n# ZZ_MODEL=llama3.2\n";
        std::fs::write(&env_example, content)?;
        println!("Created: {}", env_example.display());
    } else {
        println!("Exists:  {}", env_example.display());
    }

    println!();
    println!("ZeroZero project initialized!");
    println!();
    println!("Next steps:");
    println!("  1. Copy .env.example to .env and add your API key");
    println!("  2. Add skills to .zerozero/skills/<name>/SKILL.md (slash: /name)");
    println!("  3. Add flat commands to .zerozero/commands/<name>.md");
    println!("  4. Add plugins to .zerozero/plugins.toml");
    println!("  5. Run: zz");

    Ok(())
}

/// Run the MCP server mode (`zz mcp serve`).
///
/// Builds the standard tool registry (full-access sandbox, isolated
/// network namespace) and serves it over JSON-RPC 2.0:
/// - stdio transport when `port` is `None` (the supported parity path),
/// - a single localhost TCP connection when `port` is `Some`.
async fn run_mcp_serve(port: Option<u16>) -> anyhow::Result<()> {
    let server = zerozero_tools::standard_server();

    match port {
        None => {
            eprintln!("zz mcp: serving tools over stdio JSON-RPC 2.0");
            server.run().await
        }
        Some(p) => {
            eprintln!("zz mcp: serving tools over TCP 127.0.0.1:{p}");
            server.run_tcp(p).await
        }
    }
}

/// Print version and build information.
fn run_version() {
    let version = env!("CARGO_PKG_VERSION");
    let name = env!("CARGO_PKG_NAME");
    println!("{name} v{version}");
    println!();
    println!("Built with Rust (edition 2024)");
    println!();
    println!("Providers: {}", provider_ids().join(", "));
    println!();
    println!("Commands:");
    println!("  zz                  Start TUI (interactive mode)");
    println!("  zz exec <prompt>    Headless mode (--output jsonl|json-pretty|snippet|parser)");
    println!("  zz multi <prompts>  Run multiple agents in parallel");
    println!("  zz eval             Run eval suite");
    println!("  zz sessions         List saved sessions");
    println!("  zz version          Show version info");
    println!();
    println!("Environment variables:");
    println!(
        "  ZZ_PROVIDER         LLM provider ({})",
        provider_ids().join("|")
    );
    println!("  XAI_API_KEY         xAI API key (default provider)");
    println!("  ZZ_MODEL            Model name override");
    println!(
        "  ZZ_AUTH_PATH        Override auth.json path (default: ~/.config/zerozero/auth.json)"
    );
    println!("  ZZ_SANDBOX          Sandbox mode (workspace-write|read-only|full-access)");
    println!("  ZZ_APPROVAL         Approval policy (on-request|never|untrusted)");
    println!("  ZZ_MAX_TURNS        Max agent turns (default: 10)");
    println!();
    println!("Auth (OpenCode-style):");
    println!("  zz auth list                  Show providers + key status");
    println!("  zz auth login <provider> -k   Save API key to auth.json");
    println!("  zz auth logout <provider>     Remove stored key");
}

/// Print diagnostics: provider/model/key status, config, sandbox, tools
/// parity with Codex `doctor`). Pure local introspection — no
/// network / LLM calls.
fn run_doctor() {
    let version = env!("CARGO_PKG_VERSION");
    println!("zz doctor — ZeroZero v{version} diagnostics\n");

    // Provider + model (resolved from env/config, mirroring build_provider).
    let raw = std::env::var("ZZ_PROVIDER").unwrap_or_default();
    let spec = provider_spec(&raw);
    let provider_label = if raw.trim().is_empty() {
        format!("{} (default)", spec.id)
    } else {
        spec.id.to_string()
    };
    let model = resolve_model(spec, None);
    println!("Provider : {provider_label}");
    println!("Model    : {model}");
    println!("Auth path: {}", auth_path().display());

    // API key presence for the active provider + common ones.
    let src = key_source(spec.id);
    println!(
        "Active key: {} ({})",
        match src {
            KeySource::Missing => "not set",
            _ => "set",
        },
        match src {
            KeySource::Env => "env",
            KeySource::AuthStore => "auth.json",
            KeySource::LegacyFallback => "OPENAI_API_KEY fallback",
            KeySource::NotRequired => "not required",
            KeySource::Missing => "missing",
        }
    );
    print!("API keys:");
    for p in zerozero_llm::PROVIDERS.iter().filter(|p| p.requires_key) {
        let s = key_source(p.id);
        let mark = if matches!(s, KeySource::Missing) {
            "no"
        } else {
            "yes"
        };
        print!(" {}={}", p.id, mark);
    }
    println!();

    // Config file + active profile + features.
    let cfg = ZeroZeroConfig::load();
    let path = ZeroZeroConfig::default_save_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());
    println!("\nConfig   : {path}");
    println!(
        "Profile  : {}",
        cfg.active_profile.as_deref().unwrap_or("(none)")
    );
    let features: Vec<&String> = cfg
        .features
        .iter()
        .filter(|(_, v)| **v)
        .map(|(k, _)| k)
        .collect();
    println!(
        "Features : {}",
        if features.is_empty() {
            "(none enabled)".to_string()
        } else {
            features
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    // Sandbox support (platform + policy).
    let sandbox_supported = cfg!(target_os = "linux");
    println!(
        "\nSandbox  : {} (landlock/seccomp on Linux)",
        if sandbox_supported {
            "supported"
        } else {
            "limited (non-Linux)"
        }
    );

    // Tools.
    let tools = zerozero_tools::ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));
    println!("Tools    : {} registered", tools.definitions().len());

    println!("\nStatus   : OK (all checks are local introspection)");
}

/// Show the resolved configuration (merged config file + env/CLI).
fn run_config_show() {
    let cfg = ZeroZeroConfig::load();
    let resolved = cfg.resolve(None, None, None, None);

    let provider = resolved.provider.unwrap_or_else(|| {
        std::env::var("ZZ_PROVIDER").unwrap_or_else(|_| "xai (default)".to_string())
    });
    let model = resolved.model.unwrap_or_else(|| {
        let p = std::env::var("ZZ_PROVIDER").unwrap_or_default();
        resolve_model(provider_spec(&p), None)
    });
    let approval = resolved.approval.unwrap_or_else(|| {
        std::env::var("ZZ_APPROVAL").unwrap_or_else(|_| "on-request".to_string())
    });

    println!("ZeroZero Configuration (config.toml)");
    println!("=====================================");
    if let Some(active) = &cfg.active_profile {
        println!("Active profile: {}", active);
    } else {
        println!("Active profile: (none)");
    }
    println!();
    println!("Provider:        {provider}");
    println!("Model:           {model}");
    println!("Approval:        {approval}");
    println!();
    println!("Features:");
    if cfg.features.is_empty() {
        println!("  (none)");
    } else {
        for (k, v) in &cfg.features {
            println!("  {k} = {v}");
        }
    }
    println!();
    println!(
        "Profiles: {}",
        cfg.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    println!(
        "Config path:     {}",
        ZeroZeroConfig::default_save_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(unknown)".to_string())
    );
    println!();
    println!("API Keys (env or auth.json):");
    println!("  Auth path: {}", auth_path().display());
    for p in zerozero_llm::PROVIDERS.iter().filter(|p| p.requires_key) {
        let src = key_source(p.id);
        let label = match src {
            KeySource::Env => "set (env)",
            KeySource::AuthStore => "set (auth.json)",
            KeySource::LegacyFallback => "set (legacy)",
            KeySource::NotRequired => "n/a",
            KeySource::Missing => "not set",
        };
        println!("  {:12} {:22} {}", p.id, p.api_key_env, label);
    }
}

/// Dispatch the `zz config` subcommands.
async fn run_config_command(command: ConfigCommand) -> anyhow::Result<()> {
    match command {
        ConfigCommand::Show => {
            run_config_show();
        }
        ConfigCommand::Use { name } => {
            let mut cfg = ZeroZeroConfig::load();
            cfg.use_profile(&name);
            cfg.save()?;
            println!("Activated profile '{name}' (saved to config.toml).");
        }
        ConfigCommand::Feature { name, action } => match action
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "list" => {
                let cfg = ZeroZeroConfig::load();
                println!("{}", zerozero_cli::format_feature_list(&cfg));
            }
            "enable" | "on" | "true" | "1" => {
                let name = name
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("feature enable requires a flag name"))?;
                let msg = zerozero_cli::apply_feature(name, true)?;
                println!("{msg}");
            }
            "disable" | "off" | "false" | "0" => {
                let name = name
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("feature disable requires a flag name"))?;
                let msg = zerozero_cli::apply_feature(name, false)?;
                println!("{msg}");
            }
            other => {
                anyhow::bail!("unknown feature action '{other}' (expected enable|disable|list)")
            }
        },
        ConfigCommand::Permissions { action, name } => match action
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "list" => {
                let cfg = ZeroZeroConfig::load();
                if cfg.permissions.is_empty() {
                    println!("(no permission rules — all tools allowed by default)");
                } else {
                    for rule in &cfg.permissions {
                        println!("{rule}");
                    }
                }
            }
            "add" => {
                let rule = name
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("permissions add requires a rule string"))?;
                if PermissionRule::parse(rule).is_none() {
                    anyhow::bail!("invalid permission rule '{rule}'");
                }
                let mut cfg = ZeroZeroConfig::load();
                if !cfg.permissions.iter().any(|r| r == rule) {
                    cfg.permissions.push(rule.clone());
                    cfg.save()?;
                }
                println!("Added permission rule '{rule}' (saved to config.toml).");
            }
            "remove" | "rm" => {
                let rule = name
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("permissions remove requires a rule string"))?;
                let mut cfg = ZeroZeroConfig::load();
                let before = cfg.permissions.len();
                cfg.permissions.retain(|r| r != rule);
                if cfg.permissions.len() == before {
                    anyhow::bail!("permission rule '{rule}' not found");
                }
                cfg.save()?;
                println!("Removed permission rule '{rule}' (saved to config.toml).");
            }
            other => {
                anyhow::bail!("unknown permissions action '{other}' (expected list|add|remove)")
            }
        },
        ConfigCommand::Set { key, value } => {
            let key_lc = key.to_ascii_lowercase();
            if key_lc != "model" && key_lc != "provider" && key_lc != "approval" {
                anyhow::bail!("unknown config key '{key}' (expected model|provider|approval)");
            }
            let mut cfg = ZeroZeroConfig::load();
            match key_lc.as_str() {
                "model" => cfg.model = Some(value.clone()),
                "provider" => cfg.provider = Some(value.clone()),
                "approval" => cfg.approval = Some(value.clone()),
                _ => unreachable!(),
            }
            cfg.save()?;
            println!("Set {key} = '{value}' (saved to config.toml).");
        }
    }
    Ok(())
}

/// Build LLM provider from env + auth store (OpenCode-style resolution).
///
/// Key priority: env var → `~/.config/zerozero/auth.json` → (xAI only)
/// legacy `OPENAI_API_KEY` fallback.
fn build_provider() -> Box<dyn Provider> {
    build_provider_with_model(None)
}

/// Build LLM provider with an optional model override .
///
/// Uses the built-in provider registry (`zerozero_llm::providers`) so every
/// OpenAI-compatible backend shares one code path.
fn build_provider_with_model(model_override: Option<String>) -> Box<dyn Provider> {
    let provider_type = std::env::var("ZZ_PROVIDER").unwrap_or_default();
    build_provider_with_provider_type(&provider_type, model_override)
}

/// Build a provider for an explicit `provider_type` + optional model override.
///
/// This is the 3-tier-picker variant: the TUI model picker can switch both
/// provider and model mid-session without mutating process-wide env.
fn build_provider_with_provider_type(
    provider_type: &str,
    model_override: Option<String>,
) -> Box<dyn Provider> {
    let spec = provider_spec(provider_type);
    let api_key = resolve_api_key_for_spec(spec);
    let base_url = resolve_base_url(spec);
    let model = resolve_model(spec, model_override);

    match spec.kind {
        ProviderKind::Anthropic => Box::new(AnthropicProvider::new(api_key, base_url, model)),
        ProviderKind::Gemini => Box::new(GeminiProvider::new(api_key, model)),
        ProviderKind::OpenAiCompat | ProviderKind::Local => {
            Box::new(OpenAIProvider::new(api_key, base_url, model))
        }
    }
}

/// Create a provider factory closure for TUI model switching (3-tier picker).
///
/// The returned closure takes `(provider_type, model)` and rebuilds the
/// provider with the given provider id and model name, keeping the same
/// api_key/base_url resolution from environment variables.
fn make_provider_factory() -> Box<dyn Fn(String, String) -> Box<dyn Provider> + Send + Sync> {
    Box::new(move |provider_type: String, model: String| {
        build_provider_with_provider_type(&provider_type, Some(model))
    })
}

/// Load plugins from .zerozero/plugins.json or .zerozero/plugins.toml
/// and register them into the tool registry. Returns the count of
/// plugins registered. Logs a warning if config exists but fails to load.
fn register_external_plugins(registry: &mut zerozero_tools::ToolRegistry) -> (usize, Vec<String>) {
    if let Some(config_path) = zerozero_plugins::discover_config() {
        match zerozero_plugins::load_plugins_config(&config_path) {
            Ok(file) => {
                let names: Vec<String> = file.plugins.iter().map(|p| p.name.clone()).collect();
                let count = zerozero_plugins::register_plugins(registry, &file);
                if count > 0 {
                    eprintln!("Loaded {count} plugin(s) from {}", config_path.display());
                }
                (count, names)
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to load plugins config from {}: {e}",
                    config_path.display()
                );
                (0, Vec::new())
            }
        }
    } else {
        (0, Vec::new())
    }
}

/// Load skills from standard multi-source directories (Grok-style registry)
/// and build a system prompt. Returns (system_prompt, skill_names, skill_dirs).
///
/// See [`zerozero_skills::standard_skill_dirs`] for discovery order.
fn load_skills_system_prompt() -> (Option<String>, Vec<String>, Vec<std::path::PathBuf>) {
    let dirs = zerozero_skills::discover_skill_paths();
    let mut registry = zerozero_skills::SkillRegistry::new();
    let _ = registry.load_standard();
    let names = registry.list();
    if registry.all().is_empty() {
        return (None, names, dirs);
    }
    let mut prompt = String::from("You have access to the following skills:\n\n");
    for skill in registry.all() {
        prompt.push_str(&format!(
            "## Skill: {}\n{}\n\n{}\n\n",
            skill.name, skill.description, skill.content
        ));
    }
    prompt.push_str("Use these skills as guidance when relevant to the task.");
    (Some(prompt), names, dirs)
}

/// Load project-level rules files (AGENTS.md / CLAUDE.md) into a single
/// system-prompt fragment (parity: Codex auto-reads AGENTS.md / CLAUDE.md).
/// Searches, in order: `./AGENTS.md`, `./CLAUDE.md`, then
/// `~/.config/zerozero/AGENTS.md` / `~/.config/zerozero/CLAUDE.md`.
/// Returns `None` when no rules file exists.
fn load_project_rules() -> Option<String> {
    let candidates: Vec<std::path::PathBuf> = {
        let mut c = vec![
            std::path::PathBuf::from("AGENTS.md"),
            std::path::PathBuf::from("CLAUDE.md"),
        ];
        if let Ok(home) = std::env::var("HOME") {
            c.push(std::path::PathBuf::from(format!(
                "{home}/.config/zerozero/AGENTS.md"
            )));
            c.push(std::path::PathBuf::from(format!(
                "{home}/.config/zerozero/CLAUDE.md"
            )));
        }
        c
    };
    let mut blocks: Vec<String> = Vec::new();
    for path in &candidates {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                blocks.push(format!("# Project Rules ({})\n\n{}", path.display(), text));
            }
        }
    }
    if blocks.is_empty() {
        None
    } else {
        Some(blocks.join("\n\n"))
    }
}

/// Run `zz exec` in headless mode: create LLM provider from env, call
/// `core::run_turn`, emit JSONL events on stdout.
#[allow(clippy::too_many_arguments)]
async fn run_exec(
    prompt: String,
    json_pretty: bool,
    output_style: &str,
    max_turns_override: Option<u32>,
    sandbox_override: Option<String>,
    approval_override: Option<String>,
    model_override: Option<String>,
    provider_override: Option<String>,
    system_prompt: Option<String>,
    skill_names: Vec<String>,
    no_skills: bool,
    quiet: bool,
    output_file: Option<String>,
    dry_run: bool,
    continue_session: Option<String>,
    branch: Option<String>,
    plan: bool,
    effort_override: Option<String>,
    allow_net_override: Option<String>,
    ask_mode: bool,
    profile_override: Option<&str>,
) -> anyhow::Result<()> {
    // Load the central config file and apply profile/model/approval/provider
    // from it BEFORE CLI overrides, so that CLI flags still win.
    let cfg = ZeroZeroConfig::load();
    let resolved = cfg.resolve(profile_override, None, None, None);
    if let Some(p) = &resolved.provider {
        unsafe {
            std::env::set_var("ZZ_PROVIDER", p);
        }
    }
    if let Some(m) = &resolved.model {
        unsafe {
            std::env::set_var("ZZ_MODEL", m);
        }
    }
    if let Some(a) = &resolved.approval {
        unsafe {
            std::env::set_var("ZZ_APPROVAL", a);
        }
    }

    // Apply explicit provider override before validate_api_key and build_provider.
    if let Some(provider) = &provider_override {
        // SAFETY: No async tasks have been spawned yet. This is the
        // earliest point in run_exec, before any tokio operations.
        unsafe {
            std::env::set_var("ZZ_PROVIDER", provider);
        }
    }

    let provider_type = std::env::var("ZZ_PROVIDER").unwrap_or_default();
    validate_api_key(&provider_type);

    let max_turns: u32 = max_turns_override
        .or_else(|| {
            std::env::var("ZZ_MAX_TURNS")
                .ok()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(10);

    let sandbox_str =
        sandbox_override.unwrap_or_else(|| std::env::var("ZZ_SANDBOX").unwrap_or_default());
    let sandbox = match sandbox_str.as_str() {
        "read-only" => SandboxPolicy::ReadOnly,
        "full-access" => SandboxPolicy::FullAccess,
        _ => SandboxPolicy::WorkspaceWrite {
            workspace_dir: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
        },
    };

    let approval_str =
        approval_override.unwrap_or_else(|| std::env::var("ZZ_APPROVAL").unwrap_or_default());
    let approval = match approval_str.as_str() {
        "never" => ApprovalPolicy::Never,
        "untrusted" => ApprovalPolicy::Untrusted,
        "auto-edit" => ApprovalPolicy::AutoEdit,
        "on-ask" => ApprovalPolicy::OnAsk,
        _ => ApprovalPolicy::OnRequest,
    };

    let plan_mode = plan
        || matches!(
            std::env::var("ZZ_PLAN").unwrap_or_default().as_str(),
            "1" | "true"
        );

    // Reasoning effort: --effort flag overrides ZZ_EFFORT env. Default None
    // (no reasoning parameter sent — preserves prior behavior).
    let effort: Effort = effort_override
        .or_else(|| std::env::var("ZZ_EFFORT").ok())
        .map(|s| s.parse::<Effort>().map_err(|e| anyhow::anyhow!("{e}")))
        .transpose()?
        .unwrap_or_default();

    if let Some(model) = &model_override {
        // SAFETY: This is the only place we modify env vars, and it's
        // before any async code runs (no tasks have been spawned yet).
        // This allows --model to override ZZ_MODEL for build_provider().
        unsafe {
            std::env::set_var("ZZ_MODEL", model);
        }
    }

    let provider = build_provider();
    let net_policy = Arc::new(NetPolicy::parse(
        &allow_net_override.unwrap_or_else(|| "none".to_string()),
    ));
    let mut tools =
        zerozero_tools::ToolRegistry::standard_with_net(Arc::new(sandbox.clone()), net_policy);
    let _ = register_external_plugins(&mut tools);

    // Session persistence: open SQLite store if ZZ_SESSION_DB is set.
    let session_db = std::env::var("ZZ_SESSION_DB").ok();
    let session_store = match &session_db {
        Some(path) => match SessionStore::open(std::path::Path::new(path)) {
            Ok(store) => Some(store),
            Err(e) => {
                eprintln!("warning: failed to open session DB at {path}: {e}");
                None
            }
        },
        None => None,
    };

    // Compaction config from env vars.
    let compaction_config = CompactionConfig {
        max_messages: std::env::var("ZZ_MAX_MESSAGES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20),
        max_tokens: std::env::var("ZZ_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100_000),
        keep_recent: std::env::var("ZZ_KEEP_RECENT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6),
        token_budget: std::env::var("ZZ_TOKEN_BUDGET")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100_000),
        keep_recent_turns: std::env::var("ZZ_KEEP_RECENT_TURNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6),
    };

    // --continue <session-id>: load prior messages from the session store
    // so run_turn can resume the conversation. If the session doesn't
    // exist (or no session DB is configured), we proceed with an empty
    // history — the turn simply starts fresh.
    //
    // --branch <session-id>: like --continue but creates a NEW session
    // with the prior messages as context (fork, not resume).
    let (continue_session_id, prior_messages): (Option<String>, Vec<zerozero_llm::ChatMessage>) =
        if let Some(ref branch_id) = branch {
            // Branch: load messages from source, create new session ID
            if let Some(ref store) = session_store {
                match store.get_messages(branch_id) {
                    Ok(msgs) => {
                        let new_id = format!(
                            "branch-{}-{}",
                            &branch_id[..8.min(branch_id.len())],
                            chrono_minute_id()
                        );
                        (Some(new_id), msgs)
                    }
                    Err(e) => {
                        eprintln!("warning: failed to load session for branch {branch_id}: {e}");
                        let new_id = format!(
                            "branch-{}-{}",
                            &branch_id[..8.min(branch_id.len())],
                            chrono_minute_id()
                        );
                        (Some(new_id), Vec::new())
                    }
                }
            } else {
                eprintln!("warning: --branch requires ZZ_SESSION_DB to be set; starting fresh");
                let new_id = format!(
                    "branch-{}-{}",
                    &branch_id[..8.min(branch_id.len())],
                    chrono_minute_id()
                );
                (Some(new_id), Vec::new())
            }
        } else if let Some(ref cid) = continue_session {
            if let Some(ref store) = session_store {
                match store.get_messages(cid) {
                    Ok(msgs) => (Some(cid.clone()), msgs),
                    Err(e) => {
                        eprintln!("warning: failed to load session {cid}: {e}");
                        (Some(cid.clone()), Vec::new())
                    }
                }
            } else {
                eprintln!("warning: --continue requires ZZ_SESSION_DB to be set; starting fresh");
                (Some(cid.clone()), Vec::new())
            }
        } else {
            (None, Vec::new())
        };

    // Build system prompt from skills + custom system prompt.
    let mut skill_prompt: Option<String> = {
        let skills_text = if no_skills {
            None
        } else if skill_names.is_empty() {
            // Load all skills (existing behavior).
            let (text, _names, _dirs) = load_skills_system_prompt();
            text
        } else {
            // Load only specified skills by name (same multi-source discovery).
            let mut registry = zerozero_skills::SkillRegistry::new();
            let _ = registry.load_standard();
            let filtered: Vec<_> = registry
                .all()
                .iter()
                .filter(|s| skill_names.contains(&s.name))
                .collect();
            if filtered.is_empty() {
                None
            } else {
                let mut text = String::from("# Skills\n\n");
                for skill in filtered {
                    text.push_str(&format!("## {}\n{}\n\n", skill.name, skill.content));
                }
                Some(text)
            }
        };

        // Combine skills + custom system prompt.
        let project_rules = load_project_rules();
        match (skills_text, system_prompt, project_rules) {
            (Some(skills), Some(custom), Some(rules)) => Some(format!(
                "{skills}\n\n# System Prompt\n\n{custom}\n\n{rules}"
            )),
            (Some(skills), Some(custom), None) => {
                Some(format!("{skills}\n\n# System Prompt\n\n{custom}"))
            }
            (Some(skills), None, Some(rules)) => Some(format!("{skills}\n\n{rules}")),
            (Some(skills), None, None) => Some(skills),
            (None, Some(custom), Some(rules)) => {
                Some(format!("# System Prompt\n\n{custom}\n\n{rules}"))
            }
            (None, Some(custom), None) => Some(format!("# System Prompt\n\n{custom}")),
            (None, None, Some(rules)) => Some(rules),
            (None, None, None) => None,
        }
    };

    let mut prompt = prompt;
    if !no_skills {
        let mut registry = zerozero_skills::SkillRegistry::new();
        let _ = registry.load_standard();
        if let Some((skill_names, task)) =
            zerozero_tui::slash::try_skill_exec_prompt(&prompt, &registry)
        {
            let block =
                zerozero_tui::slash::format_skill_chain_blocks(&registry, &skill_names, &task);
            skill_prompt = Some(match skill_prompt {
                Some(base) => format!("{base}\n\n{block}"),
                None => block,
            });
            prompt = task;
        }
    }

    // Always prepend the core identity prompt so the agent is never "naked"
    // (no identity, no safety rules, no tool guidance) even when the user
    // supplies no skills, no --system-prompt, and no project rules file.
    let final_system_prompt = zerozero_core::compose_system_prompt(skill_prompt.as_deref());

    // Dry run: print what we WOULD do, then exit without calling the API.
    if dry_run {
        println!("Dry run — configuration:");
        println!(
            "  Provider: {}",
            std::env::var("ZZ_PROVIDER").unwrap_or_else(|_| "xai".to_string())
        );
        println!(
            "  Model:    {}",
            std::env::var("ZZ_MODEL").unwrap_or_else(|_| "grok-4".to_string())
        );
        println!("  Sandbox:  {}", sandbox_str);
        println!("  Approval: {}", approval_str);
        println!("  Max turns: {}", max_turns);
        println!("  Prompt:   {}", &prompt[..prompt.len().min(100)]);
        if !final_system_prompt.is_empty() {
            let sp_preview = &final_system_prompt[..final_system_prompt.len().min(200)];
            println!("  System prompt: {}...", sp_preview);
        }
        println!("  Tools:    {} registered", tools.definitions().len());
        println!();
        println!("(Dry run — no API call was made)");
        return Ok(());
    }

    // Resolve the effective output style. `--json-pretty` remains a
    // backward-compatible alias that selects the `json-pretty` style.
    let style = if json_pretty {
        "json-pretty"
    } else {
        output_style
    };

    let mut output_file = if let Some(path) = &output_file {
        Some(std::fs::File::create(path)?)
    } else {
        None
    };

    // Collect the full event stream so it can be rendered once, after the
    // turn completes, according to the chosen output `style`.
    let mut collected: Vec<zerozero_exec::Event> = Vec::new();

    // Load hooks from .zerozero/hooks.toml (cwd). NoopHooks fallback.
    let hook_list = zerozero_core::load_hooks();
    let hooks: Box<dyn zerozero_core::LifecycleHooks> = if hook_list.is_empty() {
        Box::new(zerozero_core::NoopHooks)
    } else {
        eprintln!(
            "Loaded {} hook(s) from .zerozero/hooks.toml",
            hook_list.len()
        );
        Box::new(zerozero_core::CompositeHook::new(hook_list))
    };

    let perms = zerozero_core::ZeroZeroConfig::load().permissions;
    let result = zerozero_core::run_turn(
        &prompt,
        Some(&final_system_prompt),
        &*provider,
        &tools,
        max_turns,
        &sandbox,
        &approval,
        plan_mode,
        ask_mode,
        session_store.as_ref(),
        &compaction_config,
        &*hooks,
        continue_session_id.as_deref(),
        &prior_messages,
        effort,
        &perms,
        None,
        |event| {
            if quiet {
                // Quiet mode: only output the final agent_message
                // item.completed event and errors. Suppress all other
                // events (session.started, prompt, item.started,
                // item.updated, tool.*, approval.*, turn.completed, etc.).
                let print = matches!(
                    &event,
                    zerozero_exec::Event::ItemCompleted { item }
                        if matches!(item.kind, zerozero_exec::ItemKind::AgentMessage)
                ) || matches!(&event, zerozero_exec::Event::Error { .. });
                if print {
                    if let Ok(line) = serde_json::to_string(&event) {
                        println!("{line}");
                        if let Some(f) = &mut output_file {
                            use std::io::Write;
                            let _ = writeln!(f, "{line}");
                        }
                    }
                }
            } else {
                // Non-quiet: buffer every event so we can render the full
                // stream once, in the requested style, after the turn ends.
                collected.push(event);
            }
        },
    )
    .await;

    // Render the collected stream in the chosen style and emit it (plus
    // mirror to the optional output file). Quiet mode already printed
    // incrementally above, so it is skipped here. We render even on error so
    // that error events reach stdout as JSONL (e.g. AC-6: HTTP 500 must
    // surface an `error` event before the non-zero exit).
    if !quiet {
        let rendered = zerozero_cli::output::render_events(&collected, style);
        print!("{rendered}");
        if let Some(f) = &mut output_file {
            use std::io::Write;
            let _ = f.write_all(rendered.as_bytes());
        }
    }

    // Propagate the error instead of `std::process::exit` so the caller
    // (e.g. the background child) can record the outcome.
    result?;
    Ok(())
}

/// Print a helpful error message when the required API key is missing.
fn print_api_key_error(provider_type: &str) {
    let spec = provider_spec(provider_type);
    let env_name = if spec.api_key_env.is_empty() {
        "(none)"
    } else {
        spec.api_key_env
    };
    eprintln!("error: {env_name} not set for provider '{}'\n", spec.id);
    eprintln!("ZeroZero needs an API key to call the LLM provider.\n");
    eprintln!("Option A — save key (OpenCode-style, recommended):\n");
    eprintln!("  zz auth login {} --key \"your-key-here\"\n", spec.id);
    eprintln!("Option B — environment variable:\n");
    if !spec.api_key_env.is_empty() {
        eprintln!(
            "  $env:{} = \"your-key-here\"   # PowerShell",
            spec.api_key_env
        );
        eprintln!(
            "  export {}=\"your-key-here\"   # bash/zsh\n",
            spec.api_key_env
        );
    }
    eprintln!("Auth store: {}\n", auth_path().display());
    eprintln!("Available providers (ZZ_PROVIDER / --provider):\n");
    for p in zerozero_llm::PROVIDERS {
        let key = if p.requires_key {
            p.api_key_env
        } else {
            "(no key required)"
        };
        let def = if p.id == "xai" { " (default)" } else { "" };
        eprintln!("  {}{def:<10} {key:<22} {}", p.id, p.default_base_url);
    }
    eprintln!("\nExample:\n");
    eprintln!("  zz                          # open TUI, then type /connect");
    eprintln!("  zz connect xai              # interactive key entry (CLI)");
    eprintln!("  zz connect xai -k xai-...   # non-interactive");
    eprintln!("  zz auth login xai --key xai-...");
    eprintln!("  ZZ_PROVIDER=ollama zz       # local, no key");
}

/// `zz connect` dispatcher:
/// - provider id / empty → API key login
/// - `host:port` → remote TUI client
async fn run_connect_command(args: ConnectArgs) -> anyhow::Result<()> {
    match args.target.as_deref() {
        None => {
            // Interactive default-provider login (most common "I have no key" path).
            println!("No address given — treating as provider API-key connect (OpenCode-style).");
            println!("Tip: open the TUI with `zz` and type /connect for the full UI.\n");
            auth_login("xai", args.key)?;
        }
        Some(t) if looks_like_remote_addr(t) => {
            remote::run_connect(t.to_string()).await?;
        }
        Some(t) if zerozero_llm::find_provider(t).is_some() => {
            auth_login(t, args.key)?;
        }
        Some(t) => {
            anyhow::bail!(
                "unknown connect target '{t}'.\n\
                 \n\
                 Provider API key:\n\
                   zz connect xai\n\
                   zz connect openai -k sk-...\n\
                   zz auth login <provider> --key <KEY>\n\
                 \n\
                 Remote TUI client (zz serve):\n\
                   zz connect 127.0.0.1:7712\n\
                 \n\
                 Interactive TUI:\n\
                   zz\n\
                   then type /connect"
            );
        }
    }
    Ok(())
}

/// Whether `s` looks like a remote `host:port` rather than a provider id.
fn looks_like_remote_addr(s: &str) -> bool {
    if zerozero_llm::find_provider(s).is_some() {
        return false;
    }
    // host:port, IPv6-ish, or bare hostname with port separator
    s.contains(':')
        || s.parse::<std::net::SocketAddr>().is_ok()
        || s.starts_with("localhost")
        || s.ends_with(".local")
}

/// Generate a minute-precision timestamp ID for session branching.
fn chrono_minute_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:x}", secs / 60)
}

/// Validate that the required API key is available (env or auth store).
fn validate_api_key(provider_type: &str) {
    let id = resolve_provider_id(provider_type);
    if has_api_key(id) {
        return;
    }
    print_api_key_error(id);
    std::process::exit(1);
}

/// `zz auth` — manage provider credentials (OpenCode `/connect` parity).
fn run_auth(args: AuthArgs) -> anyhow::Result<()> {
    match args.command.unwrap_or(AuthCommand::List) {
        AuthCommand::List => {
            println!("Provider credentials\n");
            println!("Auth path: {}\n", auth_path().display());
            println!("{:<12} {:<22} {:<14} SOURCE", "PROVIDER", "ENV", "STATUS");
            println!("{}", "-".repeat(64));
            for p in zerozero_llm::PROVIDERS {
                let src = key_source(p.id);
                let (status, source) = match src {
                    KeySource::Env => ("set", "env"),
                    KeySource::AuthStore => ("set", "auth.json"),
                    KeySource::LegacyFallback => ("set", "legacy env"),
                    KeySource::NotRequired => ("n/a", "local"),
                    KeySource::Missing => ("missing", "-"),
                };
                let env = if p.api_key_env.is_empty() {
                    "-"
                } else {
                    p.api_key_env
                };
                println!("{:<12} {:<22} {:<14} {}", p.id, env, status, source);
            }
            println!("\nSave a key:  zz auth login <provider> --key <KEY>");
            println!("Remove key:  zz auth logout <provider>");
            println!("Use provider: zz --  or  $env:ZZ_PROVIDER='groq'; zz");
        }
        AuthCommand::Login { provider, key } => {
            auth_login(&provider, key)?;
        }
        AuthCommand::Set { provider, key } => {
            auth_login(&provider, Some(key))?;
        }
        AuthCommand::Logout { provider } => {
            let id = resolve_known_provider(&provider)?;
            let mut store = AuthStore::load()?;
            if store.remove(id) {
                store.save()?;
                println!(
                    "Removed stored key for '{id}' from {}",
                    auth_path().display()
                );
            } else {
                println!("No stored key for '{id}' in {}", auth_path().display());
            }
        }
        AuthCommand::Path => {
            println!("{}", auth_path().display());
        }
    }
    Ok(())
}

fn resolve_known_provider(raw: &str) -> anyhow::Result<&'static str> {
    zerozero_llm::find_provider(raw)
        .map(|p| p.id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown provider '{raw}'. Known: {}",
                provider_ids().join(", ")
            )
        })
}

fn auth_login(provider: &str, key: Option<String>) -> anyhow::Result<()> {
    let id = resolve_known_provider(provider)?;
    let spec = provider_spec(id);
    if !spec.requires_key {
        println!("Provider '{id}' does not require an API key (local). Nothing to store.");
        return Ok(());
    }
    let key = match key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => {
            // Read one line from stdin (scripts / pipes).
            use std::io::{self, Write};
            eprint!("Enter API key for {id} ({}): ", spec.api_key_env);
            let _ = io::stderr().flush();
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            let k = line.trim().to_string();
            if k.is_empty() {
                anyhow::bail!("empty API key — aborting");
            }
            k
        }
    };
    let mut store = AuthStore::load()?;
    store.set(id, key);
    store.save()?;
    println!("Saved API key for '{id}' → {}", auth_path().display());
    println!("Use it with:  $env:ZZ_PROVIDER='{id}'; zz   # or leave default for xai");
    println!("Verify:       zz doctor");
    Ok(())
}

/// Run the interactive TUI with streaming chat.
///
/// API key is **not** required at startup (OpenCode parity): the user can open
/// the TUI and use `/connect` to enter a key. Headless `zz exec` still requires
/// a key up front via [`validate_api_key`].
async fn run_tui() -> anyhow::Result<()> {
    // Load skills (for system prompt + /skills display).
    let (skill_prompt, skill_names, skill_dirs) = load_skills_system_prompt();
    // auto-inject project rules (AGENTS.md / CLAUDE.md) into the TUI
    // system prompt, mirroring the exec path.
    let skill_prompt = match (skill_prompt, load_project_rules()) {
        (Some(sp), Some(rules)) => Some(format!("{sp}\n\n{rules}")),
        (Some(sp), None) => Some(sp),
        (None, Some(rules)) => Some(rules),
        (None, None) => None,
    };
    // Always prepend the core identity prompt (same as exec path) so the
    // agent is never "naked" in the TUI either.
    let skill_prompt = Some(zerozero_core::compose_system_prompt(
        skill_prompt.as_deref(),
    ));

    let sandbox = match std::env::var("ZZ_SANDBOX").unwrap_or_default().as_str() {
        "read-only" => SandboxPolicy::ReadOnly,
        "full-access" => SandboxPolicy::FullAccess,
        _ => SandboxPolicy::WorkspaceWrite {
            workspace_dir: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
        },
    };

    let approval = match std::env::var("ZZ_APPROVAL").unwrap_or_default().as_str() {
        "never" => ApprovalPolicy::Never,
        "untrusted" => ApprovalPolicy::Untrusted,
        "auto-edit" => ApprovalPolicy::AutoEdit,
        "on-ask" => ApprovalPolicy::OnAsk,
        _ => ApprovalPolicy::OnRequest,
    };

    let provider: Arc<dyn Provider> = Arc::from(build_provider());
    let provider_factory = make_provider_factory();
    // : initial model name for the TUI /model command.
    // Matches the default from build_provider for the current provider type.
    let initial_model = {
        let spec = provider_spec(&std::env::var("ZZ_PROVIDER").unwrap_or_default());
        resolve_model(spec, None)
    };
    let mut tools = ToolRegistry::standard(Arc::new(sandbox.clone()));
    let (_plugin_count, plugin_names) = register_external_plugins(&mut tools);

    let compaction_config = CompactionConfig {
        max_messages: std::env::var("ZZ_MAX_MESSAGES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20),
        max_tokens: std::env::var("ZZ_MAX_TOKENS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100_000),
        keep_recent: std::env::var("ZZ_KEEP_RECENT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6),
        token_budget: std::env::var("ZZ_TOKEN_BUDGET")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100_000),
        keep_recent_turns: std::env::var("ZZ_KEEP_RECENT_TURNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(6),
    };

    // Plugin directories for hot-reload.
    let mut plugin_dirs = Vec::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    plugin_dirs.push(cwd.join(".zerozero"));

    // Session DB path for /sessions command.
    let session_db_path = std::env::var("ZZ_SESSION_DB")
        .ok()
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let home = std::env::var("HOME").ok()?;
            Some(std::path::PathBuf::from(format!(
                "{home}/.zerozero/sessions.db"
            )))
        });

    zerozero_tui::run_async(
        provider,
        tools,
        sandbox,
        approval,
        compaction_config,
        skill_names,
        skill_dirs,
        plugin_names,
        plugin_dirs,
        session_db_path,
        skill_prompt,
        provider_factory,
        initial_model,
    )
    .await
}

/// Run `zz sessions list`: print saved sessions as a table.
fn run_sessions_list(json: bool) -> anyhow::Result<()> {
    let session_db = std::env::var("ZZ_SESSION_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.zerozero/sessions.db")
    });

    let store = SessionStore::open(std::path::Path::new(&session_db))?;
    let sessions = store.list_sessions()?;

    if json {
        let arr: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "created_at": s.created_at,
                    "message_count": s.message_count,
                    "prompt": s.prompt,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
        return Ok(());
    }

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!("{:<30} {:<20} {:<8} Prompt", "ID", "Created", "Msgs");
    println!("{}", "-".repeat(80));
    for s in &sessions {
        let prompt_preview = if s.prompt.len() > 30 {
            format!("{}...", &s.prompt[..30])
        } else {
            s.prompt.clone()
        };
        println!(
            "{:<30} {:<20} {:<8} {}",
            s.id, s.created_at, s.message_count, prompt_preview
        );
    }
    Ok(())
}

/// Run `zz rewind <session_id> [--to <seq>]` .
/// Without `--to`: list checkpoints (each user message) for the session.
/// With `--to <seq>`: truncate the session to that checkpoint (delete
/// messages with seq > target). The target must be a user message.
fn run_rewind(session_id: String, to: Option<i64>) -> anyhow::Result<()> {
    let session_db = std::env::var("ZZ_SESSION_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.zerozero/sessions.db")
    });

    let store = SessionStore::open(std::path::Path::new(&session_db))?;

    match to {
        None => {
            let checkpoints = store.checkpoint_list(&session_id)?;
            if checkpoints.is_empty() {
                println!("No checkpoints found for session {session_id}");
                return Ok(());
            }
            println!("Checkpoints for session {session_id}:");
            println!("{:<6} {:<10} {:<24} Preview", "Seq", "Role", "Created");
            println!("{}", "-".repeat(80));
            for cp in &checkpoints {
                println!(
                    "{:<6} {:<10} {:<24} {}",
                    cp.seq, cp.role, cp.created_at, cp.content_preview
                );
            }
            println!("\nRewind with: zz rewind {session_id} --to <seq>");
        }
        Some(target_seq) => {
            let checkpoints = store.checkpoint_list(&session_id)?;
            if !checkpoints.iter().any(|c| c.seq == target_seq) {
                eprintln!(
                    "error: seq {target_seq} is not a valid checkpoint (must be a user message)"
                );
                std::process::exit(1);
            }
            store.truncate_after(&session_id, target_seq)?;
            let remaining = store.get_messages(&session_id)?;
            println!(
                "Rewound to seq {target_seq} ({} messages remaining)",
                remaining.len()
            );
        }
    }

    Ok(())
}

/// Run `zz resume [--last | --id <ID>] [--inspect]` F14).
///
/// Without flags: list recent sessions (alias of `zz sessions`).
/// `--last` / `--id X`: print the session's metadata + (with `--inspect`)
/// its persisted plan and approval decisions. Full TUI resume re-opens the
/// session transcript and continues the conversation.
fn run_resume(last: bool, id: Option<String>, inspect: bool) -> anyhow::Result<()> {
    let session_db = std::env::var("ZZ_SESSION_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.zerozero/sessions.db")
    });
    let store = SessionStore::open(std::path::Path::new(&session_db))?;
    let sessions = store.list_sessions()?;

    let target_id = match (last, id) {
        (_, Some(i)) => Some(i),
        (true, None) => sessions.first().map(|s| s.id.clone()),
        (false, None) => {
            // No target: list sessions.
            if sessions.is_empty() {
                println!("No saved sessions.");
                return Ok(());
            }
            println!("{:<30} {:<20} {:<8} Prompt", "ID", "Created", "Msgs");
            println!("{}", "-".repeat(80));
            for s in &sessions {
                let preview: String = if s.prompt.chars().count() > 30 {
                    format!("{}...", s.prompt.chars().take(30).collect::<String>())
                } else {
                    s.prompt.clone()
                };
                println!(
                    "{:<30} {:<20} {:<8} {}",
                    s.id, s.created_at, s.message_count, preview
                );
            }
            println!("\nResume with: zz resume --id <ID>  (or `zz resume --last`)");
            return Ok(());
        }
    };

    let target_id = target_id.ok_or_else(|| anyhow::anyhow!("no session to resume"))?;
    let meta = sessions
        .into_iter()
        .find(|s| s.id == target_id)
        .ok_or_else(|| anyhow::anyhow!("session {target_id} not found"))?;

    println!("Resuming session: {}", meta.id);
    println!("  created: {}", meta.created_at);
    println!("  prompt : {}", meta.prompt);
    println!("  messages: {}", meta.message_count);

    if inspect {
        match store.get_plan(&target_id)? {
            Some(plan) => println!("  plan:\n{plan}"),
            None => println!("  plan: (none)"),
        }
        let approvals = store.get_approvals(&target_id)?;
        if approvals.is_empty() {
            println!("  approvals: (none)");
        } else {
            println!("  approvals:");
            for a in &approvals {
                println!("    - {a}");
            }
        }
    }
    Ok(())
}

/// Render a session's messages as readable markdown .
///
/// Pure function — given the transcript, produce a `## user` / `## assistant`
/// / `## tool` structured markdown block. Kept pure so it is unit-testable
/// without a database.
pub fn render_transcript_markdown(messages: &[zerozero_llm::ChatMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" | "system" => {
                out.push_str(&format!("## {}\n\n", msg.role));
                out.push_str(msg.content.trim());
                out.push_str("\n\n");
            }
            "assistant" => {
                out.push_str("## assistant\n\n");
                if !msg.content.trim().is_empty() {
                    out.push_str(msg.content.trim());
                    out.push('\n');
                }
                if let Some(calls) = &msg.tool_calls {
                    for call in calls {
                        out.push_str(&format!(
                            "- tool_call `{}` (`{}`):\n```\n{}\n```\n",
                            call.function.name, call.id, call.function.arguments
                        ));
                    }
                }
                out.push('\n');
            }
            "tool" => {
                let who = msg
                    .tool_call_id
                    .as_ref()
                    .map(|id| format!(" (call {id})"))
                    .unwrap_or_default();
                out.push_str(&format!("## tool result{who}\n\n```\n"));
                out.push_str(msg.content.trim());
                out.push_str("\n```\n\n");
            }
            other => {
                out.push_str(&format!("## {other}\n\n"));
                out.push_str(msg.content.trim());
                out.push_str("\n\n");
            }
        }
    }
    out
}

/// Run `zz session show <id>` / `zz session export <id>` .
async fn run_session_command(command: SessionCommand) -> anyhow::Result<()> {
    let session_db = std::env::var("ZZ_SESSION_DB").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{home}/.zerozero/sessions.db")
    });
    let store = SessionStore::open(std::path::Path::new(&session_db))?;

    match command {
        SessionCommand::Show { id } => {
            let msgs = store.get_messages(&id)?;
            if msgs.is_empty() {
                eprintln!("warning: session {id} has no messages");
            }
            print!("{}", render_transcript_markdown(&msgs));
        }
        SessionCommand::Export { id } => {
            let json = store.export_session(&id)?;
            println!("{json}");
        }
        SessionCommand::Delete { id } => {
            store.delete_session(&id)?;
            println!("Deleted session {id}.");
        }
        SessionCommand::Prune => {
            let sessions = store.list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions to prune.");
                return Ok(());
            }
            let n = sessions.len();
            for s in &sessions {
                store.delete_session(&s.id)?;
            }
            println!("Pruned {n} session(s).");
        }
    }
    Ok(())
}

/// mode (default) or agent mode (--agent, calls `zz exec` on each task).
fn run_eval(agent_mode: bool, task_filter: Option<String>) -> anyhow::Result<()> {
    // Search for eval/run.sh from CWD upward (handles running from
    // crates/cli/ during tests or from project root normally).
    let run_script = find_eval_script()
        .ok_or_else(|| anyhow::anyhow!("eval/run.sh not found — eval suite not set up"))?;

    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&run_script);
    if agent_mode {
        cmd.arg("--agent");
    }
    if let Some(task) = &task_filter {
        cmd.arg(task);
    }

    let status = cmd
        .status()
        .map_err(|e| anyhow::anyhow!("failed to run eval: {e}"))?;

    if !status.success() {
        std::process::exit(1);
    }
    Ok(())
}

/// Find eval/run.sh by searching from CWD upward through parent dirs.
fn find_eval_script() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("eval").join("run.sh");
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Append the contents of `--include` files to the prompt under an
/// "Included files" section (parity: Codex `--include`). A missing file is
/// a hard error (no silent skip).
fn apply_include_files(mut prompt: String, includes: &[String]) -> anyhow::Result<String> {
    if includes.is_empty() {
        return Ok(prompt);
    }
    let mut section = String::from("\n\n## Included files\n");
    for path in includes {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read --include file '{path}': {e}"))?;
        section.push_str(&format!("\n### {path}\n```\n{content}\n```\n"));
    }
    prompt.push_str(&section);
    Ok(prompt)
}

/// Resolve the prompt from a positional arg or stdin.
/// A missing value or the literal "-" means "read from stdin".
fn read_prompt(prompt: Option<String>) -> anyhow::Result<String> {
    match prompt {
        // `@path` reads the prompt from a file (parity: Codex `codex @path`).
        Some(p) if p.starts_with('@') => {
            let path = &p[1..];
            let content = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("failed to read prompt file '{path}': {e}"))?;
            Ok(content)
        }
        Some(p) if p != "-" => Ok(p),
        _ => {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
    }
}

/// Run `zz multi "prompt1,prompt2,..."`: run multiple agents in parallel.
async fn run_multi(prompts: String) -> anyhow::Result<()> {
    let provider_type = std::env::var("ZZ_PROVIDER").unwrap_or_default();
    validate_api_key(&provider_type);

    let max_turns: u32 = std::env::var("ZZ_MAX_TURNS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let provider: Arc<dyn Provider> = Arc::from(build_provider());
    let sandbox = SandboxPolicy::FullAccess;
    let mut tools = ToolRegistry::standard(Arc::new(sandbox.clone()));
    let _ = register_external_plugins(&mut tools);
    let tools = Arc::new(tools);
    let approval = ApprovalPolicy::Never;
    let compaction_config = CompactionConfig::default();

    let tasks: Vec<zerozero_multi_agent::AgentTask> = prompts
        .split(',')
        .enumerate()
        .map(|(i, p)| zerozero_multi_agent::AgentTask {
            id: format!("agent-{i}"),
            prompt: p.trim().to_string(),
            max_turns,
        })
        .collect();

    let orchestrator = zerozero_multi_agent::MultiAgentOrchestrator::new(
        provider,
        tools,
        sandbox,
        approval,
        compaction_config,
    );

    let results = orchestrator.run_parallel(tasks).await;

    for result in &results {
        println!(
            "{{\"agent_id\":\"{}\",\"success\":{},\"error\":{}}}",
            result.id,
            result.success,
            serde_json::Value::from(result.error.clone().unwrap_or_default())
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-wide env vars so they do not
    /// interfere with each other or with other tests reading env vars.
    /// Rust 2024 edition requires `unsafe` around `std::env::set_var`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// AC-3 : Verify `build_provider_with_model(Some(model))` uses
    /// the model override, not the `ZZ_MODEL` env var. This is the factory
    /// path used by the TUI `/model` slash command closure
    /// (`make_provider_factory`), which the E2E test cannot directly
    /// exercise (it only covers the CLI `--model` flag → env var path).
    #[test]
    fn test_build_provider_with_model_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: Held `ENV_LOCK` so no other env-mutating test runs
        // concurrently. No async tasks are spawned here.
        unsafe {
            std::env::set_var("ZZ_PROVIDER", "openai");
            std::env::set_var("OPENAI_API_KEY", "test-key");
            std::env::set_var("ZZ_MODEL", "gpt-4o-mini");
        }

        // With override — should use "grok-4.3", NOT "gpt-4o-mini".
        let provider = build_provider_with_model(Some("grok-4.3".to_string()));
        assert_eq!(
            provider.model(),
            "grok-4.3",
            "model override must take precedence over ZZ_MODEL env var"
        );

        // Without override — should fall back to ZZ_MODEL env var.
        let provider = build_provider_with_model(None);
        assert_eq!(
            provider.model(),
            "gpt-4o-mini",
            "without override, ZZ_MODEL env var must be used"
        );
    }

    /// AC-3 : Verify the factory closure produced by
    /// `make_provider_factory` builds a provider with the correct model.
    /// This is the exact path used by the TUI `/model` slash command to
    /// hot-swap the provider mid-session.
    #[test]
    fn test_make_provider_factory() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: Held `ENV_LOCK` so no other env-mutating test runs
        // concurrently. No async tasks are spawned here.
        unsafe {
            std::env::set_var("ZZ_PROVIDER", "openai");
            std::env::set_var("OPENAI_API_KEY", "test-key");
        }

        let factory = make_provider_factory();
        let provider = factory("openai".to_string(), "o3-mini".to_string());
        assert_eq!(
            provider.model(),
            "o3-mini",
            "factory closure must produce provider with the given model"
        );

        // Factory can be called multiple times with different models.
        let provider2 = factory("openai".to_string(), "gpt-5".to_string());
        assert_eq!(
            provider2.model(),
            "gpt-5",
            "factory closure must be reusable with a different model"
        );

        // Factory can switch provider type mid-session (3-tier picker path).
        unsafe {
            std::env::set_var("XAI_API_KEY", "test-xai-key");
        }
        let provider3 = factory("xai".to_string(), "grok-4".to_string());
        assert_eq!(
            provider3.model(),
            "grok-4",
            "factory must switch provider type when given a different id"
        );
    }

    /// AC-7 : the `Mcp` subcommand (and its `serve` subcommand)
    /// must parse via the `Command` enum, so `zz mcp serve` dispatches to
    /// `run_mcp_serve` → `McpServer::run`.
    #[test]
    fn test_mcp_subcommand_parses() {
        // `zz mcp serve` parses and selects the Mcp/Serve variant.
        let cli = Cli::try_parse_from(["zz", "mcp", "serve"]).expect("zz mcp serve must parse");
        match cli.command {
            Some(Command::Mcp(McpSubcommand::Serve(_))) => {}
            _ => panic!("expected Command::Mcp(McpSubcommand::Serve) for `zz mcp serve`"),
        }

        // `zz mcp serve --port 9000` parses with the port override.
        let cli = Cli::try_parse_from(["zz", "mcp", "serve", "--port", "9000"])
            .expect("zz mcp serve --port must parse");
        match cli.command {
            Some(Command::Mcp(McpSubcommand::Serve(args))) => {
                assert_eq!(args.port, Some(9000));
            }
            _ => panic!("expected Command::Mcp::Serve for `zz mcp serve --port 9000`"),
        }

        // `zz mcp` without a subcommand must fail to parse (no silent
        // no-op) — `serve` is required.
        let result = Cli::try_parse_from(["zz", "mcp"]);
        assert!(
            result.is_err(),
            "zz mcp without a subcommand must be rejected"
        );
    }

    /// AC-1: transcript renders as structured markdown with
    /// user/assistant/tool sections and tool-call blocks.
    #[test]
    fn test_render_transcript_markdown() {
        use zerozero_llm::ChatMessage;
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
            ChatMessage {
                role: "user".into(),
                content: "Fix the bug".into(),
                tool_call_id: None,
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: "On it.".into(),
                tool_call_id: None,
                tool_calls: Some(vec![zerozero_llm::ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    function: zerozero_llm::ToolCallFunction {
                        name: "Bash".into(),
                        arguments: "{\"cmd\":\"ls\"}".into(),
                    },
                }]),
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: "file.txt".into(),
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                attachments: None,
                thinking_signature: None,
                redacted_thinking: None,
                thinking: None,
            },
        ];
        let md = render_transcript_markdown(&msgs);
        assert!(md.contains("## system"), "got: {md}");
        assert!(md.contains("## user"), "got: {md}");
        assert!(md.contains("## assistant"), "got: {md}");
        assert!(md.contains("## tool result (call call_1)"), "got: {md}");
        assert!(md.contains("tool_call `Bash` (`call_1`)"), "got: {md}");
        assert!(md.contains("Fix the bug"), "got: {md}");
    }

    /// `read_prompt` expands `@path` to the file's contents, and
    /// a missing file is a hard error (no silent no-op).
    #[test]
    fn test_read_prompt_expands_at_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("prompt.txt");
        std::fs::write(&p, "refactor the parser").unwrap();
        let got = read_prompt(Some(format!("@{}", p.display()))).unwrap();
        assert_eq!(got, "refactor the parser");

        // Missing file must error.
        let missing = read_prompt(Some("@/no/such/file/zz".to_string()));
        assert!(missing.is_err(), "missing @file must error");

        // `-` and None still read stdin.
        let stdin_prompt = read_prompt(Some("-".to_string())).unwrap();
        assert_eq!(stdin_prompt, "");
    }

    /// `apply_include_files` appends file contents under an
    /// "Included files" section; missing file is a hard error.
    #[test]
    fn test_apply_include_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ctx.txt");
        std::fs::write(&p, "fn main() {}").unwrap();

        let out = apply_include_files("fix it".to_string(), &[p.display().to_string()]).unwrap();
        assert!(out.contains("fix it"), "got: {out}");
        assert!(out.contains("## Included files"), "got: {out}");
        assert!(out.contains("fn main() {}"), "got: {out}");

        // Empty include list is a no-op.
        let same = apply_include_files("x".to_string(), &[]).unwrap();
        assert_eq!(same, "x");

        // Missing file must error.
        assert!(apply_include_files("x".to_string(), &["/no/such".to_string()]).is_err());
    }

    /// `update_job_result` flips a persisted job's status/result.
    /// Uses a temp HOME so the durable store (`~/.config/zerozero/jobs.json`)
    /// lives in isolation. Tests the actual background-completion code path.
    #[test]
    fn test_update_job_result_flips_store() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", dir.path());
        }

        // Seed a Running job (as the parent would on `zz exec --background`).
        let path = jobs::JobStore::default_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"[{"id":"job-x","prompt":"do it","created_at":"T","status":"running","result":null,"session_id":null}]"#,
        )
        .unwrap();

        // Child completes successfully -> Done.
        update_job_result("job-x", Ok(()));
        let store = jobs::JobStore::new(jobs::JobStore::default_path());
        store.load().unwrap();
        let job = store.get("job-x").unwrap();
        assert_eq!(job.status, jobs::JobStatus::Done);
        assert_eq!(job.result.as_deref(), Some("completed"));

        // Child fails -> Failed with the error message.
        update_job_result("job-x", Err(anyhow::anyhow!("boom")));
        let store = jobs::JobStore::new(jobs::JobStore::default_path());
        store.load().unwrap();
        let job = store.get("job-x").unwrap();
        assert_eq!(job.status, jobs::JobStatus::Failed);
        assert!(job.result.unwrap().contains("boom"));
    }

    #[test]
    fn test_jobs_log_reads_captured_output() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("HOME", dir.path());
        }

        // Write a captured log for job-y under the real log path.
        let log_path = jobs::JobStore::log_path("job-y");
        std::fs::create_dir_all(log_path.parent().unwrap()).unwrap();
        std::fs::write(&log_path, "line1\nline2 from child\n").unwrap();

        // read_log returns the captured bytes (real file read, not mocked).
        let store = jobs::JobStore::new(jobs::JobStore::default_path());
        let contents = store.read_log("job-y").expect("log should exist");
        assert_eq!(contents, "line1\nline2 from child\n");

        // Absent id -> NotFound (drives the `no log for job` error path).
        let missing = store.read_log("job-none").unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    }
}
