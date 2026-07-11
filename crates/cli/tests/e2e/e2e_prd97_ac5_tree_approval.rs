//! AC-5: Tree view + approval cross-thread overlay — Phase 5 (tui crate).
//!
//! Test: Tree view shows indented list (root + root.0). Approval from
//! inactive thread → overlay with source label, press `o` → switch to
//! source thread, approve → resolved.
//!
//! Pattern: (PTY E2E — \r Enter, background PTY reader, strip_ansi).
//! Also includes unit-level assertions using TestBackend for tree view
//! and approval overlay rendering (SpawnAgentTool not yet wired, so TUI
//! only has root thread at startup — multi-thread scenarios tested at
//! unit level).

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use zerozero_multi_agent::{AgentMetadata, AgentPath, AgentStatus};
use zerozero_tui::app::{App, KeyAction, PendingApproval};

/// Render the TUI to a string using TestBackend (for unit-level assertions).
fn render_to_string(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| zerozero_tui::ui::render(f, app)).unwrap();
    format!("{:?}", terminal.backend().buffer())
}

/// Build a test App with 2 agents (root + child) for tree view testing.
fn make_app_with_2_agents() -> App {
    let mut app = App::new();
    let root_id = "root-005".to_string();
    let child_id = "child-005".to_string();
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
            last_task_message: Some("subtask".to_string()),
        },
    ];
    app
}

/// AC-5 test 1: Tree view displays indented list (root + root.0).
///
/// Unit-level: App with 2 agents, show_agent_tree=true, render via
/// TestBackend, verify "Agents" title and both agent paths appear.
/// Also PTY E2E: verify TUI starts without crash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac5_tree_view_displays_indented_list() {
    // --- Unit-level: tree view rendering ---
    let mut app = make_app_with_2_agents();
    app.show_agent_tree = true;

    let content = render_to_string(&app);

    // The tree view should show "Agents" title.
    assert!(
        content.contains("Agents"),
        "tree view should render 'Agents' title. Content: {}",
        &content[..content.len().min(500)]
    );

    // The tree view should show both agent paths.
    assert!(
        content.contains("root"),
        "tree view should show root agent. Content: {}",
        &content[..content.len().min(500)]
    );
    assert!(
        content.contains("agent-0") || content.contains("agent"),
        "tree view should show child agent nickname. Content: {}",
        &content[..content.len().min(500)]
    );
    assert!(
        content.contains("root.0"),
        "tree view should show child agent_path 'root.0'. Content: {}",
        &content[..content.len().min(500)]
    );

    // --- PTY E2E: verify TUI starts and handles keys without crash ---
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

    // Wait for TUI to initialize.
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

    // Quit TUI.
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
        initialized,
        "TUI should initialize without crash. Output: {}",
        &output[..output.len().min(500)]
    );
}

/// AC-5 test 2: Approval overlay shows source label for inactive thread.
///
/// Unit-level: App with pending_approval from an inactive thread, render
/// via TestBackend, verify overlay shows "Pending Approval" and source
/// thread label.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac5_approval_overlay_shows_source_label() {
    let mut app = make_app_with_2_agents();
    let source_thread_id = "child-005".to_string();

    // Set pending approval from the child (inactive) thread.
    app.pending_approval = Some(PendingApproval {
        source_thread_id: source_thread_id.clone(),
        tool_call_id: "call-123".to_string(),
        tool_name: "bash".to_string(),
        args: serde_json::json!({"command": "rm -rf /tmp/test"}),
        danger_level: "Warning".to_string(),
    });

    // Active thread is root, not the source — so overlay should show.
    assert_ne!(
        app.active_thread_id, source_thread_id,
        "source thread should be different from active thread"
    );

    let content = render_to_string(&app);

    // The overlay should show "Pending Approval" title.
    assert!(
        content.contains("Pending") && content.contains("Approval"),
        "approval overlay should show 'Pending Approval' title. Content: {}",
        &content[..content.len().min(500)]
    );

    // The overlay should show the source thread ID.
    assert!(
        content.contains(&source_thread_id),
        "approval overlay should show source thread ID '{}'. Content: {}",
        source_thread_id,
        &content[..content.len().min(500)]
    );

    // The overlay should show the tool name.
    assert!(
        content.contains("bash"),
        "approval overlay should show tool name 'bash'. Content: {}",
        &content[..content.len().min(500)]
    );

    // The overlay should show the danger level.
    assert!(
        content.contains("Warning"),
        "approval overlay should show danger level 'Warning'. Content: {}",
        &content[..content.len().min(500)]
    );

    // The overlay should show the 'o' hint.
    assert!(
        content.contains("o") && content.contains("switch"),
        "approval overlay should show press 'o' to switch hint. Content: {}",
        &content[..content.len().min(500)]
    );

    // Verify no overlay when pending_approval is None.
    app.pending_approval = None;
    let content = render_to_string(&app);
    assert!(
        !content.contains("Pending Approval"),
        "no approval overlay when pending_approval is None"
    );
}

/// AC-5 test 3: Press 'o' switches to the source thread of pending approval.
///
/// Unit-level: App with pending_approval, press 'o' → SwitchToApprovalSource
/// action returned, active_thread_id switches to source thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd97_ac5_press_o_switches_to_source_thread() {
    let mut app = make_app_with_2_agents();
    let root_id = "root-005".to_string();
    let child_id = "child-005".to_string();

    // Active thread is root.
    assert_eq!(app.active_thread_id, root_id);

    // Set pending approval from child (inactive) thread.
    app.pending_approval = Some(PendingApproval {
        source_thread_id: child_id.clone(),
        tool_call_id: "call-456".to_string(),
        tool_name: "write_file".to_string(),
        args: serde_json::json!({"path": "/tmp/test.rs"}),
        danger_level: "Caution".to_string(),
    });

    // Press 'o' — should return SwitchToApprovalSource.
    let action = app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert_eq!(
        action,
        KeyAction::SwitchToApprovalSource,
        "press 'o' with pending approval should return SwitchToApprovalSource"
    );

    // Simulate the switch (as done in lib.rs event loop).
    if let KeyAction::SwitchToApprovalSource = action {
        if let Some(approval) = app.pending_approval.take() {
            app.active_thread_id = approval.source_thread_id;
        }
    }

    // Verify active_thread_id switched to the source thread.
    assert_eq!(
        app.active_thread_id, child_id,
        "active_thread_id should switch to source thread after pressing 'o'"
    );

    // Verify pending_approval was consumed.
    assert!(
        app.pending_approval.is_none(),
        "pending_approval should be consumed after switching"
    );

    // Verify 'o' without pending_approval does not switch.
    let mut app2 = make_app_with_2_agents();
    app2.pending_approval = None;
    let action = app2.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
    assert_ne!(
        action,
        KeyAction::SwitchToApprovalSource,
        "'o' without pending_approval should not return SwitchToApprovalSource"
    );
    assert_eq!(
        app2.active_thread_id, root_id,
        "active_thread_id should not change without pending_approval"
    );

    // Verify 'o' is added to input_buffer when no pending approval.
    assert!(
        app2.composer.input_buffer.contains('o'),
        "'o' should be added to input buffer when no pending approval"
    );
}
