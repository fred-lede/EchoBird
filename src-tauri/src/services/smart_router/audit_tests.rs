use std::convert::Infallible;
use std::net::SocketAddr;

use super::*;

fn audit_candidate(id: &str) -> Candidate {
    let mut candidate = audit_http_candidate("127.0.0.1:1".parse().unwrap(), id);
    candidate.base_url = format!("fixture://{id}");
    candidate
}

fn audit_http_candidate(addr: SocketAddr, id: &str) -> Candidate {
    let model_id = format!("{id}-model");
    let base_url = format!("http://{addr}/v1");
    Candidate {
        attempt: None,
        internal_id: id.into(),
        fingerprint: candidate_fingerprint(&model_id, &base_url, "fixture-key"),
        model_id,
        base_url,
        api_key: "fixture-key".into(),
        account_source: None,
    }
}

async fn audit_mock(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn audit_completed(id: &str, output: Value, usage: Value) -> Value {
    json!({"object":"response","id":id,"status":"completed","error":null,"output":output,"usage":usage})
}

fn audit_text() -> Value {
    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fallback answer"}]}])
}

fn audit_response(value: Value, streaming: bool, native: bool) -> Response {
    let body = if streaming {
        format!(
            "event: response.created\ndata: {}\n\nevent: response.{}\ndata: {}\n\n",
            json!({"type":"response.created","response":{"object":"response","id":value["id"],"status":"in_progress","output":[]}}),
            value["status"].as_str().unwrap(),
            json!({"type":format!("response.{}", value["status"].as_str().unwrap()),"response":value})
        )
    } else {
        value.to_string()
    };
    let mut response = Response::builder()
        .header(
            header::CONTENT_TYPE,
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(Body::from(body))
        .unwrap();
    if native {
        response.extensions_mut().insert(NativeResponses);
    }
    response
}

fn audit_fixtures(first: Response, second: Response) -> AppState {
    let mut state = AppState::for_tests().unwrap();
    state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([
        ("first".into(), first),
        ("second".into(), second),
    ]))));
    state
}

fn audit_chat_json(value: Value) -> Response {
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}

fn audit_empty_chat() -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":0,"completion_tokens":0}})
}

#[tokio::test]
async fn audit_nonstream_empty_chat_and_messages_fall_back() {
    for anthropic in [true, false] {
        let state = audit_fixtures(
            audit_chat_json(audit_empty_chat()),
            audit_chat_json(
                json!({"choices":[{"message":{"role":"assistant","content":"nonstream fallback"},"finish_reason":"stop"}]}),
            ),
        );
        let body = json!({"model":"auto","messages":[{"role":"user","content":"hello"}],"max_tokens":1024,"stream":false});
        let candidates = vec![audit_candidate("first"), audit_candidate("second")];
        let response = if anthropic {
            route_messages_with_candidates(&state, &HeaderMap::new(), body, candidates).await
        } else {
            route_chat_with_candidates(&state, &HeaderMap::new(), body, candidates).await
        };
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model",
            "anthropic={anthropic}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("nonstream fallback"));
    }
}

#[tokio::test]
async fn audit_nonstream_all_empty_chat_and_messages_fail() {
    for anthropic in [false, true] {
        let state = audit_fixtures(
            audit_chat_json(audit_empty_chat()),
            audit_chat_json(audit_empty_chat()),
        );
        let body = json!({"model":"auto","messages":[{"role":"user","content":"hello"}],"max_tokens":1024,"stream":false});
        let candidates = vec![audit_candidate("first"), audit_candidate("second")];
        let response = if anthropic {
            route_messages_with_candidates(&state, &HeaderMap::new(), body, candidates).await
        } else {
            route_chat_with_candidates(&state, &HeaderMap::new(), body, candidates).await
        };
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "anthropic={anthropic}"
        );
    }
}

#[tokio::test]
async fn audit_nonstream_chat_valid_empty_and_semantic_outputs_are_preserved() {
    let mut counted = audit_empty_chat();
    counted["usage"]["prompt_tokens"] = json!(1);
    let mut length = audit_empty_chat();
    length["choices"][0]["finish_reason"] = json!("length");
    let mut filtered = audit_empty_chat();
    filtered["choices"][0]["finish_reason"] = json!("content_filter");
    let controls = [
        counted,
        length,
        filtered,
        json!({"choices":[{"message":{"role":"assistant","content":"","refusal":"cannot help"},"finish_reason":"stop"}]}),
        json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[{"message":{"role":"assistant","content":"","audio":{"data":"opaque-audio"}},"finish_reason":"stop"}]}),
        json!({"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"reasoning"},"finish_reason":"stop"}]}),
        json!({"choices":[{"message":{"role":"assistant","content":[{"type":"future_opaque_part","data":"opaque"}]},"finish_reason":"stop"}]}),
        json!({"opaque":"non-Chat JSON stays untouched"}),
    ];
    for control in controls {
        let expected = control.to_string();
        let state = audit_fixtures(
            audit_chat_json(control),
            audit_chat_json(audit_empty_chat()),
        );
        let response = route_chat_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"messages":[],"stream":false}),
            vec![audit_candidate("first"), audit_candidate("second")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "first/first-model"
        );
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            expected.as_bytes()
        );
        assert!(state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key("second"));
    }
}

#[tokio::test]
async fn audit_empty_completed_responses_fall_back_before_committing_headers() {
    for streaming in [false, true] {
        for native in [false, true] {
            let state = audit_fixtures(
                audit_response(
                    audit_completed(
                        "resp_empty",
                        json!([]),
                        json!({"input_tokens":0,"output_tokens":0}),
                    ),
                    streaming,
                    native,
                ),
                audit_response(
                    audit_completed("resp_answer", audit_text(), Value::Null),
                    streaming,
                    native,
                ),
            );
            let response = route_responses_with_candidates(
                &state,
                &HeaderMap::new(),
                json!({"input":"hello","stream":streaming}),
                vec![audit_candidate("first"), audit_candidate("second")],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "second/second-model",
                "streaming={streaming}, native={native}"
            );
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(String::from_utf8_lossy(&body).contains("fallback answer"));
            assert_eq!(
                state.route_memory.lock().unwrap().candidates["first"].failure,
                Some(FailureClass::Transient)
            );
        }
    }
}

#[tokio::test]
async fn audit_all_empty_completed_responses_return_service_unavailable() {
    for streaming in [false, true] {
        let empty = audit_completed("resp_empty", json!([]), Value::Null);
        let state = audit_fixtures(
            audit_response(empty.clone(), streaming, true),
            audit_response(empty, streaming, false),
        );
        let response = route_responses_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"input":"hello","stream":streaming}),
            vec![audit_candidate("first"), audit_candidate("second")],
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "streaming={streaming}"
        );
    }
}

#[tokio::test]
async fn audit_bound_empty_response_fails_without_switching_provider() {
    for streaming in [false, true] {
        let state = audit_fixtures(
            audit_response(
                audit_completed("resp_empty", json!([]), Value::Null),
                streaming,
                true,
            ),
            audit_response(
                audit_completed("resp_unused", audit_text(), Value::Null),
                streaming,
                true,
            ),
        );
        let first = audit_candidate("first");
        remember_response(&state, &first, Some("resp_prior"));
        let response = route_responses_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"previous_response_id":"resp_prior","input":"continue","stream":streaming}),
            vec![first, audit_candidate("second")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key("second"));
        assert!(!state
            .route_memory
            .lock()
            .unwrap()
            .response_bindings
            .contains_key("resp_empty"));
    }
}

#[tokio::test]
async fn audit_bound_native_context_failure_stays_http_bad_request() {
    let mut failed = audit_completed("resp_bound_context_failed", json!([]), Value::Null);
    failed["status"] = json!("failed");
    failed["error"] = json!({"code":"context_length_exceeded","message":"maximum context length exceeded","status":429});
    let state = audit_fixtures(
        audit_response(failed, true, true),
        audit_response(
            audit_completed("resp_unused", audit_text(), Value::Null),
            true,
            true,
        ),
    );
    let first = audit_candidate("first");
    remember_response(&state, &first, Some("resp_bound_context_prior"));
    let response = route_responses_with_candidates(
        &state,
        &HeaderMap::new(),
        json!({"previous_response_id":"resp_bound_context_prior","input":"too long","stream":true}),
        vec![first, audit_candidate("second")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(!text.contains("event: response.created"));
    assert!(!text.contains("event: response.failed"));
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["error"]["code"],
        "context_length_exceeded"
    );
    assert!(state
        .fixture_responses
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .contains_key("second"));
    let memory = state.route_memory.lock().unwrap();
    assert!(memory.candidates.is_empty());
    assert!(!memory
        .response_bindings
        .contains_key("resp_bound_context_failed"));
}

#[tokio::test]
async fn audit_translated_stop_only_responses_fall_back() {
    for streaming in [false, true] {
        let first = audit_mock(Router::new()
            .route("/v1/responses", post(|| async { StatusCode::NOT_FOUND }))
            .route("/v1/chat/completions", post(|| async {
                Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\ndata: [DONE]\n\n")).unwrap()
            }))).await;
        let second = audit_mock(Router::new().route(
            "/v1/responses",
            post(|Json(body): Json<Value>| async move {
                audit_response(
                    audit_completed("resp_http_answer", audit_text(), Value::Null),
                    body["stream"].as_bool().unwrap_or(false),
                    false,
                )
            }),
        ))
        .await;
        let state = AppState::for_tests().unwrap();
        let response = route_responses_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"input":"hello","stream":streaming}),
            vec![
                audit_http_candidate(first, "first"),
                audit_http_candidate(second, "second"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("fallback answer"));
    }
}

#[tokio::test]
async fn audit_native_api_responses_reorders_assistant_text_without_dropping_items() {
    let call = json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":"{}"});
    let thought = json!({"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]});
    let text = json!({"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"I will read it"}]});
    let result = json!({"type":"function_call_output","call_id":"call_1","output":"file contents"});
    let expected = json!([thought.clone(), text.clone(), call.clone(), result.clone()]);
    let observed = Arc::new(Mutex::new(None));
    let capture = observed.clone();
    let addr = audit_mock(Router::new().route(
        "/v1/responses",
        post(move |Json(body): Json<Value>| {
            *capture.lock().unwrap() = Some(body);
            async { Json(audit_completed("resp_sorted", audit_text(), Value::Null)) }
        }),
    ))
    .await;
    let state = AppState::for_tests().unwrap();
    let response = route_responses_with_candidates(
        &state,
        &HeaderMap::new(),
        json!({"input":[call,thought,text,result],"stream":false}),
        vec![audit_http_candidate(addr, "first")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        observed.lock().unwrap().as_ref().unwrap()["input"],
        expected
    );
}

#[tokio::test]
async fn audit_meaningful_or_accounted_responses_do_not_fall_back() {
    let mut incomplete = audit_completed("resp_truncated", json!([]), Value::Null);
    incomplete["status"] = json!("incomplete");
    incomplete["incomplete_details"] = json!({"reason":"max_output_tokens"});
    let controls = [
        audit_completed("resp_text", audit_text(), Value::Null),
        audit_completed(
            "resp_thought",
            json!([{"type":"reasoning","summary":[{"type":"summary_text","text":"thinking"}]}]),
            Value::Null,
        ),
        audit_completed(
            "resp_opaque_thought",
            json!([{"type":"reasoning","encrypted_content":"opaque-native-data","summary":[]}]),
            Value::Null,
        ),
        audit_completed(
            "resp_private_thought",
            json!([{"type":"reasoning","id":"rs_private","summary":[]}]),
            Value::Null,
        ),
        audit_completed(
            "resp_mcp",
            json!([{"type":"mcp_call","id":"mcp_1","name":"read_file","output":"fixture result"}]),
            json!({"input_tokens":0,"output_tokens":0}),
        ),
        audit_completed(
            "resp_search",
            json!([{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"fixture"}}]),
            Value::Null,
        ),
        audit_completed(
            "resp_computer",
            json!([{"type":"computer_call","id":"cu_1","call_id":"call_1","action":{"type":"screenshot"}}]),
            Value::Null,
        ),
        audit_completed(
            "resp_unknown",
            json!([{"type":"future_native_output","id":"future_1","data":"opaque"}]),
            Value::Null,
        ),
        audit_completed(
            "resp_opaque_part",
            json!([{"type":"message","content":[{"type":"future_opaque_part","data":"opaque"}]}]),
            Value::Null,
        ),
        audit_completed(
            "resp_tool",
            json!([{"type":"function_call","name":"read_file","call_id":"call_1","arguments":"{}"}]),
            Value::Null,
        ),
        audit_completed(
            "resp_refusal",
            json!([{"type":"message","content":[{"type":"refusal","refusal":"cannot help"}]}]),
            Value::Null,
        ),
        audit_completed(
            "resp_usage",
            json!([]),
            json!({"input_tokens":1,"output_tokens":0}),
        ),
        incomplete,
    ];
    for streaming in [false, true] {
        for control in &controls {
            let state = audit_fixtures(
                audit_response(control.clone(), streaming, true),
                audit_response(
                    audit_completed("resp_unused", audit_text(), Value::Null),
                    streaming,
                    true,
                ),
            );
            let response = route_responses_with_candidates(
                &state,
                &HeaderMap::new(),
                json!({"input":"hello","stream":streaming}),
                vec![audit_candidate("first"), audit_candidate("second")],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "first/first-model"
            );
            response.into_body().collect().await.unwrap();
            assert!(state
                .fixture_responses
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key("second"));
        }
    }
}

#[tokio::test]
async fn audit_role_only_chat_stream_falls_back_without_leaking_initial_frames() {
    let first = audit_mock(Router::new().route("/v1/chat/completions", post(|| async {
        Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":0,\"completion_tokens\":0}}\n\ndata: [DONE]\n\n")).unwrap()
    }))).await;
    let second = audit_mock(Router::new().route("/v1/chat/completions", post(|| async {
        Response::builder().header(header::CONTENT_TYPE, "text/event-stream").body(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")).unwrap()
    }))).await;
    let state = AppState::for_tests().unwrap();
    let response = route_chat_with_candidates(
        &state,
        &HeaderMap::new(),
        json!({"messages":[],"stream":true}),
        vec![
            audit_http_candidate(first, "first"),
            audit_http_candidate(second, "second"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["X-EchoBird-Routed-Via"],
        "second/second-model"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains("\"role\""));
}

#[tokio::test]
async fn audit_stream_preflight_releases_content_before_eof() {
    for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(4);
        let (admin, content, terminal) = if protocol == RouteProtocol::Chat {
            ("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n", "data: {\"choices\":[{\"delta\":{\"content\":\"first token\"},\"finish_reason\":null}]}\n\n", "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        } else {
            ("event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_delayed\",\"status\":\"in_progress\",\"object\":\"response\",\"output\":[]}}\n\n", "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"first token\"}\n\n", "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_delayed\",\"status\":\"completed\",\"object\":\"response\",\"output\":[]}}\n\n")
        };
        tx.send(Ok(Bytes::from(admin))).await.unwrap();
        let mut state = AppState::for_tests().unwrap();
        state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([(
            "first".into(),
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(ReceiverStream::new(rx)))
                .unwrap(),
        )]))));
        let mut pending = tokio::spawn(async move {
            route_with_policy(
                &state,
                &HeaderMap::new(),
                json!({"input":"hello","messages":[],"stream":true}),
                vec![audit_candidate("first")],
                RoutePolicy::default(),
                protocol,
            )
            .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut pending)
                .await
                .is_err(),
            "role-only metadata must not commit the route"
        );
        tx.send(Ok(Bytes::from(content))).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap();
        let mut body = response.into_body().into_data_stream();
        let mut received = String::new();
        while !received.contains("first token") {
            let chunk = tokio::time::timeout(Duration::from_secs(1), body.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            received.push_str(&String::from_utf8_lossy(&chunk));
        }
        tx.send(Ok(Bytes::from(terminal))).await.unwrap();
        drop(tx);
        while let Some(chunk) = body.next().await {
            chunk.unwrap();
        }
    }
}

#[tokio::test]
async fn audit_early_stream_frame_preserves_late_content_without_extending_first_byte_deadline() {
    for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
        for early_admin in [false, true] {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(2);
            let (admin, answer) = if protocol == RouteProtocol::Chat {
                ("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"},\"finish_reason\":null}]}\n\n", "data: {\"choices\":[{\"delta\":{\"content\":\"delayed answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
            } else {
                ("event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_late\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n", "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"delayed answer\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_late\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}}\n\n")
            };
            if early_admin {
                tx.send(Ok(Bytes::from(admin))).await.unwrap();
            }
            tokio::spawn(async move {
                if early_admin {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                } else {
                    for _ in 0..3 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        let _ = tx.send(Ok(Bytes::new())).await;
                    }
                    tokio::time::sleep(Duration::from_millis(90)).await;
                }
                let _ = tx.send(Ok(Bytes::from(answer))).await;
            });
            let mut state = AppState::for_tests().unwrap();
            state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([(
                "first".into(),
                Response::new(Body::from_stream(ReceiverStream::new(rx))),
            )]))));
            let response = route_with_policy(
                &state,
                &HeaderMap::new(),
                json!({"input":"hello","messages":[],"stream":true}),
                vec![audit_candidate("first")],
                RoutePolicy {
                    time_budget: Duration::from_millis(100),
                    first_byte_timeout: Duration::from_secs(1),
                    ..RoutePolicy::default()
                },
                protocol,
            )
            .await;
            assert_eq!(
                response.status(),
                if early_admin {
                    StatusCode::OK
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                "protocol={protocol:?}, early_admin={early_admin}"
            );
            if early_admin {
                let body = response.into_body().collect().await.unwrap().to_bytes();
                assert!(String::from_utf8_lossy(&body).contains("delayed answer"));
            }
        }
    }
}

#[tokio::test]
async fn audit_cancelled_preflight_drops_upstream_and_owned_activity() {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(2);
    tx.send(Ok(Bytes::from(
        "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
    )))
    .await
    .unwrap();
    let mut state = AppState::for_tests().unwrap();
    state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([(
        "audit-cancel-probe".into(),
        Response::new(Body::from_stream(ReceiverStream::new(rx))),
    )]))));
    let mut pending = tokio::spawn(async move {
        route_chat_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"messages":[],"stream":true}),
            vec![audit_candidate("audit-cancel-probe")],
        )
        .await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut pending)
            .await
            .is_err()
    );
    assert!(ROUTE_ACTIVITY
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .active_attempts
        .values()
        .any(|id| id == "audit-cancel-probe"));
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), tx.closed())
        .await
        .unwrap();
    assert!(!ROUTE_ACTIVITY
        .get()
        .unwrap()
        .lock()
        .unwrap()
        .active_attempts
        .values()
        .any(|id| id == "audit-cancel-probe"));
}

#[tokio::test]
async fn audit_late_empty_completion_does_not_start_fallback_after_budget() {
    for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(2);
        let (admin, terminal) = if protocol == RouteProtocol::Chat {
            ("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n","data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
        } else {
            ("event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_late_empty\",\"object\":\"response\",\"status\":\"in_progress\",\"output\":[]}}\n\n","event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_late_empty\",\"object\":\"response\",\"status\":\"completed\",\"output\":[]}}\n\n")
        };
        tx.send(Ok(Bytes::from(admin))).await.unwrap();
        let terminal_sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sent = terminal_sent.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            if tx.send(Ok(Bytes::from(terminal))).await.is_ok() {
                sent.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        let state = audit_fixtures(
            Response::new(Body::from_stream(ReceiverStream::new(rx))),
            audit_response(
                audit_completed("resp_unused", audit_text(), Value::Null),
                true,
                true,
            ),
        );
        let response = route_with_policy(
            &state,
            &HeaderMap::new(),
            json!({"input":"hello","messages":[],"stream":true}),
            vec![audit_candidate("first"), audit_candidate("second")],
            RoutePolicy {
                time_budget: Duration::from_millis(100),
                ..RoutePolicy::default()
            },
            protocol,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(terminal_sent.load(std::sync::atomic::Ordering::SeqCst));
        assert!(state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key("second"));
    }
}

#[tokio::test]
async fn audit_context_limit_stream_is_bad_request_without_fallback_or_cooldown() {
    let error = Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from("event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_limit\",\"status\":\"in_progress\",\"object\":\"response\",\"output\":[]}}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"context_length_exceeded\",\"message\":\"maximum context length exceeded\"}}}\n\n")).unwrap();
    let state = audit_fixtures(
        error,
        audit_response(
            audit_completed("resp_unused", audit_text(), Value::Null),
            true,
            true,
        ),
    );
    let response = route_responses_with_candidates(
        &state,
        &HeaderMap::new(),
        json!({"input":"too long","stream":true}),
        vec![audit_candidate("first"), audit_candidate("second")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(state
        .fixture_responses
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .contains_key("second"));
    assert!(state.route_memory.lock().unwrap().candidates.is_empty());
}

#[tokio::test]
async fn audit_http_context_limit_is_bad_request_even_with_retryable_status() {
    for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
        for status in [
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let error = Response::builder().status(status).header(header::CONTENT_TYPE,"application/json").body(Body::from(r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#)).unwrap();
            let state = audit_fixtures(
                error,
                audit_response(
                    audit_completed("resp_unused", audit_text(), Value::Null),
                    false,
                    true,
                ),
            );
            let response = route_with_policy(
                &state,
                &HeaderMap::new(),
                json!({"input":"too long","messages":[]}),
                vec![audit_candidate("first"), audit_candidate("second")],
                RoutePolicy::default(),
                protocol,
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert!(state
                .fixture_responses
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key("second"));
            assert!(state.route_memory.lock().unwrap().candidates.is_empty());
        }
    }
}

#[tokio::test]
async fn audit_preflight_limits_administrative_buffer_and_falls_back() {
    let admin = format!(
        "event: ping\ndata: {}\n\n",
        json!({"padding":"x".repeat(MAX_STREAM_PREFLIGHT_BYTES+1)})
    );
    let state = audit_fixtures(Response::new(Body::from(admin)),Response::new(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"bounded fallback\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")));
    let response = route_chat_with_candidates(
        &state,
        &HeaderMap::new(),
        json!({"messages":[],"stream":true}),
        vec![audit_candidate("first"), audit_candidate("second")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["X-EchoBird-Routed-Via"],
        "second/second-model"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("bounded fallback"));
}

#[tokio::test]
async fn audit_chat_usage_and_truncation_empty_streams_remain_valid() {
    for (finish, usage) in [
        ("stop", json!({"prompt_tokens":1,"completion_tokens":0})),
        ("length", Value::Null),
        ("content_filter", Value::Null),
    ] {
        let wire = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{},"finish_reason":finish}],"usage":usage})
        );
        let state = audit_fixtures(
            Response::new(Body::from(wire)),
            Response::new(Body::from("unused")),
        );
        let response = route_chat_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"messages":[],"stream":true}),
            vec![audit_candidate("first"), audit_candidate("second")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "first/first-model"
        );
        response.into_body().collect().await.unwrap();
        assert!(state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key("second"));
    }
}

#[tokio::test]
async fn audit_audio_payload_is_released_as_output_and_remains_valid() {
    for (protocol,payload) in [(RouteProtocol::Chat,"data: {\"choices\":[{\"delta\":{\"audio\":{\"data\":\"fixture-audio\"}},\"finish_reason\":null}]}\n\n"),(RouteProtocol::Responses,"event: response.output_audio.delta\ndata: {\"type\":\"response.output_audio.delta\",\"delta\":\"fixture-audio\"}\n\n")] {
        let (tx,rx) = tokio::sync::mpsc::channel::<Result<Bytes,Infallible>>(2);
        tx.send(Ok(Bytes::from(payload))).await.unwrap();
        let mut state = AppState::for_tests().unwrap();
        state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([("first".into(),Response::new(Body::from_stream(ReceiverStream::new(rx))))]))));
        let response = tokio::time::timeout(Duration::from_secs(1),route_with_policy(&state,&HeaderMap::new(),json!({"messages":[],"input":"hello","stream":true}),vec![audit_candidate("first")],RoutePolicy::default(),protocol)).await.unwrap();
        assert_eq!(response.status(),StatusCode::OK);
        let terminal = if protocol == RouteProtocol::Chat { "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n" } else { "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_audio\",\"status\":\"completed\",\"object\":\"response\",\"output\":[]}}\n\n" };
        tx.send(Ok(Bytes::from(terminal))).await.unwrap();
        drop(tx);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("fixture-audio"));
    }
}

#[tokio::test]
async fn audit_native_reasoning_progress_releases_preflight_before_quiet_completion() {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(2);
    tx.send(Ok(Bytes::from("event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_private\",\"status\":\"in_progress\",\"summary\":[]}}\n\n"))).await.unwrap();
    let mut state = AppState::for_tests().unwrap();
    state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([(
        "first".into(),
        Response::new(Body::from_stream(ReceiverStream::new(rx))),
    )]))));
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        route_with_policy(
            &state,
            &HeaderMap::new(),
            json!({"input":"hello","stream":true}),
            vec![audit_candidate("first")],
            RoutePolicy {
                time_budget: Duration::from_millis(100),
                ..RoutePolicy::default()
            },
            RouteProtocol::Responses,
        ),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // The route is committed to actual reasoning before a quiet upstream finishes.
    tx.send(Ok(Bytes::from("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_private\",\"status\":\"completed\",\"object\":\"response\",\"output\":[{\"type\":\"reasoning\",\"id\":\"rs_private\",\"summary\":[]}]}}\n\n"))).await.unwrap();
    drop(tx);
    response.into_body().collect().await.unwrap();
}

#[tokio::test]
async fn audit_empty_known_message_parts_still_fall_back() {
    for streaming in [false, true] {
        let empty = audit_completed(
            "resp_empty_parts",
            json!([{"type":"message","content":[{"type":"output_text","text":""},{"type":"refusal","refusal":""}]}]),
            Value::Null,
        );
        let state = audit_fixtures(
            audit_response(empty, streaming, true),
            audit_response(
                audit_completed("resp_answer", audit_text(), Value::Null),
                streaming,
                true,
            ),
        );
        let response = route_responses_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"input":"hello","stream":streaming}),
            vec![audit_candidate("first"), audit_candidate("second")],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model"
        );
        response.into_body().collect().await.unwrap();
    }
}

#[test]
fn audit_context_limit_wins_over_generic_rate_limit_classification() {
    assert_eq!(classify_failure(StatusCode::TOO_MANY_REQUESTS,br#"{"error":{"code":"context_length_exceeded","message":"request exceeds the context window"}}"#),None);
}

#[test]
fn audit_anthropic_adapter_separates_marked_google_thoughts() {
    let mut adapter = AnthropicSseAdapter::new();
    let mut output = Vec::new();
    for (text, marked) in [
        ("<th", true),
        ("ought>plan</th", false),
        ("ought>answer", false),
    ] {
        output.extend(adapter.push(format!("data: {}\n\n",json!({"choices":[{"delta":{"content":text,"extra_content":{"google":{"thought":marked}}}}]})).as_bytes()).unwrap());
    }
    output.extend(adapter.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap());
    let wire = output
        .into_iter()
        .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
        .collect::<String>();
    assert!(wire.contains("thinking_delta"));
    assert!(wire.contains("\"thinking\":\"plan\""));
    assert!(wire.contains("\"text\":\"answer\""));
    assert!(!wire.contains("<thought>"));
    let mut parser = crate::services::account_proxy::SseParser::default();
    let boundaries = parser
        .push(wire.as_bytes())
        .into_iter()
        .filter_map(|(_, data)| {
            let value: Value = serde_json::from_str(&data).unwrap();
            matches!(
                value["type"].as_str(),
                Some("content_block_start" | "content_block_stop")
            )
            .then(|| {
                (
                    value["type"].as_str().unwrap().to_owned(),
                    value["index"].as_u64().unwrap(),
                    value["content_block"]["type"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        boundaries,
        vec![
            ("content_block_start".into(), 0, "thinking".into()),
            ("content_block_stop".into(), 0, "".into()),
            ("content_block_start".into(), 1, "text".into()),
            ("content_block_stop".into(), 1, "".into())
        ]
    );
}

#[test]
fn audit_anthropic_adapter_preserves_literal_thought_tags_and_explicit_reasoning() {
    let mut adapter = AnthropicSseAdapter::new();
    let output = adapter.push(format!("data: {}\n\n",json!({"choices":[{"delta":{"content":"<thought>quoted</thought>","reasoning_content":"existing reasoning"}}]})).as_bytes()).unwrap();
    let wire = output
        .into_iter()
        .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
        .collect::<String>();
    assert!(wire.contains("\"thinking\":\"existing reasoning\""));
    assert!(wire.contains("<thought>quoted</thought>"));
}

#[test]
fn audit_anthropic_adapter_keeps_refusal_text() {
    let mut adapter = AnthropicSseAdapter::new();
    let output = adapter
        .push(
            format!(
                "data: {}\n\n",
                json!({"choices":[{"delta":{"refusal":"cannot help"}}]})
            )
            .as_bytes(),
        )
        .unwrap();
    let wire = output
        .into_iter()
        .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
        .collect::<String>();
    assert!(wire.contains("\"text\":\"cannot help\""));
}
