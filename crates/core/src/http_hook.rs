//! HTTP webhook hook — POST JSON event to URL, fire-and-forget .
//!
//! Parity Claude Code `type: "http"` handler. Each `HttpHook` instance is
//! bound to one `HookEvent` and dispatches only that event. `pre_tool` parses
//! the response body for `{"decision":"block"}` to abort tool execution.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::hooks::{
    HookAction, HookEvent, LifecycleHooks, PostToolContext, PreCompactCtx, SessionStartCtx,
    StopCtx, ToolFailureCtx, ToolHookContext, UserPromptCtx,
};

/// HTTP webhook hook — POST JSON event payload to a configured URL.
pub struct HttpHook {
    event: HookEvent,
    matcher: Option<String>,
    url: String,
    timeout: Duration,
    headers: HashMap<String, String>,
    client: reqwest::Client,
}

impl HttpHook {
    pub fn new(
        event: HookEvent,
        url: String,
        timeout: Duration,
        headers: HashMap<String, String>,
        matcher: Option<String>,
    ) -> Self {
        Self {
            event,
            matcher,
            url,
            timeout,
            headers,
            client: reqwest::Client::new(),
        }
    }

    /// Fire-and-forget POST JSON. Ignore error (non-blocking, parity Claude Code S4).
    async fn dispatch(&self, event_name: &str, payload: Value) {
        let mut body = serde_json::Map::new();
        body.insert(
            "hook_event_name".to_string(),
            Value::String(event_name.to_string()),
        );
        if let Some(obj) = payload.as_object() {
            for (k, v) in obj {
                body.insert(k.clone(), v.clone());
            }
        }
        let _ = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .headers(build_header_map(&self.headers))
            .json(&Value::Object(body))
            .timeout(self.timeout)
            .send()
            .await;
    }

    /// Matcher check — true if matcher is None OR tool_name contains matcher.
    fn matcher_ok(&self, tool_name: &str) -> bool {
        match &self.matcher {
            None => true,
            Some(m) => tool_name.contains(m),
        }
    }
}

fn build_header_map(headers: &HashMap<String, String>) -> reqwest::header::HeaderMap {
    let mut map = reqwest::header::HeaderMap::new();
    for (k, v) in headers {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
            reqwest::header::HeaderValue::from_str(v),
        ) {
            map.insert(name, val);
        }
    }
    map
}

#[async_trait::async_trait]
impl LifecycleHooks for HttpHook {
    async fn pre_tool(&self, ctx: &ToolHookContext) -> HookAction {
        if self.event != HookEvent::PreToolUse || !self.matcher_ok(&ctx.tool_name) {
            return HookAction::Continue {
                args: ctx.args.clone(),
            };
        }
        let body = serde_json::json!({
            "tool_name": ctx.tool_name,
            "tool_use_id": ctx.tool_call_id,
            "tool_input": ctx.args,
        });
        let mut payload = serde_json::Map::new();
        payload.insert(
            "hook_event_name".to_string(),
            Value::String("PreToolUse".to_string()),
        );
        if let Some(obj) = body.as_object() {
            for (k, v) in obj {
                payload.insert(k.clone(), v.clone());
            }
        }
        let req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .headers(build_header_map(&self.headers))
            .json(&Value::Object(payload))
            .timeout(self.timeout);
        match req.send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<Value>().await {
                Ok(v) if v.get("decision").and_then(|d| d.as_str()) == Some("block") => {
                    let reason = v
                        .get("reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("blocked by hook")
                        .to_string();
                    HookAction::Abort { reason }
                }
                _ => HookAction::Continue {
                    args: ctx.args.clone(),
                },
            },
            _ => HookAction::Continue {
                args: ctx.args.clone(),
            },
        }
    }

    async fn post_tool(&self, ctx: &PostToolContext) {
        if self.event == HookEvent::PostToolUse && self.matcher_ok(&ctx.tool_name) {
            self.dispatch(
                "PostToolUse",
                serde_json::json!({
                    "tool_name": ctx.tool_name,
                    "tool_use_id": ctx.tool_call_id,
                    "tool_input": ctx.args,
                    "tool_response": ctx.result,
                }),
            )
            .await;
        }
    }

    async fn session_start(&self, ctx: &SessionStartCtx) {
        if self.event == HookEvent::SessionStart {
            self.dispatch(
                "SessionStart",
                serde_json::json!({
                    "session_id": ctx.session_id,
                    "cwd": ctx.cwd,
                }),
            )
            .await;
        }
    }

    async fn session_end(&self, ctx: &SessionStartCtx) {
        if self.event == HookEvent::SessionEnd {
            self.dispatch(
                "SessionEnd",
                serde_json::json!({
                    "session_id": ctx.session_id,
                }),
            )
            .await;
        }
    }

    async fn user_prompt_submit(&self, ctx: &UserPromptCtx) {
        if self.event == HookEvent::UserPromptSubmit {
            self.dispatch(
                "UserPromptSubmit",
                serde_json::json!({
                    "session_id": ctx.session_id,
                    "prompt": ctx.prompt,
                }),
            )
            .await;
        }
    }

    async fn stop(&self, ctx: &StopCtx) {
        if self.event == HookEvent::Stop {
            self.dispatch(
                "Stop",
                serde_json::json!({
                    "session_id": ctx.session_id,
                    "reason": ctx.reason,
                }),
            )
            .await;
        }
    }

    async fn post_tool_failure(&self, ctx: &ToolFailureCtx) {
        if self.event == HookEvent::PostToolUseFailure && self.matcher_ok(&ctx.tool_name) {
            self.dispatch(
                "PostToolUseFailure",
                serde_json::json!({
                    "tool_name": ctx.tool_name,
                    "tool_use_id": ctx.tool_call_id,
                    "tool_input": ctx.args,
                    "error": ctx.error,
                }),
            )
            .await;
        }
    }

    async fn pre_compact(&self, ctx: &PreCompactCtx) {
        if self.event == HookEvent::PreCompact {
            self.dispatch(
                "PreCompact",
                serde_json::json!({
                    "session_id": ctx.session_id,
                    "before_messages": ctx.before_messages,
                    "before_tokens": ctx.before_tokens,
                    "trigger": ctx.trigger,
                }),
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn test_http_hook_post_received() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        let hook = HttpHook::new(
            HookEvent::SessionStart,
            url,
            Duration::from_secs(5),
            HashMap::new(),
            None,
        );
        hook.session_start(&SessionStartCtx {
            session_id: "s1".to_string(),
            cwd: "/tmp".to_string(),
        })
        .await;
        // Mock server received the POST — verify via recorded requests.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["hook_event_name"], "SessionStart");
        assert_eq!(body["session_id"], "s1");
        assert_eq!(body["cwd"], "/tmp");
        // Content-Type header.
        let ct = requests[0]
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(ct.contains("application/json"));
    }

    #[tokio::test]
    async fn test_http_hook_fire_and_forget_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        let hook = HttpHook::new(
            HookEvent::Stop,
            url,
            Duration::from_secs(5),
            HashMap::new(),
            None,
        );
        // Should not panic or return error — fire-and-forget.
        hook.stop(&StopCtx {
            session_id: "s1".to_string(),
            reason: "completed".to_string(),
        })
        .await;
    }

    #[tokio::test]
    async fn test_http_hook_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        let hook = HttpHook::new(
            HookEvent::Stop,
            url,
            Duration::from_millis(100),
            HashMap::new(),
            None,
        );
        let start = std::time::Instant::now();
        hook.stop(&StopCtx {
            session_id: "s1".to_string(),
            reason: "completed".to_string(),
        })
        .await;
        let elapsed = start.elapsed();
        // Should return well under 10s (timeout 100ms + small overhead).
        assert!(elapsed < Duration::from_secs(2), "elapsed: {elapsed:?}");
    }

    #[tokio::test]
    async fn test_http_hook_pre_tool_block() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "decision": "block",
                "reason": "forbidden"
            })))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        let hook = HttpHook::new(
            HookEvent::PreToolUse,
            url,
            Duration::from_secs(5),
            HashMap::new(),
            None,
        );
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({"command": "ls"}),
        };
        let action = hook.pre_tool(&ctx).await;
        match action {
            HookAction::Abort { reason } => assert_eq!(reason, "forbidden"),
            HookAction::Continue { .. } => panic!("Should abort"),
        }
    }

    #[tokio::test]
    async fn test_http_hook_pre_tool_allow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        let hook = HttpHook::new(
            HookEvent::PreToolUse,
            url,
            Duration::from_secs(5),
            HashMap::new(),
            None,
        );
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({"command": "ls"}),
        };
        let action = hook.pre_tool(&ctx).await;
        match action {
            HookAction::Continue { args } => assert_eq!(args["command"], "ls"),
            HookAction::Abort { .. } => panic!("Should continue"),
        }

        // Non-2xx → continue (non-blocking error).
        let server2 = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server2)
            .await;
        let hook2 = HttpHook::new(
            HookEvent::PreToolUse,
            format!("{}/hook", server2.uri()),
            Duration::from_secs(5),
            HashMap::new(),
            None,
        );
        let action2 = hook2.pre_tool(&ctx).await;
        match action2 {
            HookAction::Continue { .. } => {}
            HookAction::Abort { .. } => panic!("500 should continue"),
        }
    }

    #[tokio::test]
    async fn test_http_hook_matcher_skip() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let url = format!("{}/hook", server.uri());
        // matcher = "edit_file" — should skip "bash" tool.
        let hook = HttpHook::new(
            HookEvent::PreToolUse,
            url,
            Duration::from_secs(5),
            HashMap::new(),
            Some("edit_file".to_string()),
        );
        let ctx = ToolHookContext {
            tool_name: "bash".to_string(),
            tool_call_id: "c1".to_string(),
            args: json!({}),
        };
        let action = hook.pre_tool(&ctx).await;
        match action {
            HookAction::Continue { .. } => {}
            HookAction::Abort { .. } => panic!("matcher skip should continue"),
        }
        // No POST sent because matcher didn't match.
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.is_empty(),
            "no POST should be sent on matcher miss"
        );
    }
}
