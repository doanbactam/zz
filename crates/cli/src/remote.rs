//! Remote TUI / app-server .
//!
//! Implements `zz serve` (long-lived agent engine reachable over TCP) and
//! `zz connect` (a thin remote TUI client), speaking a lightweight
//! line-delimited JSON protocol over plain TCP (one JSON frame per line).
//!
//! No new external dependencies are introduced: the protocol rides on
//! `tokio::net::{TcpListener, TcpStream}` and `serde_json`. This mirrors the
//! existing `zz exec` JSONL event stream, so the server simply re-emits the
//! `zerozero_exec::Event` values produced by `run_turn` as JSON lines.
//!
//! # Protocol
//!
//! Every frame is a single JSON object on its own line. Direction is implied
//! by the `type` tag:
//!
//! Server -> Client (`ServerFrame`):
//! ```jsonc
//! {"type":"hello","server":"zz-serve","version":"0.2.0"}
//! {"type":"event","event":{"type":"session.started","session_id":"..."}}
//! {"type":"done"}                 // session turn finished, awaiting input
//! {"type":"error","message":"..."}
//! ```
//!
//! Client -> Server (`ClientFrame`):
//! ```jsonc
//! {"type":"hello","client":"zz-connect"}
//! {"type":"input","text":"user typed this"}   // initial prompt / follow-up
//! {"type":"bye"}
//! ```
//!
//! The server runs the agent engine via `zerozero_core::run_turn` for the
//! initial prompt it receives from the client, forwarding every `Event`
//! through the `emit` closure as a `ServerFrame::Event` JSON line. The
//! connection stays open so further client `input` frames can drive
//! additional turns; the heavy loop runs on the server machine while the
//! interactive UI lives on the client.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use zerozero_llm::Provider;
use zerozero_tools::ToolRegistry;

use zerozero_core::run_turn;
use zerozero_exec::Event;

// ---------------------------------------------------------------------------
// Protocol frames
// ---------------------------------------------------------------------------

/// Frames sent from the server (agent engine) to the connected client.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ServerFrame {
    /// Greeting sent immediately after a client connects.
    #[serde(rename = "hello")]
    Hello { server: String, version: String },
    /// A single agent `Event`, serialized as a JSON value (carried opaquely
    /// so the wire shape matches `zz exec` JSONL verbatim).
    #[serde(rename = "event")]
    Event { event: serde_json::Value },
    /// The engine finished the current turn and is idle, awaiting input.
    #[serde(rename = "done")]
    Done,
    /// A fatal server-side error.
    #[serde(rename = "error")]
    Error { message: String },
    /// An approval request : the server paused on a tool call that
    /// needs user consent. The client shows an overlay and replies with a
    /// `ClientFrame::Approval`.
    #[serde(rename = "approve")]
    Approve {
        tool_call_id: String,
        tool_name: String,
        args: String,
        danger_level: String,
    },
}

/// Frames sent from the client (remote TUI) to the server.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum ClientFrame {
    /// Greeting sent immediately after connecting.
    #[serde(rename = "hello")]
    Hello { client: String },
    /// Free-form user input (initial prompt or a follow-up).
    #[serde(rename = "input")]
    Input { text: String },
    /// Client is disconnecting.
    #[serde(rename = "bye")]
    Bye,
    /// Reply to a server `approve` request : grant or deny consent.
    #[serde(rename = "approval")]
    Approval { approved: bool },
}

// ---------------------------------------------------------------------------
// Framing helpers (one JSON object per line)
// ---------------------------------------------------------------------------

/// Read a single newline-delimited JSON frame from `reader`.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<serde_json::Value>>
where
    R: AsyncReadExt + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = reader.read(&mut byte).await?;
        if n == 0 {
            // EOF: if we buffered a partial frame, parse it; else None.
            if buf.is_empty() {
                return Ok(None);
            }
            let line = String::from_utf8_lossy(&buf).to_string();
            return Ok(Some(serde_json::from_str(&line)?));
        }
        if byte[0] == b'\n' {
            if buf.is_empty() {
                continue; // skip stray blank lines
            }
            let line = String::from_utf8_lossy(&buf).to_string();
            return Ok(Some(serde_json::from_str(&line)?));
        }
        buf.push(byte[0]);
    }
}

/// Write a single JSON frame followed by a newline.
pub async fn write_frame<W>(writer: &mut W, value: &serde_json::Value) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let line = serde_json::to_string(value)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Start the agent engine as a long-lived service and accept a single TCP
/// client (`zz serve`).
///
/// Builds a provider + standard tool registry (reusing the same helpers as
/// `zz exec`), then delegates to [`run_serve_with`], which runs the engine
/// over a keep-alive connection so the client can drive multiple turns.
/// Returns the bound address so callers/tests can reach it.
pub async fn run_serve(
    port: u16,
    approval: zerozero_sandbox::ApprovalPolicy,
) -> Result<SocketAddr> {
    // Build the engine: provider + standard tool registry (full access
    // sandbox, isolated network namespace — same as `zz exec` defaults).
    let provider = crate::build_provider();
    let net_policy = Arc::new(zerozero_sandbox::NetPolicy::parse("none"));
    let sandbox = zerozero_sandbox::SandboxPolicy::FullAccess;
    let mut tools = zerozero_tools::ToolRegistry::standard_with_net(Arc::new(sandbox), net_policy);
    let _ = crate::register_external_plugins(&mut tools);
    run_serve_with(port, provider, tools, approval).await
}

/// Run the agent engine as a long-lived service bound to `port`, accepting a
/// single TCP client and driving a multi-turn session (`zz serve`,
/// fixed for follow-up turns in).
///
/// After each `run_turn` the server emits `Done`; further client `input`
/// frames drive additional turns until the client sends `Bye` or disconnects.
pub async fn run_serve_with(
    port: u16,
    provider: Box<dyn Provider>,
    tools: ToolRegistry,
    approval: zerozero_sandbox::ApprovalPolicy,
) -> Result<SocketAddr> {
    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr).await?;
    let local_addr = listener.local_addr()?;
    eprintln!("zz serve: listening on {addr} (single client, JSON-line protocol)");

    let (stream, peer) = listener.accept().await?;
    eprintln!("zz serve: client connected from {peer}");

    // Split so the read side can live in its own task and the write side can
    // be shared behind a mutex for event forwarding.
    let (mut read_half, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));

    let compaction_config = zerozero_compaction::CompactionConfig::default();
    let hooks: Box<dyn zerozero_core::LifecycleHooks> = Box::new(zerozero_core::NoopHooks);
    let effort = zerozero_llm::Effort::default();
    // the approval policy is caller-selected (`zz serve --approval`).
    // `OnAsk` prompts for every tool call (operator gates each one remotely);
    // `AutoEdit` only prompts for destructive commands. In all cases the
    // decision is awaited from the wire channel, never local stdin.
    let (approve_tx, approve_rx) = mpsc::unbounded_channel::<bool>();
    let approve_rx = Arc::new(tokio::sync::Mutex::new(approve_rx));

    // Input channel: client `input` frames flow to the turn loop. A `Bye`
    // frame (or EOF) ends the session; we signal that with an empty-string
    // sentinel so the loop can distinguish it from a normal empty prompt.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let writer_for_reader = Arc::clone(&writer);
    let approve_tx_for_reader = approve_tx.clone();

    // Greet the client.
    {
        let mut w = writer.lock().await;
        let hello = serde_json::to_value(ServerFrame::Hello {
            server: "zz-serve".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        })?;
        write_frame(&mut *w, &hello).await?;
    }

    // Reader task: consume client frames, forwarding prompts to the turn
    // loop. A `Bye` frame (or EOF / bad frame) ends the session.
    let reader_handle = tokio::spawn(async move {
        loop {
            match read_frame(&mut read_half).await {
                Ok(Some(value)) => match serde_json::from_value::<ClientFrame>(value) {
                    Ok(ClientFrame::Hello { .. }) => { /* ignore */ }
                    Ok(ClientFrame::Input { text }) => {
                        let _ = input_tx.send(text.trim().to_string());
                    }
                    // client approval decision feeds the wire channel
                    // that `run_turn` awaits instead of local stdin.
                    Ok(ClientFrame::Approval { approved }) => {
                        let _ = approve_tx_for_reader.send(approved);
                    }
                    Ok(ClientFrame::Bye) => {
                        let _ = input_tx.send(String::new());
                        break;
                    }
                    Err(e) => {
                        let mut w = writer_for_reader.lock().await;
                        let err = serde_json::to_value(ServerFrame::Error {
                            message: format!("bad client frame: {e}"),
                        })
                        .unwrap();
                        let _ = write_frame(&mut *w, &err).await;
                        let _ = input_tx.send(String::new());
                        break;
                    }
                },
                Ok(None) => {
                    let _ = input_tx.send(String::new());
                    break;
                }
                Err(e) => {
                    eprintln!("zz serve: client read error: {e}");
                    let _ = input_tx.send(String::new());
                    break;
                }
            }
        }
    });

    // Turn loop: run the engine for each prompt the client sends, forwarding
    // every `Event` back as a `ServerFrame::Event`, then signal `Done` so the
    // client can drive follow-up turns (multi-turn remote session).
    while let Some(prompt) = input_rx.recv().await {
        if prompt.is_empty() {
            break; // Bye / disconnect sentinel
        }
        if prompt.trim().is_empty() {
            continue;
        }

        // Channel so the `emit` closure can hand events to the turn-loop
        // task, which writes them in order. The closure just sends (cheap);
        // the loop awaits the writer so `Done` is always emitted AFTER the
        // last event (fixes a reordering bug where `Done` was written inline
        // before the spawned event tasks ran).
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();

        let emit = move |event: Event| {
            let _ = event_tx.send(event);
        };

        let result = run_turn(
            &prompt,
            None,
            &*provider,
            &tools,
            10,
            &zerozero_sandbox::SandboxPolicy::FullAccess,
            &approval,
            false,
            false,
            None,
            &compaction_config,
            &*hooks,
            None,
            &[],
            effort,
            &[],
            Some(approve_rx.clone()),
            emit,
        )
        .await;

        // Drain all events emitted during the turn, in order.
        while let Ok(event) = event_rx.try_recv() {
            // surface a tool-approval request to the remote client so
            // the human operator can consent over the wire.
            if let Event::ApprovalRequested {
                tool_call_id,
                tool_name,
                args,
                danger_level,
            } = &event
            {
                let approve = serde_json::to_value(ServerFrame::Approve {
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    args: args.to_string(),
                    danger_level: danger_level.clone(),
                })
                .unwrap_or(serde_json::Value::Null);
                let mut w = writer.lock().await;
                write_frame(&mut *w, &approve).await?;
            }
            let value = serde_json::to_value(ServerFrame::Event {
                event: serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
            })
            .unwrap_or(serde_json::Value::Null);
            let mut w = writer.lock().await;
            write_frame(&mut *w, &value).await?;
        }

        // Announce completion (or error) for this turn — always after events.
        {
            let mut w = writer.lock().await;
            if let Err(e) = &result {
                let err = serde_json::to_value(ServerFrame::Error {
                    message: e.to_string(),
                })?;
                write_frame(&mut *w, &err).await?;
            }
            let done = serde_json::to_value(ServerFrame::Done)?;
            write_frame(&mut *w, &done).await?;
        }
    }

    reader_handle.abort();
    eprintln!("zz serve: client disconnected, shutting down");
    Ok(local_addr)
}

// ---------------------------------------------------------------------------
// Client (remote TUI)
// ---------------------------------------------------------------------------

/// Connect to a `zz serve` instance and render its event stream in the full
/// interactive TUI (the same widget tree as the local `zz` TUI), sending user
/// input back as JSON `input` frames. This is the "TUI connect" path: the rich
/// UI is decoupled from the engine, which runs on the server ().
pub async fn run_connect(addr: String) -> Result<()> {
    use crossterm::event::{self as cevent, Event as CEvent, KeyCode};
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;
    use std::time::Duration;
    use zerozero_exec::Event as ExecEvent;
    use zerozero_tui::app::{App, KeyAction, PendingApproval, SlashAction};
    use zerozero_tui::ui;

    const HELP_TEXT: &str = "\
Slash commands:
  /help              Show this help text
  /clear             Clear the conversation
  /quit              Quit the TUI (same as 'q')";

    let stream = TcpStream::connect(&addr).await?;
    let (mut read_half, write_half) = stream.into_split();
    let writer = Arc::new(Mutex::new(write_half));

    // Display channel: parsed server frames flow to the UI loop.
    let (display_tx, mut display_rx) = mpsc::unbounded_channel::<ServerFrame>();
    // Input channel: UI loop -> dedicated writer task.
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<ClientFrame>();

    // Reader task: parse ServerFrames and forward them (parsed) to the UI.
    tokio::spawn(async move {
        loop {
            match read_frame(&mut read_half).await {
                Ok(Some(value)) => match serde_json::from_value::<ServerFrame>(value) {
                    Ok(frame) => {
                        let _ = display_tx.send(frame);
                    }
                    Err(e) => {
                        let _ = display_tx.send(ServerFrame::Error {
                            message: format!("[protocol error] {e}"),
                        });
                        break;
                    }
                },
                Ok(None) => {
                    let _ = display_tx.send(ServerFrame::Error {
                        message: "[disconnected]".to_string(),
                    });
                    break;
                }
                Err(e) => {
                    let _ = display_tx.send(ServerFrame::Error {
                        message: format!("[protocol error] {e}"),
                    });
                    break;
                }
            }
        }
    });

    // Writer task: drains the input channel, serializing ClientFrames.
    {
        let writer = Arc::clone(&writer);
        tokio::spawn(async move {
            while let Some(frame) = input_rx.recv().await {
                let value = serde_json::to_value(&frame).unwrap_or(serde_json::Value::Null);
                let mut w = writer.lock().await;
                if write_frame(&mut *w, &value).await.is_err() {
                    break;
                }
            }
        });
    }

    // Greet the server.
    let _ = input_tx.send(ClientFrame::Hello {
        client: "zz-connect".to_string(),
    });

    // Set up the terminal.
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    enable_raw_mode()?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.set_active_thread_id("root".to_string());
    let tid = app.active_thread_id.clone();

    let draw_result = (|| -> Result<()> {
        loop {
            // Drain any newly arrived server frames into the shared App state.
            while let Ok(frame) = display_rx.try_recv() {
                match frame {
                    ServerFrame::Hello { server, version } => {
                        app.messages.push(zerozero_llm::ChatMessage {
                            role: "system".to_string(),
                            content: format!("connected to {server} v{version}"),
                            tool_call_id: None,
                            tool_calls: None,
                            attachments: None,
                            thinking_signature: None,
                            redacted_thinking: None,
                            thinking: None,
                        });
                    }
                    ServerFrame::Event { event } => {
                        if let Ok(ev) = serde_json::from_value::<ExecEvent>(event) {
                            app.apply_chat_event(&tid, ev);
                        }
                    }
                    ServerFrame::Done => {
                        app.is_streaming = false;
                    }
                    ServerFrame::Error { message } => {
                        app.messages.push(zerozero_llm::ChatMessage {
                            role: "system".to_string(),
                            content: format!("[error] {message}"),
                            tool_call_id: None,
                            tool_calls: None,
                            attachments: None,
                            thinking_signature: None,
                            redacted_thinking: None,
                            thinking: None,
                        });
                    }
                    // remote approval. Store the request and let the
                    // operator consent via the existing overlay (y = allow,
                    // n = deny), which sends a `ClientFrame::Approval` back.
                    ServerFrame::Approve {
                        tool_call_id,
                        tool_name,
                        args,
                        danger_level,
                    } => {
                        app.pending_approval = Some(PendingApproval {
                            source_thread_id: tid.clone(),
                            tool_call_id,
                            tool_name,
                            args: serde_json::Value::String(args),
                            danger_level,
                        });
                    }
                }
            }

            // Render through the same widget tree as the local TUI.
            terminal.draw(|f| ui::render(f, &app))?;

            // Poll for key input (non-blocking with a short timeout).
            if cevent::poll(Duration::from_millis(80))? {
                if let CEvent::Key(key) = cevent::read()? {
                    // Windows emits Press+Release; only Press types characters.
                    if !key.is_press() {
                        continue;
                    }
                    // remote approval consent. While a tool-approval
                    // request is pending, y allows and n denies, sending the
                    // decision back over the wire (the engine awaits it).
                    if app.pending_approval.is_some() {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                let _ = input_tx.send(ClientFrame::Approval { approved: true });
                                app.messages.push(zerozero_llm::ChatMessage {
                                    role: "system".to_string(),
                                    content: "[approval] allowed by operator".to_string(),
                                    tool_call_id: None,
                                    tool_calls: None,
                                    attachments: None,
                                    thinking_signature: None,
                                    redacted_thinking: None,
                                    thinking: None,
                                });
                                app.pending_approval = None;
                                continue;
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') => {
                                let _ = input_tx.send(ClientFrame::Approval { approved: false });
                                app.messages.push(zerozero_llm::ChatMessage {
                                    role: "system".to_string(),
                                    content: "[approval] denied by operator".to_string(),
                                    tool_call_id: None,
                                    tool_calls: None,
                                    attachments: None,
                                    thinking_signature: None,
                                    redacted_thinking: None,
                                    thinking: None,
                                });
                                app.pending_approval = None;
                                continue;
                            }
                            _ => {}
                        }
                    }
                    match app.handle_key(key) {
                        KeyAction::Submit => {
                            let text = app.composer.input_buffer.clone();
                            if !text.trim().is_empty() {
                                app.messages.push(zerozero_llm::ChatMessage {
                                    role: "user".to_string(),
                                    content: text.clone(),
                                    tool_call_id: None,
                                    tool_calls: None,
                                    attachments: None,
                                    thinking_signature: None,
                                    redacted_thinking: None,
                                    thinking: None,
                                });
                                let _ = input_tx.send(ClientFrame::Input { text });
                            }
                            app.composer.input_buffer.clear();
                        }
                        KeyAction::Quit => break,
                        KeyAction::Slash(slash) => match slash {
                            SlashAction::ClearChat => app.messages.clear(),
                            SlashAction::ToggleDiff => app.show_diff = !app.show_diff,
                            SlashAction::ShowHelp => app.messages.push(zerozero_llm::ChatMessage {
                                role: "system".to_string(),
                                content: HELP_TEXT.to_string(),
                                tool_call_id: None,
                                tool_calls: None,
                                attachments: None,
                                thinking_signature: None,
                                redacted_thinking: None,
                                thinking: None,
                            }),
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }

            if app.should_quit {
                break;
            }
        }
        Ok(())
    })();

    // Restore the terminal regardless of how we exit.
    disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if app.should_quit {
        let _ = input_tx.send(ClientFrame::Bye);
    }

    draw_result
}

// ---------------------------------------------------------------------------
// Tests — pure protocol parsing (no live network) + one ephemeral exchange.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use zerozero_llm::{ChatMessage, DeltaStream, Effort, Provider, SseEvent, SseEventStream};
    use zerozero_sandbox::SandboxPolicy;
    use zerozero_tools::ToolRegistry;

    #[test]
    fn test_server_event_frame_roundtrip() {
        // A representative `zerozero_exec::Event` serialized exactly as `zz exec` would.
        let event = serde_json::json!({
            "type": "item.completed",
            "item": {"id": "item_0", "type": "agent_message", "text": "echo: hi"}
        });
        let frame = ServerFrame::Event {
            event: event.clone(),
        };
        let serialized = serde_json::to_string(&frame).expect("serialize");
        let parsed: ServerFrame = serde_json::from_str(&serialized).expect("deserialize");
        match parsed {
            ServerFrame::Event { event: e } => assert_eq!(e, event),
            other => panic!("expected Event variant, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_client_input_frame() {
        let frame: ClientFrame = serde_json::from_str(r#"{"type":"input","text":"hello world"}"#)
            .expect("parse input frame");
        match frame {
            ClientFrame::Input { text } => assert_eq!(text, "hello world"),
            other => panic!("expected Input variant, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_client_hello_frame() {
        let frame: ClientFrame =
            serde_json::from_str(r#"{"type":"hello","client":"zz-connect"}"#).expect("parse hello");
        assert!(matches!(frame, ClientFrame::Hello { client } if client == "zz-connect"));
    }

    #[test]
    fn test_parse_server_hello_done_error() {
        let hello: ServerFrame =
            serde_json::from_str(r#"{"type":"hello","server":"zz-serve","version":"0.2.0"}"#)
                .expect("hello");
        assert!(matches!(hello, ServerFrame::Hello { .. }));
        let done: ServerFrame = serde_json::from_str(r#"{"type":"done"}"#).expect("done");
        assert!(matches!(done, ServerFrame::Done));
        let err: ServerFrame =
            serde_json::from_str(r#"{"type":"error","message":"boom"}"#).expect("error");
        assert!(matches!(err, ServerFrame::Error { message } if message == "boom"));
    }

    // Live exchange on an ephemeral port. No LLM involved — we stand up our
    // own minimal server that emits one event and reads one input, exercising
    // the real framing helpers end-to-end.
    #[tokio::test]
    async fn test_serve_connect_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Minimal stand-in server: emit one event, then read one input frame.
        let server = tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let (mut read_half, mut write_half) = stream.into_split();

            let hello = serde_json::to_value(ServerFrame::Hello {
                server: "zz-serve".into(),
                version: "0.2.0".into(),
            })
            .unwrap();
            write_frame(&mut write_half, &hello).await.unwrap();

            let event = serde_json::json!({"type": "session.started", "session_id": "s1"});
            let frame = serde_json::to_value(ServerFrame::Event { event }).unwrap();
            write_frame(&mut write_half, &frame).await.unwrap();

            // Read the client's input frame.
            let value = read_frame(&mut read_half).await.unwrap().unwrap();
            let parsed: ClientFrame = serde_json::from_value(value).unwrap();
            assert!(matches!(parsed, ClientFrame::Input { text } if text == "ping"));
        });

        // Client side.
        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();

        let hello = read_frame(&mut read_half).await.unwrap().unwrap();
        let hello: ServerFrame = serde_json::from_value(hello).unwrap();
        assert!(matches!(hello, ServerFrame::Hello { .. }));

        let event_line = read_frame(&mut read_half).await.unwrap().unwrap();
        let event_frame: ServerFrame = serde_json::from_value(event_line).unwrap();
        match event_frame {
            ServerFrame::Event { event } => {
                assert_eq!(event["type"], "session.started");
                assert_eq!(event["session_id"], "s1");
            }
            other => panic!("expected event, got {other:?}"),
        }

        let input = serde_json::to_value(ClientFrame::Input {
            text: "ping".into(),
        })
        .unwrap();
        write_frame(&mut write_half, &input).await.unwrap();

        server.await.unwrap();
    }

    /// Fake provider: one assistant message per turn, no tool calls. Lets us
    /// drive the server's multi-turn loop without any real LLM / network.
    struct FakeProvider;
    #[async_trait::async_trait]
    impl Provider for FakeProvider {
        async fn chat_stream(&self, _prompt: &str) -> anyhow::Result<DeltaStream> {
            unreachable!("run_turn uses chat_with_tools")
        }
        async fn chat_with_tools(
            &self,
            _messages: &[ChatMessage],
            _tools: &[serde_json::Value],
            _effort: Effort,
            _images: &[String],
        ) -> anyhow::Result<SseEventStream> {
            let events: Vec<SseEvent> = vec![SseEvent::Content("echo".to_string()), SseEvent::Done];
            Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
        }
    }

    // the server must drive FOLLOW-UP turns, not just the first.
    // Connects, sends two prompts, and asserts a `Done` is emitted after each
    // (proving the turn loop kept running instead of idling on the first).
    #[tokio::test]
    async fn test_serve_multiturn_drives_followup() {
        // Discover a free port, then serve on it (small localhost race is OK).
        let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);

        let provider: Box<dyn Provider> = Box::new(FakeProvider);
        let tools = ToolRegistry::standard(Arc::new(SandboxPolicy::FullAccess));

        // `run_turn` holds a !Send SQLite connection (RefCell), so the server
        // must run on a current-thread runtime via `spawn_local` (same as
        // production `zz serve`, which runs it inline in `main`). We spawn it
        // WITHOUT awaiting, then drive the client inside the same `run_until`
        // future so both run concurrently on the LocalSet.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                tokio::task::spawn_local(async move {
                    let _ = run_serve_with(
                        addr.port(),
                        provider,
                        tools,
                        zerozero_sandbox::ApprovalPolicy::OnAsk,
                    )
                    .await;
                });

                // Let the server bind + accept.
                tokio::time::sleep(Duration::from_millis(150)).await;

                let stream = TcpStream::connect(addr).await.unwrap();
                let (mut read_half, mut write_half) = stream.into_split();

                let hello = serde_json::to_value(ClientFrame::Hello {
                    client: "zz-connect".into(),
                })
                .unwrap();
                write_frame(&mut write_half, &hello).await.unwrap();

                // Turn 1.
                let input1 = serde_json::to_value(ClientFrame::Input {
                    text: "turn one".into(),
                })
                .unwrap();
                write_frame(&mut write_half, &input1).await.unwrap();

                let mut saw_event = false;
                let mut saw_done1 = false;
                for _ in 0..100 {
                    let value = read_frame(&mut read_half).await.unwrap().unwrap();
                    match serde_json::from_value::<ServerFrame>(value).unwrap() {
                        ServerFrame::Event { .. } => saw_event = true,
                        ServerFrame::Done => {
                            saw_done1 = true;
                            break;
                        }
                        ServerFrame::Error { message } => panic!("server error: {message}"),
                        _ => {}
                    }
                }
                assert!(saw_event, "server should emit events for turn one");
                assert!(saw_done1, "server should emit Done after turn one");

                // Turn 2 (follow-up) — multi-turn must keep running.
                let input2 = serde_json::to_value(ClientFrame::Input {
                    text: "turn two".into(),
                })
                .unwrap();
                write_frame(&mut write_half, &input2).await.unwrap();

                let mut saw_done2 = false;
                for _ in 0..100 {
                    let value = read_frame(&mut read_half).await.unwrap().unwrap();
                    match serde_json::from_value::<ServerFrame>(value).unwrap() {
                        ServerFrame::Done => {
                            saw_done2 = true;
                            break;
                        }
                        ServerFrame::Error { message } => panic!("server error: {message}"),
                        _ => {}
                    }
                }
                assert!(
                    saw_done2,
                    "server should accept a follow-up turn (multi-turn)"
                );

                let bye = serde_json::to_value(ClientFrame::Bye).unwrap();
                write_frame(&mut write_half, &bye).await.unwrap();
                // Server task ends on Bye; LocalSet is dropped when this future returns.
            })
            .await;
    }

    // the new `approve`/`approval` frames round-trip over the wire
    // (forward-compatible for remote approval).
    #[test]
    fn test_approve_approval_frames_roundtrip() {
        let approve = ServerFrame::Approve {
            tool_call_id: "tc_1".into(),
            tool_name: "bash".into(),
            args: "rm -rf /".into(),
            danger_level: "danger".into(),
        };
        let json = serde_json::to_value(&approve).unwrap();
        assert_eq!(json["type"], "approve");
        let back: ServerFrame = serde_json::from_value(json).unwrap();
        match back {
            ServerFrame::Approve {
                tool_call_id,
                tool_name,
                args,
                danger_level,
            } => {
                assert_eq!(tool_call_id, "tc_1");
                assert_eq!(tool_name, "bash");
                assert_eq!(args, "rm -rf /");
                assert_eq!(danger_level, "danger");
            }
            _ => panic!("approve frame did not round-trip"),
        }

        let approval = ClientFrame::Approval { approved: true };
        let json = serde_json::to_value(&approval).unwrap();
        assert_eq!(json["type"], "approval");
        match serde_json::from_value::<ClientFrame>(json).unwrap() {
            ClientFrame::Approval { approved } => assert!(approved),
            _ => panic!("approval frame did not round-trip"),
        }
    }

    // the exact path `run_connect` uses to wire the UI to the engine —
    // a server `Event` is applied to the shared `App` and rendered through the
    // same widget tree as the local TUI. This proves the remote TUI is a real
    // chat view, not a JSON log unit test on the real function).
    #[test]
    fn test_connect_applies_events_to_app_and_renders() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use zerozero_exec::{Event, Item, ItemKind, ItemStarted, ItemUpdateKind, ItemUpdated};
        use zerozero_llm::ChatMessage;
        use zerozero_tui::app::App;
        use zerozero_tui::ui;

        let mut app = App::new();
        app.set_active_thread_id("root".into());
        let tid = app.active_thread_id.clone();

        // User message is echoed locally by the client on Submit.
        app.messages.push(ChatMessage {
            role: "user".into(),
            content: "hello".into(),
            tool_call_id: None,
            tool_calls: None,
            attachments: None,
            thinking_signature: None,
            redacted_thinking: None,
            thinking: None,
        });

        // Stream an assistant message token-by-token via server Events.
        app.apply_chat_event(
            &tid,
            Event::ItemStarted {
                item: ItemStarted {
                    id: "i1".into(),
                    kind: ItemKind::AgentMessage,
                },
            },
        );
        app.apply_chat_event(
            &tid,
            Event::ItemUpdated {
                item: ItemUpdated {
                    id: "i1".into(),
                    text: "Hi ".into(),
                    kind: ItemUpdateKind::Message,
                },
            },
        );
        app.apply_chat_event(
            &tid,
            Event::ItemUpdated {
                item: ItemUpdated {
                    id: "i1".into(),
                    text: "there".into(),
                    kind: ItemUpdateKind::Message,
                },
            },
        );
        app.apply_chat_event(
            &tid,
            Event::ItemCompleted {
                item: Item {
                    id: "i1".into(),
                    kind: ItemKind::AgentMessage,
                    text: "Hi there".into(),
                },
            },
        );
        // The server emits turn.completed after the item — this is what clears
        // the streaming flag in the UI (mirrors local run_app).
        app.apply_chat_event(&tid, Event::TurnCompleted);

        assert!(
            app.messages
                .iter()
                .any(|m| m.role == "assistant" && m.content.contains("Hi there")),
            "assistant message must be recorded in chat history"
        );
        assert!(
            !app.is_streaming,
            "streaming flag cleared after turn completion"
        );

        // Render the same tree `run_connect` uses — must not panic and must
        // produce output containing the assistant text.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| ui::render(f, &app)).unwrap();
        let rendered = terminal.backend().buffer().clone();
        let text: String = rendered
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("Hi there"),
            "rendered TUI must show the assistant message, got: {text}"
        );
    }

    // the remote-approval wire contract. The server emits a tool
    // `Approve` request; the client decision (`ClientFrame::Approval`) must
    // round-trip back and be delivered to the server's approval channel —
    // exactly the path `run_turn` awaits instead of local stdin.
    #[tokio::test]
    async fn test_remote_approval_wire_contract() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Mock server: emit `Approve`, then read the client's `Approval` reply.
        let server = tokio::spawn(async move {
            let (stream, _peer) = listener.accept().await.unwrap();
            let (mut read_half, mut write_half) = stream.into_split();

            // Greet, then surface a tool-approval request (as the real server
            // does from `Event::ApprovalRequested`).
            let hello = serde_json::to_value(ServerFrame::Hello {
                server: "zz-serve".into(),
                version: "0.2.0".into(),
            })
            .unwrap();
            write_frame(&mut write_half, &hello).await.unwrap();

            let approve = serde_json::to_value(ServerFrame::Approve {
                tool_call_id: "tc_7".into(),
                tool_name: "bash".into(),
                args: "rm -rf /tmp/x".into(),
                danger_level: "danger".into(),
            })
            .unwrap();
            write_frame(&mut write_half, &approve).await.unwrap();

            // The client operator consents (y -> approved:true).
            let value = read_frame(&mut read_half).await.unwrap().unwrap();
            let parsed: ClientFrame = serde_json::from_value(value).unwrap();
            assert!(
                matches!(parsed, ClientFrame::Approval { approved } if approved),
                "client must send its approval decision over the wire"
            );
        });

        // Client side: read `Approve`, reply `Approval { approved: true }`.
        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();

        let hello = read_frame(&mut read_half).await.unwrap().unwrap();
        assert!(matches!(
            serde_json::from_value::<ServerFrame>(hello).unwrap(),
            ServerFrame::Hello { .. }
        ));

        let approve_frame = read_frame(&mut read_half).await.unwrap().unwrap();
        match serde_json::from_value::<ServerFrame>(approve_frame).unwrap() {
            ServerFrame::Approve {
                tool_call_id,
                tool_name,
                args,
                danger_level,
            } => {
                assert_eq!(tool_call_id, "tc_7");
                assert_eq!(tool_name, "bash");
                assert_eq!(args, "rm -rf /tmp/x");
                assert_eq!(danger_level, "danger");
            }
            other => panic!("expected approve frame, got {other:?}"),
        }

        let reply = serde_json::to_value(ClientFrame::Approval { approved: true }).unwrap();
        write_frame(&mut write_half, &reply).await.unwrap();

        server.await.unwrap();
    }

    // the `zz serve --approval` flag parses into the right
    // `ApprovalPolicy` (default `on-ask`, or `auto-edit`). Pure CLI parse test.
    #[test]
    fn test_serve_approval_flag_parses() {
        use crate::Cli;
        use crate::Command;
        use clap::Parser;

        // Default => OnAsk.
        let cli = Cli::try_parse_from(["zz", "serve"]).expect("parse");
        if let Some(Command::Serve(args)) = cli.command {
            assert_eq!(
                <crate::ServeApproval as std::convert::Into<zerozero_sandbox::ApprovalPolicy>>::into(args.approval),
                zerozero_sandbox::ApprovalPolicy::OnAsk
            );
        } else {
            panic!("expected Serve command");
        }

        // Explicit on-ask.
        let cli = Cli::try_parse_from(["zz", "serve", "--approval", "on-ask"]).expect("parse");
        if let Some(Command::Serve(args)) = cli.command {
            assert_eq!(
                <crate::ServeApproval as std::convert::Into<zerozero_sandbox::ApprovalPolicy>>::into(args.approval),
                zerozero_sandbox::ApprovalPolicy::OnAsk
            );
        } else {
            panic!("expected Serve command");
        }

        // auto-edit => AutoEdit.
        let cli = Cli::try_parse_from(["zz", "serve", "--approval", "auto-edit"]).expect("parse");
        if let Some(Command::Serve(args)) = cli.command {
            assert_eq!(
                <crate::ServeApproval as std::convert::Into<zerozero_sandbox::ApprovalPolicy>>::into(args.approval),
                zerozero_sandbox::ApprovalPolicy::AutoEdit
            );
        } else {
            panic!("expected Serve command");
        }

        // Port still works alongside the new flag.
        let cli = Cli::try_parse_from(["zz", "serve", "--port", "9000", "--approval", "auto-edit"])
            .expect("parse");
        if let Some(Command::Serve(args)) = cli.command {
            assert_eq!(args.port, 9000);
            assert_eq!(
                <crate::ServeApproval as std::convert::Into<zerozero_sandbox::ApprovalPolicy>>::into(args.approval),
                zerozero_sandbox::ApprovalPolicy::AutoEdit
            );
        } else {
            panic!("expected Serve command");
        }
    }

    ///136: `zz jobs` parses to list mode, `zz jobs show <id>` to
    // the Show subcommand, and `zz exec --background` flips the flag.
    // Pure CLI parse test.
    #[test]
    fn test_jobs_and_exec_background_flag_parse() {
        use crate::Cli;
        use crate::Command;
        use crate::JobsCmd;
        use clap::Parser;
        // `zz jobs` => list mode (no subcommand).
        let cli = Cli::try_parse_from(["zz", "jobs"]).expect("parse");
        if let Some(Command::Jobs(args)) = cli.command {
            assert!(args.cmd.is_none(), "zz jobs with no subcommand lists all");
        } else {
            panic!("expected Jobs command");
        }
        // `zz jobs show <id>` => Show subcommand.
        let cli = Cli::try_parse_from(["zz", "jobs", "show", "job-abc"]).expect("parse");
        if let Some(Command::Jobs(args)) = cli.command {
            if let Some(JobsCmd::Show { id }) = args.cmd {
                assert_eq!(id, "job-abc");
            } else {
                panic!("expected JobsCmd::Show");
            }
        } else {
            panic!("expected Jobs command");
        }
        // `zz jobs log <id>` => Log subcommand.
        let cli = Cli::try_parse_from(["zz", "jobs", "log", "job-xyz"]).expect("parse");
        if let Some(Command::Jobs(args)) = cli.command {
            match args.cmd {
                Some(JobsCmd::Log { id }) => assert_eq!(id, "job-xyz"),
                _ => panic!("expected JobsCmd::Log"),
            }
        } else {
            panic!("expected Jobs command");
        }
        // `zz jobs clear` => Clear subcommand.
        let cli = Cli::try_parse_from(["zz", "jobs", "clear"]).expect("parse");
        if let Some(Command::Jobs(args)) = cli.command {
            assert!(
                matches!(args.cmd, Some(JobsCmd::Clear)),
                "expected JobsCmd::Clear"
            );
        } else {
            panic!("expected Jobs command");
        }
        // `zz exec --background` flips the flag.
        let cli = Cli::try_parse_from(["zz", "exec", "do x", "--background"]).expect("parse");
        if let Some(Command::Exec(args)) = cli.command {
            assert!(args.background, "exec --background should be true");
        } else {
            panic!("expected Exec command");
        }
        // Without --background it stays false (back-compat).
        let cli = Cli::try_parse_from(["zz", "exec", "do x"]).expect("parse");
        if let Some(Command::Exec(args)) = cli.command {
            assert!(
                !args.background,
                "exec without --background should be false"
            );
        } else {
            panic!("expected Exec command");
        }
    }
}
