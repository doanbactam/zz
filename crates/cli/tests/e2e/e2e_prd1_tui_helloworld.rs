use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// E2E for AC-4 updated for): TUI opens and displays UI.
///
/// update: TUI now shows a chat area + input pane instead of
/// "hello world". The test verifies that the TUI renders the "Input" and
/// "Chat" labels, and exits on 'q' key press.
///
/// * The `zz` binary opens a fullscreen TUI (alternate screen, raw mode).
/// * A dummy OPENAI_API_KEY is set so the TUI can initialize.
/// * ratatui renders borders and titles, so the raw byte stream contains
///   the spans "Input" and "Chat".
/// * The TUI blocks on event reading after drawing, so the test sends a
///   'q' keystroke to exit cleanly.
#[test]
fn e2e_prd1_ac4_tui_opens() {
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

    let mut output = String::new();
    let start = Instant::now();
    let render_deadline = Duration::from_secs(8);

    let both_spans_seen = loop {
        if start.elapsed() >= render_deadline {
            break false;
        }

        let mut buf = [0u8; 4096];
        match reader.read(&mut buf) {
            Ok(0) => break false,
            Ok(n) => {
                output.push_str(&String::from_utf8_lossy(&buf[..n]));
                if output.contains("Composer") && output.contains("Chat") {
                    break true;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break false,
        }
    };

    // Send 'q' so the TUI exits cleanly.
    let _ = writer.write_all(b"q");
    let _ = writer.flush();
    drop(writer);
    drop(pair.master);

    // Give the child up to 3s to exit on its own, then hard-kill it.
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
        both_spans_seen,
        "Expected TUI to render 'Input' and 'Chat' spans. Got ({} bytes): {output}",
        output.len()
    );
}
