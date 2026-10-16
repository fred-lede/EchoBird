use super::*;
use crate::services::minimax_accounts::test_support::{json, MockServer};
use axum::body::{to_bytes, Body};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn credentials(base: &str, region: &str) -> Value {
    json!({"accessToken":"fixture-token","accountId":format!("fixture-{region}"),"region":region,"baseUrl":base})
}
fn catalog() -> Value {
    json!({"providers":[{"providerId":"other","config":{"models":{"ignored":{}}}},{"providerId":"minimax","config":{"model_order":["dynamic-long","dynamic-short","dynamic-long"],"models":{
        "dynamic-long":{"name":"Dynamic Long","limit":{"context":1_000_000,"output":128000},"modalities":{"input":["text","image"]},"thinking_config":{"mode":"forced_on"},"thinking":{"effortOptions":["low","high","max"]}},
        "dynamic-short":{"name":"Dynamic Short","limit":{"context":204800,"output":4096}},"disabled":{"enabled":false}
    }}}]})
}
fn sse(events: Vec<Value>) -> Response {
    let body: String = events
        .into_iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {}\n\n",
                event["type"].as_str().unwrap(),
                event
            )
        })
        .collect();
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from(body))
        .unwrap()
}
fn text_events() -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{"id":"message-fixture","model":"dynamic-short","usage":{"input_tokens":4,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好🌍"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ]
}
async fn body(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap()
}

#[tokio::test]
async fn live_catalog_keeps_order_capabilities_and_region_and_account_identity() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let server = MockServer::start(move |request| {
        let requests = requests.clone();
        async move {
            assert_eq!(request.path, "/mavis/api/v1/models");
            assert_eq!(request.headers["authorization"], "Bearer fixture-token");
            requests.lock().unwrap().push(request.query);
            json(200, catalog())
        }
    })
    .await;
    for region in ["cn", "global"] {
        let details = list_model_details(credentials(&server.url, region), &reqwest::Client::new())
            .await
            .unwrap();
        assert_eq!(
            details
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["dynamic-long", "dynamic-short"]
        );
        assert_eq!(details[0].context, Some(1_000_000));
        assert!(details[0].images);
        assert_eq!(details[0].efforts, ["low", "high", "max"]);
    }
    let queries = seen.lock().unwrap();
    assert!(queries[0].contains("region=cn"));
    assert!(queries[1].contains("region=en"));
    assert_ne!(
        cache_key(&credentials(&server.url, "cn")).unwrap(),
        cache_key(&credentials(&server.url, "global")).unwrap()
    );
    let account = credentials(&server.url, "cn");
    let mut other = account.clone();
    other["accountId"] = json!("other");
    assert_ne!(cache_key(&account).unwrap(), cache_key(&other).unwrap());
    assert!(request(
        &account,
        "dynamic-long",
        &json!({"messages":[],"reasoning_effort":"none"})
    )
    .is_err());
}

#[tokio::test]
async fn chat_text_stream_and_shared_responses_and_anthropic_paths_use_account_headers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = MockServer::start(move |request| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.path, "/mavis/api/v1/llm/v1/messages");
            assert_eq!(request.headers["authorization"], "Bearer fixture-token");
            assert_eq!(request.headers["x-api-key"], "sk-xxx");
            assert_eq!(request.headers["x-mavis-agent-id"], "main");
            assert_eq!(request.headers["anthropic-version"], "2023-06-01");
            assert_eq!(
                request.headers["x-mavis-session-id"],
                "conversation-fixture"
            );
            let input: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(input["model"], "dynamic-short");
            assert_eq!(input["stream"], true);
            sse(text_events())
        }
    })
    .await;
    let credential = credentials(&server.url, "global");
    let client = reqwest::Client::new();
    let input = json!({"messages":[{"role":"user","content":"hello"}],"metadata":{"session_id":"conversation-fixture"},"stream":false});
    let output = body(
        route(credential.clone(), "dynamic-short", input.clone(), &client)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(output["choices"][0]["message"]["content"], "你好🌍");
    assert_eq!(output["choices"][0]["finish_reason"], "stop");
    let mut stream = input.clone();
    stream["stream"] = json!(true);
    let response = route(credential.clone(), "dynamic-short", stream, &client)
        .await
        .unwrap();
    let stream = String::from_utf8(
        to_bytes(response.into_body(), 1_048_576)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(stream.contains("你好🌍"));
    assert!(stream.contains("[DONE]"));
    let response = route(credential.clone(), "dynamic-short", input.clone(), &client)
        .await
        .unwrap();
    let responses = body(super::super::chat_to_responses(response, false).await).await;
    assert_eq!(responses["status"], "completed");
    assert_eq!(responses["output"][0]["content"][0]["text"], "你好🌍");
    let anthropic = crate::services::local_llm::proxy::openai_to_anthropic(&output);
    assert_eq!(anthropic["content"][0]["text"], "你好🌍");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn fragmented_tool_arguments_and_followup_results_preserve_the_call_identity() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server=MockServer::start(move |request| {let count=count.clone();async move {
        let input:Value=serde_json::from_str(&request.body).unwrap();
        if count.fetch_add(1,Ordering::SeqCst)==0 {
            assert_eq!(input["tools"][0]["name"],"echo");
            sse(vec![json!({"type":"message_start","message":{"usage":{"input_tokens":1}}}),
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-echo","name":"echo","input":{}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"text\":"}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"你好🌍\"}"}}),
                json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),json!({"type":"message_stop"})])
        } else {
            assert_eq!(input["messages"][1]["content"][0]["id"],"call-echo");
            assert_eq!(input["messages"][2]["content"][0]["tool_use_id"],"call-echo");
            assert_eq!(input["messages"][2]["content"][0]["content"],"tool result");sse(text_events())
        }
    }}).await;
    let credential = credentials(&server.url, "cn");
    let client = reqwest::Client::new();
    let tools = json!([{"type":"function","function":{"name":"echo","parameters":{"type":"object","properties":{"text":{"type":"string"}}}}}]);
    let output = body(
        route(
            credential.clone(),
            "dynamic-short",
            json!({"messages":[{"role":"user","content":"call tool"}],"tools":tools}),
            &client,
        )
        .await
        .unwrap(),
    )
    .await;
    let message = &output["choices"][0]["message"];
    assert_eq!(message["tool_calls"][0]["id"], "call-echo");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        "{\"text\":\"你好🌍\"}"
    );
    let output=body(route(credential,"dynamic-short",json!({"messages":[{"role":"user","content":"call tool"},message,{"role":"tool","tool_call_id":"call-echo","content":"tool result"}],"tools":tools}),&client).await.unwrap()).await;
    assert_eq!(output["choices"][0]["message"]["content"], "你好🌍");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn quota_auth_and_model_errors_keep_retry_and_scope_without_following_redirects() {
    let server = MockServer::start(|request| async move {
        let input: Value = serde_json::from_str(&request.body).unwrap_or(Value::Null);
        match input["model"].as_str() {
            Some("quota") => {
                let mut response = json(403, json!({"error":{"message":"insufficient credits"}}));
                response
                    .headers_mut()
                    .insert("retry-after", "90".parse().unwrap());
                response
            }
            Some("auth") => json(401, json!({"error":{"message":"token expired"}})),
            Some("missing") => json(
                404,
                json!({"error":{"code":"model_not_found","message":"unknown model"}}),
            ),
            _ => Response::builder()
                .status(302)
                .header("location", "https://example.invalid/never")
                .body(Body::empty())
                .unwrap(),
        }
    })
    .await;
    let client = reqwest::Client::new();
    let credential = credentials(&server.url, "cn");
    let quota = route(credential.clone(), "quota", json!({"messages":[]}), &client)
        .await
        .unwrap_err();
    assert_eq!(quota.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(quota.retry_after.unwrap(), "90");
    assert!(!quota.model_scope);
    assert_eq!(
        route(credential.clone(), "auth", json!({"messages":[]}), &client)
            .await
            .unwrap_err()
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        route(
            credential.clone(),
            "missing",
            json!({"messages":[]}),
            &client
        )
        .await
        .unwrap_err()
        .model_scope
    );
    assert_eq!(
        route(
            credential.clone(),
            "redirect",
            json!({"messages":[]}),
            &client
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        list_models(credential, &client).await.unwrap_err().status,
        StatusCode::BAD_GATEWAY
    );
}

#[tokio::test]
async fn downstream_cancellation_closes_the_account_stream() {
    let stopped = Arc::new(tokio::sync::Notify::new());
    let signal = stopped.clone();
    let server = MockServer::start(move |_| {
        let signal = signal.clone();
        async move {
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(2);
            tokio::spawn(async move {
                tx.send(Ok(bytes::Bytes::from(
                    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{}}\n\n",
                )))
                .await
                .unwrap();
                tx.closed().await;
                signal.notify_one();
            });
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(
                    tokio_stream::wrappers::ReceiverStream::new(rx),
                ))
                .unwrap()
        }
    })
    .await;
    let response = route(
        credentials(&server.url, "global"),
        "dynamic-short",
        json!({"messages":[],"stream":true}),
        &reqwest::Client::new(),
    )
    .await
    .unwrap();
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), stopped.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn unsupported_generation_constraints_are_rejected_without_sending_an_account_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = MockServer::start(move |_| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            sse(text_events())
        }
    })
    .await;
    for extra in [
        json!({"stop":["END"]}),
        json!({"stop":"END"}),
        json!({"n":2}),
        json!({"tools":[{"type":"function","function":{"name":"strict","strict":true}}]}),
        json!({"tools":[{"type":"web_search"}]}),
        json!({"stop_sequences":["END"]}),
        json!({"frequency_penalty":0.5}),
        json!({"presence_penalty":1}),
        json!({"seed":0}),
        json!({"logprobs":true}),
        json!({"top_logprobs":2}),
        json!({"logit_bias":{"123":-100}}),
        json!({"modalities":["text","audio"]}),
        json!({"audio":{"voice":"alloy","format":"wav"}}),
        json!({"prediction":{"type":"content","content":"predicted"}}),
        json!({"functions":[{"name":"read"}]}),
        json!({"function_call":"required"}),
    ] {
        let mut input = json!({"messages":[{"role":"user","content":"hello"}]});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let error = route(
            credentials(&server.url, "global"),
            "dynamic-short",
            input,
            &reqwest::Client::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.model_scope);
        assert_eq!(error.error_code.as_deref(), Some("unsupported_parameter"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(request(&credentials(&server.url,"global"),"dynamic-short",&json!({"messages":[],"n":1,"stop":[],"temperature":0.4,"top_p":0.8,"max_completion_tokens":512})).is_ok());
}

#[tokio::test]
async fn unsupported_content_never_sends_a_partial_or_empty_account_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = MockServer::start(move |_| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            sse(text_events())
        }
    })
    .await;
    for block in [
        json!({"type":"file","file":{"filename":"report.pdf","file_data":"data:application/pdf;base64,cGRm"}}),
        json!({"type":"input_audio","input_audio":{"data":"YXVkaW8=","format":"wav"}}),
        json!({"type":"image_url","image_url":{"url":"data:image/png,unencoded"}}),
        json!({"type":"unknown","text":"must not be dropped"}),
    ] {
        for include_text in [false, true] {
            let mut content = vec![block.clone()];
            if include_text {
                content.insert(0, json!({"type":"text","text":"inspect the attachment"}));
            }
            let error = route(
                credentials(&server.url, "global"),
                "dynamic-short",
                json!({"messages":[{"role":"user","content":content}]}),
                &reqwest::Client::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST);
            assert!(error.model_scope);
            assert_eq!(error.error_code.as_deref(), Some("unsupported_parameter"));
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn supported_text_images_and_tools_preserve_content_with_noop_client_defaults() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let server = MockServer::start(move |request| {
        let count = count.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(body["messages"][0]["content"][0], json!({"type":"text","text":"inspect"}));
            assert_eq!(body["messages"][0]["content"][1], json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"eA=="}}));
            assert_eq!(body["messages"][0]["content"][2], json!({"type":"image","source":{"type":"url","url":"https://example.invalid/image.png"}}));
            assert_eq!(body["messages"][1]["content"][0], json!({"type":"text","text":"before after"}));
            assert_eq!(body["messages"][1]["content"][1]["id"], "call-echo");
            assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call-echo");
            assert_eq!(body["messages"][2]["content"][0]["content"], json!([{"type":"text","text":"done"}]));
            sse(text_events())
        }
    })
    .await;
    let response = route(
        credentials(&server.url, "cn"),
        "dynamic-short",
        json!({"frequency_penalty":0,"presence_penalty":0.0,"seed":null,"logprobs":false,"top_logprobs":0,"logit_bias":{},"functions":[],"modalities":["text"],"messages":[
            {"role":"user","content":[{"type":"text","text":"inspect"},{"type":"image_url","image_url":{"url":"data:image/png;base64,eA=="}},{"type":"image_url","image_url":{"url":"https://example.invalid/image.png"}}]},
            {"role":"assistant","content":[{"type":"text","text":"before"},{"type":"text","text":" after"}],"tool_calls":[{"id":"call-echo","type":"function","function":{"name":"echo","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"call-echo","content":[{"type":"text","text":"done"}]}
        ]}),
        &reqwest::Client::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        body(response).await["choices"][0]["message"]["content"],
        "你好🌍"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
