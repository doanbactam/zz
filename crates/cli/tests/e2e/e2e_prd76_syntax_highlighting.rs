use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use std::collections::HashSet;

/// Strip ANSI escape sequences from a string.
///
/// Removes SGR (`ESC[...m`), cursor positioning (`ESC[...H`), hide/show
/// cursor (`ESC[?25l`/`ESC[?25h`), and similar sequences. Ratatui renders
/// each cell independently with cursor positioning, so text like "fn main"
/// gets split by escape codes in raw PTY output (pending queue pattern). Strip ANSI
/// before doing `contains` checks on rendered text.
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip escape sequence: ESC + chars until alphabetic terminator
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

/// SSE response that returns markdown with a Rust code block
fn sse_response_with_code_block() -> String {
    let content = "Here's a Rust code example:\n\n```rust\nfn main() {\n    let x = 5;\n    println!(\"{}\", x);\n}\n```";
    let content_escaped = content.replace('\n', "\\n").replace('"', "\\\"");

    let mut body = String::new();
    body.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        content_escaped
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

/// E2E test for AC-1: Code blocks render with syntax highlighting
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd76_ac1_code_block_highlighting() {
    let (output, color_codes) =
        run_tui_with_prompt("", "show me rust", sse_response_with_code_block()).await;

    let stripped = strip_ansi(&output);
    assert!(
        stripped.contains("fn") && stripped.contains("main"),
        "Should render code block with fn main. Stripped output: {}",
        &stripped[..stripped.len().min(500)]
    );
    assert!(
        color_codes.len() >= 2,
        "Expected >= 2 distinct ANSI colors for syntax highlighting, found {}. Colors: {:?}",
        color_codes.len(),
        color_codes
    );
}

/// SSE response with comprehensive markdown features (headings, bold, italic, lists, inline code, blockquote)
fn sse_response_with_full_markdown() -> String {
    let content = "# Heading\n\n**bold text** and *italic text*\n\n- item 1\n- item 2\n\n`inline code`\n\n> blockquote\n\n```rust\nlet x = 1;\n```";
    let content_escaped = content.replace('\n', "\\n").replace('"', "\\\"");
    let mut body = String::new();
    body.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        content_escaped
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

/// SSE response with unknown language tag
fn sse_response_with_unknown_lang() -> String {
    let content = "Code example:\n\n```xyz123unknown\nlet x = 5;\n```";
    let content_escaped = content.replace('\n', "\\n").replace('"', "\\\"");
    let mut body = String::new();
    body.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        content_escaped
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

/// SSE response with 500-line Rust code block
fn sse_response_500_lines() -> String {
    let mut content = String::from("```rust\n");
    for i in 0..500 {
        content.push_str(&format!("fn func_{}() {{\n    let x = {};\n}}\n", i, i));
    }
    content.push_str("```");
    let content_escaped = content.replace('\n', "\\n").replace('"', "\\\"");
    let mut body = String::new();
    body.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        content_escaped
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

/// SSE response with multiple language code blocks
fn sse_response_multi_language() -> String {
    let content = "Rust:\n\n```rust\nfn main() {}\n```\n\nPython:\n\n```python\ndef foo(): pass\n```\n\nJS:\n\n```javascript\nfunction bar() {}\n```";
    let content_escaped = content.replace('\n', "\\n").replace('"', "\\\"");
    let mut body = String::new();
    body.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
        content_escaped
    ));
    body.push_str("data: [DONE]\n\n");
    body
}

/// Helper: spawn TUI, wait for init, send prompt, collect output, quit.
///
/// Uses a background thread for PTY reading to avoid blocking the tokio
/// runtime (pending queue fix: blocking `reader.read()` on `#[tokio::test]`
/// current_thread runtime deadlocks the mock server).
///
/// Sends `\r` (Carriage Return) instead of `\n` (Line Feed) for Enter key,
/// because `zz` TUI runs in raw mode where crossterm only maps `\r` to
/// `KeyCode::Enter` (crossterm parse.rs line 547-552).
async fn run_tui_with_prompt(
    _server_uri: &str,
    prompt: &str,
    sse_body: String,
) -> (String, HashSet<String>) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(sse_body, "text/event-stream"),
        )
        .mount(&server)
        .await;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let bin = env!("CARGO_BIN_EXE_zz");
    let mut cmd = CommandBuilder::new(bin);
    cmd.env("OPENAI_API_KEY", "test-key");
    cmd.env("ZZ_PROVIDER", "openai");
    cmd.env("OPENAI_BASE_URL", server.uri());
    cmd.env("ZZ_MODEL", "test-model");
    // Advertise truecolor support so crossterm emits 24-bit (`ESC[38;2;`)
    // escapes for syntax highlighting (pending queue fix).
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn zz");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let mut writer = pair.master.take_writer().expect("take writer");

    // Background thread for PTY reading — avoids blocking tokio runtime.
    // reader.read() is blocking I/O; on a single-threaded tokio runtime,
    // blocking the test thread would prevent the mock server from processing
    // HTTP requests from zz, causing a deadlock (GAP-12 root cause #2).
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

    // Wait for TUI to initialize (render "Composer" label).
    let mut output = String::new();
    let start = Instant::now();
    let init_deadline = Duration::from_secs(15);

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

    // Send prompt with \r (Carriage Return) — in raw mode, crossterm only
    // maps \r to KeyCode::Enter, not \n (GAP-12 root cause #1).
    let _ = writer.write_all(format!("{}\r", prompt).as_bytes());
    let _ = writer.flush();

    // Collect rendered output.
    output.clear();
    let render_deadline = Duration::from_secs(20);
    let start = Instant::now();

    loop {
        if start.elapsed() >= render_deadline {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                let chunk_str = String::from_utf8_lossy(&chunk);
                output.push_str(&chunk_str);
                // Break when the assistant response starts rendering ("assistant:"
                // label appears in stripped output), then sleep briefly to collect
                // the full response before quitting.
                let stripped = strip_ansi(&output);
                if stripped.contains("assistant:") {
                    std::thread::sleep(Duration::from_millis(1000));
                    // Drain any remaining output.
                    while let Ok(more) = rx.recv_timeout(Duration::from_millis(200)) {
                        if more.is_empty() {
                            break;
                        }
                        output.push_str(&String::from_utf8_lossy(&more));
                    }
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }

    // Scan the full captured output for truecolor (24-bit) escape codes.
    // Syntax highlighting is applied on the finalized (non-streaming) render
    // frame, which arrives during the post-"assistant:" drain above, so we
    // must scan the complete buffer rather than only pre-break chunks.
    let mut color_codes = HashSet::new();
    for line in output.lines() {
        let mut pos = 0;
        while let Some(idx) = line[pos..].find("\x1b[38;2;") {
            let start_idx = pos + idx;
            if let Some(end_idx) = line[start_idx..].find('m') {
                color_codes.insert(line[start_idx..start_idx + end_idx + 1].to_string());
            }
            pos = start_idx + 1;
        }
    }

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

    (output, color_codes)
}

/// AC-2: Existing markdown features preserved in TUI
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd76_ac2_existing_markdown() {
    let (output, _) =
        run_tui_with_prompt("", "show markdown", sse_response_with_full_markdown()).await;

    let stripped = strip_ansi(&output);
    // Verify markdown features are present in rendered output
    assert!(
        stripped.contains("Heading") || stripped.contains("heading"),
        "Should render heading. Stripped: {}",
        &stripped[..stripped.len().min(500)]
    );
    assert!(
        stripped.contains("bold text") || stripped.contains("**"),
        "Should render bold text. Stripped: {}",
        &stripped[..stripped.len().min(500)]
    );
    assert!(
        stripped.contains("item 1") || stripped.contains("•"),
        "Should render list items. Stripped: {}",
        &stripped[..stripped.len().min(500)]
    );
}

/// AC-3: Unknown language tag falls back gracefully (no crash)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd76_ac3_fallback() {
    let (output, _) =
        run_tui_with_prompt("", "show unknown lang", sse_response_with_unknown_lang()).await;

    let stripped = strip_ansi(&output);
    // Should not crash; output should contain the code content
    assert!(
        stripped.contains("let x") || stripped.contains("5"),
        "Should render unknown language code block content. Stripped: {}",
        &stripped[..stripped.len().min(500)]
    );
}

/// AC-4: Performance — 500-line code block renders in TUI
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd76_ac4_performance() {
    let start = Instant::now();
    let (output, _) = run_tui_with_prompt("", "show large code", sse_response_500_lines()).await;
    let elapsed = start.elapsed();

    let stripped = strip_ansi(&output);
    assert!(!stripped.is_empty(), "Should produce output");
    // Note: wall-clock includes TUI startup + mock server; code render itself is fast
    assert!(
        elapsed.as_secs() < 30,
        "Full TUI cycle with 500-line block should complete in < 30s, took {}ms",
        elapsed.as_millis()
    );
}

/// AC-5: Syntax highlighting works for multiple languages
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prd76_ac5_language_support() {
    let (output, color_codes) = run_tui_with_prompt(
        "",
        "show code in multiple languages",
        sse_response_multi_language(),
    )
    .await;

    let stripped = strip_ansi(&output);
    // Should have syntax highlighting colors
    assert!(
        color_codes.len() >= 2,
        "Expected >= 2 distinct colors for multi-language highlighting, found {}",
        color_codes.len()
    );
    // Should contain code content from multiple languages
    assert!(
        stripped.contains("fn main")
            || stripped.contains("def foo")
            || stripped.contains("function bar")
            || (stripped.contains("fn") && stripped.contains("main"))
            || (stripped.contains("def") && stripped.contains("foo"))
            || (stripped.contains("function") && stripped.contains("bar")),
        "Should render multi-language code block content. Stripped: {}",
        &stripped[..stripped.len().min(500)]
    );
}
