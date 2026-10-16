use super::*;
use crate::services::minimax_accounts::test_support::{json as response_json, MockServer};
use axum::body::to_bytes;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn credentials(base: &str) -> Value {
    json!({"accessToken":"fixture-token","userId":"fixture-user","accountId":"fixture-account",
        "principalType":"user","principalId":"fixture-user","baseUrl":base})
}

fn models() -> Value {
    json!({"data":[
        {"id":"grok-fixture","api_backend":"responses","context_window":500000,"context_windows":[500000,1000000]},
        {"model":"grok-second","apiBackend":"responses","contextWindows":[128000]},
        {"id":"chat-only","api_backend":"chat_completions"},
        {"id":"unknown-backend"},
        {"id":"disabled","api_backend":"responses","disabled":true}
    ]})
}

fn completed() -> Value {
    json!({"id":"resp_fixture","object":"response","status":"completed","model":"grok-fixture",
        "output":[{"type":"message","id":"msg_fixture","role":"assistant","status":"completed",
            "content":[{"type":"output_text","text":"hello","annotations":[]}]}],
        "usage":{"input_tokens":3,"output_tokens":2,"total_tokens":5}})
}

fn sse(events: &[Value]) -> Response {
    let data = events
        .iter()
        .map(|value| {
            format!(
                "event: {}\ndata: {}\n\n",
                value["type"].as_str().unwrap(),
                value
            )
        })
        .collect::<String>();
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from(data))
        .unwrap()
}

async fn response_value(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap()
}

#[tokio::test]
async fn directory_uses_real_account_models_and_headers_without_a_fabricated_version() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = MockServer::start(move |request| {
        let observed = observed.clone();
        async move {
            assert_eq!(request.path, "/models");
            assert_eq!(
                request.headers[header::AUTHORIZATION],
                "Bearer fixture-token"
            );
            assert_eq!(request.headers["x-xai-token-auth"], "xai-grok-cli");
            assert_eq!(request.headers["x-userid"], "fixture-user");
            assert_eq!(request.headers["x-grok-client-identifier"], "EchoBird");
            assert_eq!(request.headers["x-grok-client-mode"], "headless");
            assert!(request.headers[header::USER_AGENT]
                .to_str()
                .unwrap()
                .starts_with("EchoBird/"));
            let call = observed.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                assert!(!request.headers.contains_key("x-grok-client-version"));
            } else {
                assert_eq!(request.headers["x-grok-client-version"], "1.2.3");
            }
            response_json(200, models())
        }
    })
    .await;
    let client = reqwest::Client::new();
    let mut auth = credentials(&server.url);
    assert_eq!(
        list_models(auth.clone(), &client).await.unwrap(),
        vec!["grok-fixture", "grok-fixture@1000k", "grok-second"]
    );
    auth["clientVersion"] = json!("grok v1.2.3");
    list_models(auth, &client).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn native_responses_preserve_continuation_and_validate_context_variants() {
    let posts = Arc::new(AtomicUsize::new(0));
    let observed = posts.clone();
    let request_body = json!({"model":"alias","previous_response_id":"resp_previous","store":true,
        "instructions":"keep this", "input":[{"type":"reasoning","id":"reasoning_previous","encrypted_content":"opaque"},
            {"type":"function_call_output","call_id":"call_previous","output":"result"}],
        "tools":[{"type":"function","name":"inspect","parameters":{"type":"object"},"strict":true}],
        "tool_choice":{"type":"function","name":"inspect"},"prompt_cache_key":"conversation_fixture",
        "reasoning":{"effort":"high"},"metadata":{"source":"fixture"},"stream":false});
    let expected = request_body.clone();
    let server = MockServer::start(move |request| {
        let observed = observed.clone();
        let mut expected = expected.clone();
        async move {
            if request.path == "/models" {
                return response_json(200, models());
            }
            assert_eq!(request.path, "/responses");
            observed.fetch_add(1, Ordering::SeqCst);
            expected["model"] = json!("grok-fixture");
            assert_eq!(
                serde_json::from_str::<Value>(&request.body).unwrap(),
                expected
            );
            assert_eq!(request.headers["x-grok-model-override"], "grok-fixture");
            assert_eq!(request.headers["x-grok-context-window"], "1000000");
            assert_eq!(request.headers["x-grok-conv-id"], "conversation_fixture");
            assert_eq!(request.headers["x-grok-session-id"], "conversation_fixture");
            assert!(!request.headers["x-grok-req-id"].is_empty());
            response_json(200, completed())
        }
    })
    .await;
    let client = reqwest::Client::new();
    let auth = credentials(&server.url);
    assert_eq!(
        response_value(
            route_responses(auth.clone(), "grok-fixture@1000k", request_body, &client)
                .await
                .unwrap()
        )
        .await,
        completed()
    );
    let error = route_responses(
        auth.clone(),
        "grok-fixture@2000k",
        json!({"input":"test"}),
        &client,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.model_scope);
    let error = route_responses(auth, "chat-only", json!({"input":"test"}), &client)
        .await
        .unwrap_err();
    assert!(error.model_scope);
    assert_eq!(posts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn catalog_cache_is_bound_to_account_user_principal_and_endpoint() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = MockServer::start(move |request| {
        let observed = observed.clone();
        async move {
            assert_eq!(request.path, "/models");
            observed.fetch_add(1, Ordering::SeqCst);
            response_json(200, models())
        }
    })
    .await;
    let client = reqwest::Client::new();
    let original = credentials(&server.url);
    resolve_model(&original, "grok-fixture", &client)
        .await
        .unwrap();
    resolve_model(&original, "grok-fixture", &client)
        .await
        .unwrap();
    for field in [
        "accountId",
        "userId",
        "principalType",
        "principalId",
        "authScope",
    ] {
        let mut changed = original.clone();
        changed[field] = json!("different");
        resolve_model(&changed, "grok-fixture", &client)
            .await
            .unwrap();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 6);
    let mut changed_endpoint = original.clone();
    changed_endpoint["baseUrl"] = json!(format!("{}/v1", server.url));
    assert_ne!(
        cache_key(&original).unwrap(),
        cache_key(&changed_endpoint).unwrap()
    );
}

#[tokio::test]
async fn chat_conversion_preserves_function_round_trips_and_sampling_controls() {
    let server = MockServer::start(|request| async move {
        if request.path == "/models" { return response_json(200, models()); }
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["top_p"], 0.8);
        assert_eq!(body["max_output_tokens"], 256);
        assert_eq!(body["instructions"], "be helpful");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "inspect");
        assert_eq!(body["tools"][0]["strict"], true);
        assert_eq!(body["tool_choice"], json!({"type":"function","name":"inspect"}));
        assert_eq!(body["input"][0], json!({"type":"function_call","call_id":"call_prior","name":"inspect","arguments":"{}"}));
        assert_eq!(body["input"][1], json!({"type":"function_call_output","call_id":"call_prior","output":"done"}));
        assert_eq!(body["input"][2]["content"], json!([
            {"type":"input_image","image_url":"https://example.test/high","detail":"high"},
            {"type":"input_image","image_url":"https://example.test/low","detail":"low"},
            {"type":"input_image","image_url":"https://example.test/auto","detail":"auto"},
            {"type":"input_image","image_url":"https://example.test/default"}
        ]));
        sse(&[json!({"type":"response.output_text.delta","item_id":"msg_fixture","output_index":0,"content_index":0,"delta":"hello"}),
            json!({"type":"response.completed","response":completed()})])
    }).await;
    let response = route(credentials(&server.url), "grok-fixture", json!({"messages":[
        {"role":"system","content":"be helpful"},
        {"role":"assistant","content":null,"tool_calls":[{"id":"call_prior","type":"function","function":{"name":"inspect","arguments":"{}"}}]},
        {"role":"tool","tool_call_id":"call_prior","content":"done"},
        {"role":"user","content":[
            {"type":"image_url","image_url":{"url":"https://example.test/high","detail":"high"}},
            {"type":"image_url","image_url":{"url":"https://example.test/low","detail":"low"}},
            {"type":"image_url","image_url":{"url":"https://example.test/auto","detail":"auto"}},
            {"type":"image_url","image_url":{"url":"https://example.test/default"}}
        ]}],
        "tools":[{"type":"function","function":{"name":"inspect","parameters":{"type":"object"},"strict":true}}],
        "tool_choice":{"type":"function","function":{"name":"inspect"}},
        "temperature":0.2,"top_p":0.8,"max_completion_tokens":256,"stream":false}), &reqwest::Client::new()).await.unwrap();
    let body = response_value(response).await;
    assert_eq!(body["choices"][0]["message"]["content"], "hello");
    assert_eq!(body["usage"]["total_tokens"], 5);
}

#[tokio::test]
async fn unsupported_tools_content_and_constraints_fail_before_any_network_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = MockServer::start(move |_| {
        let observed = observed.clone();
        async move {
            observed.fetch_add(1, Ordering::SeqCst);
            response_json(500, Value::Null)
        }
    })
    .await;
    let client = reqwest::Client::new();
    for body in [
        json!({"messages":[],"stop":["stop"]}),
        json!({"messages":[],"n":2}),
        json!({"messages":[],"logprobs":true}),
        json!({"messages":[],"tools":[{"type":"custom","name":"custom"}]}),
        json!({"messages":[],"max_tokens":10,"max_completion_tokens":20}),
        json!({"messages":[{"role":"user","content":[{"type":"input_audio","data":"audio"}]}]}),
        json!({"messages":[{"role":"system","content":[{"type":"image_url","image_url":{"url":"https://example.test/image"}}]}]}),
    ] {
        let error = route(credentials(&server.url), "grok-fixture", body, &client)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.model_scope);
    }
    for tool in ["namespace", "custom", "computer_use_preview"] {
        let error = route_responses(
            credentials(&server.url),
            "grok-fixture",
            json!({"input":"test","tools":[{"type":tool}]}),
            &client,
        )
        .await
        .unwrap_err();
        assert!(error.model_scope);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn upstream_auth_and_rate_errors_preserve_status_and_retry_after_without_replay() {
    for status in [401, 429] {
        let posts = Arc::new(AtomicUsize::new(0));
        let observed = posts.clone();
        let server = MockServer::start(move |request| {
            let observed = observed.clone();
            async move {
                if request.path == "/models" {
                    return response_json(200, models());
                }
                observed.fetch_add(1, Ordering::SeqCst);
                let mut response = response_json(
                    status,
                    json!({"error":{"message":"fixture","code":"fixture_error"}}),
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, "17".parse().unwrap());
                response
            }
        })
        .await;
        let error = route_responses(
            credentials(&server.url),
            "grok-fixture",
            json!({"input":"test"}),
            &reqwest::Client::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status.as_u16(), status);
        assert_eq!(error.retry_after.unwrap(), "17");
        assert_eq!(posts.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn responses_sse_keeps_raw_events_and_nonstream_requires_a_valid_terminal() {
    for streaming in [false, true] {
        let events = vec![
            json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
            json!({"type":"response.completed","response":completed()}),
        ];
        let expected = events.clone();
        let server = MockServer::start(move |request| {
            let events = events.clone();
            async move {
                if request.path == "/models" {
                    return response_json(200, models());
                }
                sse(&events)
            }
        })
        .await;
        let response = route_responses(
            credentials(&server.url),
            "grok-fixture",
            json!({"input":"test","stream":streaming}),
            &reqwest::Client::new(),
        )
        .await
        .unwrap();
        if streaming {
            let raw = to_bytes(response.into_body(), 1_048_576).await.unwrap();
            let expected_raw = to_bytes(sse(&expected).into_body(), 1_048_576)
                .await
                .unwrap();
            assert_eq!(raw, expected_raw);
        } else {
            assert_eq!(response_value(response).await, completed());
        }
    }
    let server = MockServer::start(|request| async move {
        if request.path == "/models" { return response_json(200, models()); }
        sse(&[json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}})])
    }).await;
    for native in [false, true] {
        let response = if native {
            route_responses(
                credentials(&server.url),
                "grok-fixture",
                json!({"input":"test"}),
                &reqwest::Client::new(),
            )
            .await
        } else {
            route(
                credentials(&server.url),
                "grok-fixture",
                json!({"messages":[{"role":"user","content":"test"}]}),
                &reqwest::Client::new(),
            )
            .await
        }
        .unwrap();
        assert!(to_bytes(response.into_body(), 1_048_576).await.is_err());
    }
}

#[tokio::test]
async fn dropping_chat_response_releases_the_pending_upstream_stream() {
    struct PendingStream(Arc<AtomicUsize>);
    impl futures_util::Stream for PendingStream {
        type Item = Result<bytes::Bytes, std::io::Error>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }
    impl Drop for PendingStream {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicUsize::new(0));
    let observed = dropped.clone();
    let server = MockServer::start(move |request| {
        let observed = observed.clone();
        async move {
            if request.path == "/models" {
                return response_json(200, models());
            }
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(PendingStream(observed)))
                .unwrap()
        }
    })
    .await;
    let response = route(
        credentials(&server.url),
        "grok-fixture",
        json!({"messages":[{"role":"user","content":"test"}],"stream":true}),
        &reqwest::Client::new(),
    )
    .await
    .unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    drop(response);
    tokio::time::timeout(Duration::from_secs(2), async {
        while dropped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn endpoints_and_version_parsing_do_not_accept_a_fake_remote_fixture_or_banner() {
    assert!(base(&json!({"baseUrl":"https://example.test/v1"})).is_err());
    assert_eq!(base(&Value::Null).unwrap(), BASE);
    assert_eq!(
        parse_version("grok 1.2.3-beta.1"),
        Some("1.2.3-beta.1".into())
    );
    for banner in [
        "grok latest",
        "grok 1.2",
        "grok 1.2.3/extra",
        "1.2.3\r\ninjected: header",
    ] {
        if banner.contains("injected") {
            continue;
        }
        assert_eq!(parse_version(banner), None);
    }
}
