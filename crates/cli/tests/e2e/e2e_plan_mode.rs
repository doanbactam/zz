//! E2E tests plan mode blocks mutating tools, allows read-only.
//!
//! full-access sandbox isolates plan mode from the sandbox layer: if plan
//! mode fails to block, the file WOULD be created (sandbox allows it), so the
//! assertion catches the regression.

use assert_cmd::Command;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tool_call_sse(tool_name: &str, args: Value) -> String {
    let args_str = args.to_string();
    let chunk = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": { "name": tool_name, "arguments": args_str }
                }]
            }
        }]
    })
    .to_string();
    format!(
        "data: {chunk}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
    )
}

fn final_text_sse(text: &str) -> String {
    let chunk = json!({ "choices": [{ "delta": { "content": text } }] }).to_string();
    format!("data: {chunk}\n\ndata: [DONE]\n\n")
}

async fn mock_two_turns(server: &MockServer, first: String, second: String) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(first.into_bytes(), "text/event-stream"),
        )
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Type", "text/event-stream")
                .set_body_raw(second.into_bytes(), "text/event-stream"),
        )
        .mount(server)
        .await;
}

fn parse_events(stdout: Vec<u8>) -> Vec<Value> {
    let s = String::from_utf8(stdout).expect("stdout is valid utf-8");
    s.lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test]
async fn e2e_prd28_ac1_plan_blocks_write() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foo.txt");
    let first = tool_call_sse(
        "write_file",
        json!({ "path": target.to_str().unwrap(), "content": "hi" }),
    );
    mock_two_turns(&server, first, final_text_sse("Here is the plan.")).await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--plan", "tao file foo.txt"])
        .assert()
        .success();

    assert!(!target.exists(), "plan mode must not create the file");
}

#[tokio::test]
async fn e2e_prd28_ac2_deny_message_plan_mode() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foo.txt");
    let first = tool_call_sse(
        "write_file",
        json!({ "path": target.to_str().unwrap(), "content": "hi" }),
    );
    mock_two_turns(&server, first, final_text_sse("Plan ready.")).await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--plan", "tao file foo.txt"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let events = parse_events(output);
    let completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed" && e["tool_name"] == "write_file")
        .expect("tool.completed for write_file present");
    let result = completed["result"].as_str().unwrap();
    assert!(
        result.contains("plan mode"),
        "deny message must mention plan mode, got: {result}"
    );
}

#[tokio::test]
async fn e2e_prd28_ac3_readonly_runs_in_plan() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("data.txt");
    std::fs::write(&target, "SENTINEL_CONTENT_XYZ").unwrap();
    let first = tool_call_sse("read_file", json!({ "path": target.to_str().unwrap() }));
    mock_two_turns(&server, first, final_text_sse("Read done.")).await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--plan", "doc file"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let events = parse_events(output);
    let completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed" && e["tool_name"] == "read_file")
        .expect("tool.completed for read_file present");
    let result = completed["result"].as_str().unwrap();
    assert!(
        result.contains("SENTINEL_CONTENT_XYZ"),
        "read_file must run in plan mode and return content, got: {result}"
    );
}

#[tokio::test]
async fn e2e_prd28_ac4_no_plan_writes_file() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foo.txt");
    let first = tool_call_sse(
        "write_file",
        json!({ "path": target.to_str().unwrap(), "content": "created" }),
    );
    mock_two_turns(&server, first, final_text_sse("Done.")).await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "tao file foo.txt"])
        .assert()
        .success();

    assert!(
        target.exists(),
        "without --plan the file must be created (regression guard)"
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "created");
}

#[tokio::test]
async fn e2e_prd28_ac6_bash_unsafe_blocked() {
    let server = MockServer::start().await;
    let first = tool_call_sse(
        "bash",
        json!({ "command": "rm -rf /tmp/zz_plan_should_not_run" }),
    );
    mock_two_turns(&server, first, final_text_sse("Plan ready.")).await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--plan", "chay rm"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let events = parse_events(output);
    let completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed" && e["tool_name"] == "bash")
        .expect("tool.completed for bash present");
    let result = completed["result"].as_str().unwrap();
    assert!(
        result.contains("plan mode"),
        "unsafe bash must be blocked in plan mode, got: {result}"
    );
}

#[tokio::test]
async fn e2e_prd28_ac6_bash_safe_runs() {
    let server = MockServer::start().await;
    let first = tool_call_sse("bash", json!({ "command": "echo PLAN_BASH_OK" }));
    mock_two_turns(&server, first, final_text_sse("Done.")).await;

    let output = Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .args(["exec", "--plan", "chay echo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let events = parse_events(output);
    let completed = events
        .iter()
        .find(|e| e["type"] == "tool.completed" && e["tool_name"] == "bash")
        .expect("tool.completed for bash present");
    let result = completed["result"].as_str().unwrap();
    assert!(
        result.contains("PLAN_BASH_OK"),
        "safe bash must run in plan mode, got: {result}"
    );
}

#[tokio::test]
async fn e2e_prd28_ac7_env_zz_plan() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("foo.txt");
    let first = tool_call_sse(
        "write_file",
        json!({ "path": target.to_str().unwrap(), "content": "hi" }),
    );
    mock_two_turns(&server, first, final_text_sse("Plan ready.")).await;

    Command::cargo_bin("zz")
        .unwrap()
        .env("OPENAI_API_KEY", "test-key")
        .env("ZZ_PROVIDER", "openai")
        .env("OPENAI_BASE_URL", server.uri())
        .env("ZZ_MODEL", "test-model")
        .env("ZZ_SANDBOX", "full-access")
        .env("ZZ_PLAN", "1")
        .args(["exec", "tao file foo.txt"])
        .assert()
        .success();

    assert!(
        !target.exists(),
        "ZZ_PLAN=1 must behave like --plan and block the write"
    );
}
