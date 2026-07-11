//! E2E tests for Sandbox Apply trong Agent Loop .
//!
//! AC-1: Landlock blocks write outside workspace (WorkspaceWrite policy).
//! AC-2: seccomp blocks network syscall (WorkspaceWrite policy).
//! AC-3: FullAccess — no sandbox, command succeeds.

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build a mock LLM response that calls bash with the given command,
/// followed by a final text response.
fn tool_call_response(command: &str) -> String {
    // Build the arguments JSON object: {"command":"<command>"}
    let args_obj = serde_json::json!({"command": command}).to_string();
    // Escape it as a JSON string value for the SSE arguments field.
    let args_str = serde_json::to_string(&args_obj).unwrap();
    let line1 = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"bash","arguments":""}}]}}]}"#.to_string();
    let line2 = format!(
        r#"data: {{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"function":{{"arguments":{args_str}}}}}]}}}}]}}"#,
        args_str = args_str
    );
    let line3 = r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    let done = "data: [DONE]";
    format!("{line1}\n\n{line2}\n\n{line3}\n\n{done}\n\n")
}

fn final_text_response(text: &str) -> String {
    let line = format!(
        r#"data: {{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#,
        text = text
    );
    format!("{line}\n\ndata: [DONE]\n\n")
}

/// AC-1: Landlock blocks write to /etc under WorkspaceWrite sandbox.
#[tokio::test]
async fn e2e_prd9_ac1_landlock_block_write() {
    let server = MockServer::start().await;

    // Use a unique filename to avoid conflicts.
    let block_path = "/etc/zerozero_sandbox_e2e_ac1_block";
    let cmd = format!("echo blocked > {block_path}");

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(tool_call_response(&cmd).as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(final_text_response("Done").as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "workspace-write")
        .env("ZZ_APPROVAL", "never")
        .args(["exec", "Write to etc"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    let tool_completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .expect("tool.completed present");
    let result = tool_completed["result"].as_str().expect("result is string");

    // The command should fail — Landlock blocks write to /etc.
    // Exit code should be non-zero, stderr should contain "Permission denied"
    // or similar EPERM error.
    assert!(
        !result.contains("exit_code: 0"),
        "write to /etc should fail under sandbox: {result}"
    );

    // The blocked file should NOT exist.
    assert!(
        !std::path::Path::new(block_path).exists(),
        "file should not have been created"
    );
}

/// AC-2: seccomp blocks network syscall under WorkspaceWrite sandbox.
#[tokio::test]
async fn e2e_prd9_ac2_seccomp_block_network() {
    let server = MockServer::start().await;

    // Use python3 to directly test socket() creation. If seccomp blocks
    // socket(), python3 gets PermissionError and exits non-zero without
    // printing SOCKET_OK. If seccomp doesn't block, python3 creates the
    // socket, prints SOCKET_OK, and exits 0.
    //
    // Previous version used `curl ... || true` which masked curl's exit
    // code (always 0) and checked for lowercase "failed" but curl outputs
    // "Failed" (capital F) — assertion always failed .
    let cmd = "python3 -c 'import socket; s=socket.socket(); s.close(); print(\"SOCKET_OK\")' 2>&1";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(tool_call_response(cmd).as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(final_text_response("Done").as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "workspace-write")
        .env("ZZ_APPROVAL", "never")
        .args(["exec", "Check network"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    let tool_completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .expect("tool.completed present");
    let result = tool_completed["result"].as_str().expect("result is string");

    // seccomp should block socket() — python3 should NOT print SOCKET_OK.
    // If seccomp is not active, python3 prints SOCKET_OK and exits 0.
    // Check for "SOCKET_OK" as a standalone output line, not inside a
    // traceback (the source line `print("SOCKET_OK")` appears in tracebacks).
    let socket_ok_printed = result.lines().any(|l| l.trim() == "SOCKET_OK");
    assert!(
        !socket_ok_printed,
        "socket() should be blocked by seccomp — SOCKET_OK should NOT appear as output: {result}"
    );
    // The command should fail (non-zero exit) because socket() returns EPERM.
    assert!(
        !result.contains("exit_code: 0"),
        "python3 should exit non-zero when seccomp blocks socket(): {result}"
    );
}

/// AC-3: FullAccess — no sandbox, echo command succeeds.
#[tokio::test]
async fn e2e_prd9_ac3_full_access_ok() {
    let server = MockServer::start().await;

    let cmd = "echo sandbox_full_access_ok";

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(tool_call_response(cmd).as_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(final_text_response("Done").as_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .env("ZZ_APPROVAL", "never")
        .args(["exec", "Echo test"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let s = String::from_utf8(output).expect("stdout is valid utf-8");
    let events: Vec<Value> = s
        .lines()
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .expect("every line is valid JSON");

    let tool_completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .expect("tool.completed present");
    let result = tool_completed["result"].as_str().expect("result is string");

    assert!(
        result.contains("sandbox_full_access_ok"),
        "echo should succeed under full access: {result}"
    );
    assert!(
        result.contains("exit_code: 0"),
        "exit code should be 0 under full access: {result}"
    );
}
