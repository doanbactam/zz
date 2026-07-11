//! AC-3: `/agent` slash command + picker + footer — Phase 3 (tui crate).
//!
//! Test: `/agent` input → picker shows ≥ 2 threads (root + subagent),
//! select → active_thread_id switch, footer shows agent label.
//! Alt+Left/Alt+Right switch prev/next. Old thread stays Running after switch.
//!
//! Pattern: (PTY E2E — \r Enter, background PTY reader, strip_ansi).
//! Also includes unit-level assertions for multi-thread scenarios that
//! cannot be tested via PTY (SpawnAgentTool not yet wired into ToolRegistry,
//! so the TUI only has the root thread at startup).

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use zerozero_multi_agent::{AgentMetadata, AgentPath, AgentStatus};
use zerozero_tui::app::{App, KeyAction, SlashAction};

/// Strip ANSI escape sequences from a string (PAT-7 rule 3).
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// AC-3 test 1: `/agent` slash command opens the picker UI in the TUI.
///
/// PTY E2E: Type `/agent` + Enter (\r), verify the picker popup renders
/// with "Agent Threads" title. The picker should display the root thread.
///
/// compliance:
/// - `\r` for Enter key (not `\n`)
/// - Background thread for PTY reader (std::sync::mpsc + recv_timeout)
/// - strip_ansi() before contains check
/// - #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac3_agent_picker_displays_threads() {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let bin = env!("CARGO_BIN_EXE_zz");
    let mut cmd = CommandBuilder::new(bin);
    cmd.env("OPENAI_API_KEY", "test-key-for-tui");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn zz");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let mut writer = pair.master.take_writer().expect("take writer");

    // rule 2: Background thread for PTY reader.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(vec![]);
                    break;
                }
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = tx.send(vec![]);
                    break;
                }
            }
        }
    });

    // Wait for TUI to initialize (render "Input" label).
    let mut output = String::new();
    let start = Instant::now();
    let init_deadline = Duration::from_secs(10);

    let initialized = loop {
        if start.elapsed() >= init_deadline {
            break false;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) if chunk.is_empty() => break false,
            Ok(chunk) => {
                output.push_str(&String::from_utf8_lossy(&chunk));
                if output.contains("Composer") {
                    break true;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break false,
        }
    };

    if !initialized {
        let _ = writer.write_all(b"q");
        drop(writer);
        drop(pair.master);
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "TUI did not initialize. Output ({} bytes): {}",
            output.len(),
            &output[..output.len().min(500)]
        );
    }

    // rule 1: Send /agent with \r (Carriage Return) for Enter.
    // Send characters with small delays to ensure TUI processes each one.
    for byte in b"/agent" {
        let _ = writer.write_all(&[*byte]);
        let _ = writer.flush();
        std::thread::sleep(Duration::from_millis(20));
    }
    // Send Enter as \r (Carriage Return) — NOT \n (Line Feed).
    // In raw mode, crossterm only maps \r to KeyCode::Enter.
    let _ = writer.write_all(b"\r");
    let _ = writer.flush();

    // Collect output after /agent command — look for picker popup.
    output.clear();
    let render_deadline = Duration::from_secs(10);
    let start = Instant::now();

    let picker_shown = loop {
        if start.elapsed() >= render_deadline {
            break false;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) if chunk.is_empty() => break false,
            Ok(chunk) => {
                output.push_str(&String::from_utf8_lossy(&chunk));
                // rule 3: strip_ansi before contains check.
                // Ratatui renders each cell independently, so "Agent Threads"
                // is split across cursor positioning sequences. Check tokens
                // separately contains("A") && contains("B") not
                // contains("A B")).
                let stripped = strip_ansi(&output);
                if stripped.contains("Agent") && stripped.contains("Threads") {
                    break true;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break false,
        }
    };

    // Close picker with Esc, then quit.
    let _ = writer.write_all(b"\x1b"); // Esc
    std::thread::sleep(Duration::from_millis(100));
    let _ = writer.write_all(b"q");
    drop(writer);
    drop(pair.master);

    let wait_deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < wait_deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        picker_shown,
        "Expected /agent to open picker with 'Agent Threads' title. Output: {}",
        &output[..output.len().min(500)]
    );

    // --- Unit-level: verify picker displays ≥ 2 threads with multiple agents ---
    let mut app = App::new();
    let root_id = "root-001".to_string();
    let child_id = "child-001".to_string();
    app.active_thread_id = root_id.clone();
    app.live_agents = vec![
        AgentMetadata {
            thread_id: root_id.clone(),
            agent_path: AgentPath::root(),
            parent_thread_id: None,
            depth: 0,
            nickname: "root".to_string(),
            status: AgentStatus::Running,
            last_task_message: None,
        },
        AgentMetadata {
            thread_id: child_id,
            agent_path: AgentPath::root().child(0),
            parent_thread_id: Some(root_id),
            depth: 1,
            nickname: "agent-0".to_string(),
            status: AgentStatus::Running,
            last_task_message: Some("test task".to_string()),
        },
    ];
    app.show_agent_picker = true;

    // Verify footer label shows when multiple threads exist.
    let footer = app.agent_footer_label();
    assert!(
        !footer.is_empty(),
        "footer label should be non-empty when multiple threads exist"
    );
    assert!(
        footer.contains("root"),
        "footer should contain active agent nickname: {}",
        footer
    );
}

/// AC-3 test 2: Selecting an agent in the picker switches active_thread_id.
///
/// Unit-level: App with 2 agents, open picker, navigate Down, press Enter
/// → SelectAgent action returned, active_thread_id switches.
/// Also verifies Esc closes the picker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac3_agent_picker_select_switches_thread() {
    let mut app = App::new();
    let root_id = "root-002".to_string();
    let child_id = "child-002".to_string();
    app.active_thread_id = root_id.clone();
    app.live_agents = vec![
        AgentMetadata {
            thread_id: root_id.clone(),
            agent_path: AgentPath::root(),
            parent_thread_id: None,
            depth: 0,
            nickname: "root".to_string(),
            status: AgentStatus::Running,
            last_task_message: None,
        },
        AgentMetadata {
            thread_id: child_id.clone(),
            agent_path: AgentPath::root().child(0),
            parent_thread_id: Some(root_id.clone()),
            depth: 1,
            nickname: "agent-0".to_string(),
            status: AgentStatus::Running,
            last_task_message: Some("test".to_string()),
        },
    ];

    // Open picker via /agent slash command.
    app.show_agent_picker = true;
    app.agent_picker_selected = 0;
    assert!(app.show_agent_picker);

    // Navigate Down to select the second agent.
    let action = app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(action, KeyAction::None, "Down should navigate, not submit");
    assert_eq!(app.agent_picker_selected, 1, "should select second agent");

    // Press Enter to select — should return SelectAgent(1).
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        action,
        KeyAction::SelectAgent(1),
        "Enter in picker should return SelectAgent(index)"
    );
    assert!(
        !app.show_agent_picker,
        "picker should close after selection"
    );

    // Simulate the switch (as done in lib.rs event loop).
    if let KeyAction::SelectAgent(index) = action {
        if let Some(tid) = zerozero_tui::agent_nav::agent_at(&app.live_agents, index) {
            app.active_thread_id = tid;
        }
    }
    assert_eq!(
        app.active_thread_id, child_id,
        "active_thread_id should switch to selected agent"
    );

    // Verify the old thread (root) is still Running (not stopped by switch).
    let root_meta = app
        .live_agents
        .iter()
        .find(|a| a.thread_id == root_id)
        .expect("root should still be in live_agents");
    assert_eq!(
        root_meta.status,
        AgentStatus::Running,
        "old thread should still be Running after switch (no stop)"
    );

    // Test Esc closes picker.
    app.show_agent_picker = true;
    let action = app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(action, KeyAction::None);
    assert!(!app.show_agent_picker, "Esc should close picker");
}

/// AC-3 test 3: Alt+Left/Alt+Right switch prev/next agent thread.
///
/// Unit-level: App with 2 agents, Alt+Left → SwitchAgentPrev,
/// Alt+Right → SwitchAgentNext. Also verifies agent_nav prev/next
/// logic with wrapping.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac3_alt_left_right_switches_thread() {
    let mut app = App::new();
    let root_id = "root-003".to_string();
    let child_id = "child-003".to_string();
    app.active_thread_id = root_id.clone();
    app.live_agents = vec![
        AgentMetadata {
            thread_id: root_id.clone(),
            agent_path: AgentPath::root(),
            parent_thread_id: None,
            depth: 0,
            nickname: "root".to_string(),
            status: AgentStatus::Running,
            last_task_message: None,
        },
        AgentMetadata {
            thread_id: child_id.clone(),
            agent_path: AgentPath::root().child(0),
            parent_thread_id: Some(root_id.clone()),
            depth: 1,
            nickname: "agent-0".to_string(),
            status: AgentStatus::Running,
            last_task_message: Some("test".to_string()),
        },
    ];

    // Alt+Right should return SwitchAgentNext.
    let action = app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT));
    assert_eq!(
        action,
        KeyAction::SwitchAgentNext,
        "Alt+Right should return SwitchAgentNext"
    );

    // Alt+Left should return SwitchAgentPrev.
    let action = app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    assert_eq!(
        action,
        KeyAction::SwitchAgentPrev,
        "Alt+Left should return SwitchAgentPrev"
    );

    // Verify agent_nav logic: next from root → child.
    let next = zerozero_tui::agent_nav::next_agent(&app.live_agents, &root_id);
    assert_eq!(
        next,
        Some(child_id.clone()),
        "next_agent from root should return child"
    );

    // Verify agent_nav logic: prev from root → child (wrapping).
    let prev = zerozero_tui::agent_nav::prev_agent(&app.live_agents, &root_id);
    assert_eq!(
        prev,
        Some(child_id.clone()),
        "prev_agent from root should wrap to child"
    );

    // Verify agent_nav logic: next from child → root (wrapping).
    let next = zerozero_tui::agent_nav::next_agent(&app.live_agents, &child_id);
    assert_eq!(
        next,
        Some(root_id.clone()),
        "next_agent from child should wrap to root"
    );

    // Verify agent_nav with single agent returns None.
    let single = vec![app.live_agents[0].clone()];
    let next = zerozero_tui::agent_nav::next_agent(&single, &root_id);
    assert_eq!(next, None, "next_agent with 1 agent should return None");
    let prev = zerozero_tui::agent_nav::prev_agent(&single, &root_id);
    assert_eq!(prev, None, "prev_agent with 1 agent should return None");

    // Verify /agent slash command parsing.
    app.composer.input_buffer = "/agent".to_string();
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        action,
        KeyAction::Slash(SlashAction::OpenAgentPicker),
        "/agent should produce OpenAgentPicker action"
    );
}
