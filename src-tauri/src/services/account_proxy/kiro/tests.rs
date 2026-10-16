use super::*;
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    routing::any,
    Router,
};
use bytes::Bytes;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    content_type: &'static str,
    bytes: Vec<u8>,
}
#[derive(Clone)]
struct StateData {
    seen: Arc<Mutex<Vec<Value>>>,
    pages: Arc<Mutex<Vec<Value>>>,
    reply: Arc<Mutex<Reply>>,
}
struct Fixture {
    base: String,
    state: StateData,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn gateway(State(state): State<StateData>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let query: BTreeMap<_, _> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    let headers = request.headers().clone();
    let body: Value = if path == "/generateAssistantResponse" {
        serde_json::from_slice(
            &to_bytes(request.into_body(), 32 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    } else {
        Value::Null
    };
    state.seen.lock().unwrap().push(json!({"path":path,"query":query,"auth":headers[header::AUTHORIZATION].to_str().unwrap(),"tokenType":headers.get("tokentype").and_then(|value| value.to_str().ok()),"agentMode":headers.get("x-amzn-kiro-agent-mode").and_then(|value|value.to_str().ok()),"userAgent":headers.get(header::USER_AGENT).and_then(|value|value.to_str().ok()),"body":body}));
    assert_eq!(headers[header::AUTHORIZATION], "Bearer fixture-access");
    if path == "/List-Available-Models" {
        let index = if query.contains_key("nextToken") {
            1
        } else {
            0
        };
        return super::super::json_response(&state.pages.lock().unwrap()[index]);
    }
    assert_eq!(path, "/generateAssistantResponse");
    let reply = state.reply.lock().unwrap().clone();
    let chunks: Vec<_> = reply
        .bytes
        .chunks(7)
        .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk)))
        .collect();
    Response::builder()
        .status(reply.status)
        .header(header::CONTENT_TYPE, reply.content_type)
        .body(Body::from_stream(futures_util::stream::iter(chunks)))
        .unwrap()
}
impl Fixture {
    async fn start() -> Self {
        let state = StateData {
            seen: Arc::new(Mutex::new(vec![])),
            pages: Arc::new(Mutex::new(vec![
                json!({"models":[{"modelId":"Model/CaseId","modelName":"Visible model","supportedInputTypes":["TEXT","IMAGE"],"tokenLimits":{"maxInputTokens":200000,"maxOutputTokens":16000}}],"defaultModel":{"modelId":"auto"},"nextToken":"page-two"}),
                json!({"models":[{"modelId":"text-only","modelName":"Text only","supportedInputTypes":["TEXT"]},{"modelId":"auto","modelName":"Auto","supportedInputTypes":["TEXT","IMAGE"]}],"defaultModel":{"modelId":"auto"}}),
            ])),
            reply: Arc::new(Mutex::new(Reply {
                status: StatusCode::OK,
                content_type: "application/vnd.amazon.eventstream",
                bytes: eventstream::frame(
                    "event",
                    "assistantResponseEvent",
                    &json!({"content":"accepted 中文"}),
                ),
            })),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .fallback(any(gateway))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, state, task }
    }
    fn credentials(&self) -> Value {
        json!({"baseUrl":self.base,"accountId":uuid::Uuid::new_v4().to_string(),"credentialGeneration":"generation-one","accessToken":"fixture-access","region":"us-east-1"})
    }
    fn events(&self, events: &[(&str, Value)]) {
        self.state.reply.lock().unwrap().bytes = events
            .iter()
            .flat_map(|(kind, payload)| eventstream::frame("event", kind, payload))
            .collect();
    }
}
fn prompt(stream: bool) -> Value {
    json!({"stream":stream,"messages":[{"role":"user","content":"Hello"}]})
}
async fn route_fixture(
    credentials: Value,
    model: &str,
    body: Value,
) -> Result<Response, ProxyError> {
    super::super::route(
        crate::models::model::AccountProvider::Kiro,
        credentials,
        model,
        body,
        &reqwest::Client::new(),
    )
    .await
}
async fn json_body(response: Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), 32 * 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn paginated_directory_keeps_exact_ids_default_names_and_account_generation_partition() {
    let fixture = Fixture::start().await;
    let mut credentials = fixture.credentials();
    credentials["profileArn"] =
        json!("arn:aws:codewhisperer:us-east-1:111111111111:profile/fixture");
    credentials["tokenType"] = json!("EXTERNAL_IDP");
    assert_eq!(
        super::super::list_models(
            crate::models::model::AccountProvider::Kiro,
            credentials.clone(),
            &reqwest::Client::new()
        )
        .await
        .unwrap(),
        ["auto", "Model/CaseId", "text-only"]
    );
    assert_eq!(
        cached_model_labels(credentials["accountId"].as_str().unwrap())["Model/CaseId"],
        "Visible model"
    );
    assert!(cached_model_labels("another-account").is_empty());
    let returned = json_body(
        route_fixture(credentials.clone(), "Model/CaseId", prompt(false))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        returned["choices"][0]["message"]["content"],
        "accepted 中文"
    );
    assert_eq!(returned["usage"], json!({}));
    {
        let seen = fixture.state.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0]["query"]["origin"], "KIRO_CLI");
        assert_eq!(seen[0]["query"]["profileArn"], credentials["profileArn"]);
        assert_eq!(seen[1]["query"]["nextToken"], "page-two");
        assert_eq!(seen[2]["tokenType"], "EXTERNAL_IDP");
        assert_eq!(seen[2]["body"]["profileArn"], credentials["profileArn"]);
        assert_eq!(
            seen[2]["body"]["conversationState"]["currentMessage"]["userInputMessage"]["modelId"],
            "Model/CaseId"
        );
        assert_eq!(seen[2]["agentMode"], "vibe");
        assert!(seen[2]["userAgent"]
            .as_str()
            .unwrap()
            .starts_with("EchoBird/"));
    }
    credentials["credentialGeneration"] = json!("generation-two");
    route_fixture(credentials.clone(), "auto", prompt(false))
        .await
        .unwrap();
    assert_eq!(fixture.state.seen.lock().unwrap().len(), 6);
    credentials["accountId"] = json!("different-account");
    route_fixture(credentials.clone(), "auto", prompt(false))
        .await
        .unwrap();
    assert_eq!(fixture.state.seen.lock().unwrap().len(), 9);
    let error = route_fixture(credentials.clone(), "model/caseid", prompt(false))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.model_scope);
    assert_eq!(fixture.state.seen.lock().unwrap().len(), 9);
    catalogs()
        .lock()
        .unwrap()
        .remove(&cache_key(&credentials).unwrap());
    assert_eq!(
        cached_model_labels(credentials["accountId"].as_str().unwrap())["Model/CaseId"],
        "Visible model"
    );
    let metadata = serde_json::to_string(&cached_model_labels(
        credentials["accountId"].as_str().unwrap(),
    ))
    .unwrap();
    assert!(!metadata.contains("fixture-access"));
    assert!(!metadata.contains("generation-two"));
    assert!(!metadata.contains("profile/"));
    assert_eq!(
        serde_json::from_str::<BTreeMap<String, String>>(&metadata).unwrap()["Model/CaseId"],
        "Visible model"
    );
}

#[tokio::test]
async fn user_system_multimodal_and_split_parallel_tool_history_roundtrip() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    let name = format!("mcp__{}__read", "long-name_".repeat(10));
    let wire_name=request::build(&json!({"messages":[{"role":"user","content":"x"}],"tools":[{"type":"function","function":{"name":name,"parameters":{"type":"object"}}}]}),&Model{id:"auto".into(),name:"Auto".into(),images:true},None).unwrap().allowed.into_iter().next().unwrap();
    fixture.events(&[
        ("assistantResponseEvent",json!({"content":"answer"})),
        ("toolUseEvent",json!({"toolUseId":"fresh-one","name":wire_name,"input":"{\"path\":"})),
        ("toolUseEvent",json!({"toolUseId":"fresh-two","name":wire_name,"input":"{\"path\":"})),
        ("toolUseEvent",json!({"toolUseId":"fresh-two","input":"\"b\"}","stop":true})),
        ("toolUseEvent",json!({"toolUseId":"fresh-one","input":"\"a\"}","stop":true})),
        ("metadataEvent",json!({"tokenUsage":{"uncachedInputTokens":2,"outputTokens":4,"totalTokens":14,"cacheReadInputTokens":3,"cacheWriteInputTokens":5}})),
    ]);
    let image = json!({"type":"image_url","image_url":{"url":"data:image/png;base64,AQID","detail":"auto"}});
    let body = json!({"messages":[
        {"role":"system","content":"System instructions"},
        {"role":"developer","content":"Developer instructions"},
        {"role":"user","content":[{"type":"text","text":"Read two files"},image.clone()]},
        {"role":"assistant","content":null,"tool_calls":[{"id":"old/one","type":"function","function":{"name":name,"arguments":"{\"path\":\"a\"}"}}]},
        {"role":"assistant","content":null,"tool_calls":[{"id":"old/two","type":"function","function":{"name":name,"arguments":"{\"path\":\"b\"}"}}]},
        {"role":"tool","tool_call_id":"old/one","content":[{"type":"text","text":"Result A"},image.clone()]},
        {"role":"tool","tool_call_id":"old/two","content":"Result B"}
        ],"tools":[{"type":"function","function":{"name":name,"description":"read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}],"max_tokens":64,"temperature":1,"parallel_tool_calls":true,"stream":false});
    let result = json_body(route_fixture(credentials, "auto", body).await.unwrap()).await;
    let calls = &result["choices"][0]["message"]["tool_calls"];
    assert_eq!(calls[0]["id"], "fresh-one");
    assert_eq!(calls[0]["function"]["name"], name);
    assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"a\"}");
    assert_eq!(calls[1]["id"], "fresh-two");
    assert_eq!(calls[1]["function"]["arguments"], "{\"path\":\"b\"}");
    assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(result["usage"]["prompt_tokens"], 10);
    assert_eq!(result["usage"]["completion_tokens"], 4);
    assert_eq!(result["usage"]["total_tokens"], 14);
    assert_eq!(result["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
    let seen = fixture.state.seen.lock().unwrap();
    let native = &seen.last().unwrap()["body"]["conversationState"];
    assert_eq!(native["history"].as_array().unwrap().len(), 2);
    assert_eq!(
        native["history"][0]["userInputMessage"]["content"],
        "System instructions\n\nDeveloper instructions\n\nRead two files"
    );
    assert_eq!(
        native["history"][0]["userInputMessage"]["images"][0]["source"]["bytes"],
        "AQID"
    );
    assert_eq!(
        native["currentMessage"]["userInputMessage"]["images"][0]["source"]["bytes"],
        "AQID"
    );
    let old_calls = &native["history"][1]["assistantResponseMessage"]["toolUses"];
    let results =
        &native["currentMessage"]["userInputMessage"]["userInputMessageContext"]["toolResults"];
    assert_eq!(old_calls[0]["toolUseId"], results[0]["toolUseId"]);
    assert_eq!(old_calls[1]["toolUseId"], results[1]["toolUseId"]);
    assert_eq!(results[0]["content"][0]["text"], "Result A");
    assert_eq!(results[1]["content"][0]["text"], "Result B");
    assert!(seen.last().unwrap()["body"].get("max_tokens").is_none());
}

#[tokio::test]
async fn stream_keeps_split_thinking_usage_and_only_reports_real_token_counts() {
    let fixture = Fixture::start().await;
    fixture.events(&[
        ("assistantResponseEvent",json!({"content":"<thin"})),
        ("assistantResponseEvent",json!({"content":"king>推理</think"})),
        ("assistantResponseEvent",json!({"content":"ing>answer"})),
        ("contextUsageEvent",json!({"contextUsagePercentage":20})),
        ("metadataEvent",json!({"stopReason":"max_tokens","tokenUsage":{"uncachedInputTokens":0,"outputTokens":0,"totalTokens":0}})),
    ]);
    let mut body = prompt(true);
    body["reasoning_effort"] = json!("medium");
    let response = route_fixture(fixture.credentials(), "auto", body)
        .await
        .unwrap();
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let returned = String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(returned.contains("reasoning_content"));
    assert!(returned.contains("推理"));
    assert!(returned.contains("answer"));
    assert!(returned.contains("\"finish_reason\":\"length\""));
    assert!(returned.contains("\"prompt_tokens\":0"));
    assert!(returned.ends_with("data: [DONE]\n\n"));
    let seen = fixture.state.seen.lock().unwrap();
    assert!(seen.last().unwrap()["body"]["conversationState"]["currentMessage"]["userInputMessage"]["content"].as_str().unwrap().starts_with("<thinking_mode>enabled</thinking_mode><max_thinking_length>20000</max_thinking_length>"));
}

#[tokio::test]
async fn early_exception_is_real_status_late_exception_stays_explicit_and_never_completes() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    for (kind, reason, status, model_scope) in [
        (
            "serviceQuotaExceededError",
            "MONTHLY_REQUEST_COUNT",
            StatusCode::TOO_MANY_REQUESTS,
            false,
        ),
        (
            "validationError",
            "CONTENT_LENGTH_EXCEEDS_THRESHOLD",
            StatusCode::BAD_REQUEST,
            true,
        ),
        (
            "serviceUnavailableError",
            "INSUFFICIENT_MODEL_CAPACITY",
            StatusCode::SERVICE_UNAVAILABLE,
            true,
        ),
        ("accessDeniedError", "", StatusCode::UNAUTHORIZED, false),
        ("throttlingError", "", StatusCode::TOO_MANY_REQUESTS, false),
    ] {
        fixture.state.reply.lock().unwrap().bytes = eventstream::frame(
            "exception",
            kind,
            &json!({"message":"refused","reason":reason}),
        );
        for streaming in [true, false] {
            let error = route_fixture(credentials.clone(), "auto", prompt(streaming))
                .await
                .unwrap_err();
            assert_eq!(error.status, status);
            assert_eq!(error.model_scope, model_scope);
        }
    }
    let mut bytes = eventstream::frame(
        "event",
        "assistantResponseEvent",
        &json!({"content":"partial"}),
    );
    bytes.extend(eventstream::frame(
        "exception",
        "serviceQuotaExceededError",
        &json!({"message":"late","reason":"MONTHLY_REQUEST_COUNT"}),
    ));
    fixture.state.reply.lock().unwrap().bytes = bytes;
    let response = route_fixture(credentials.clone(), "auto", prompt(true))
        .await
        .unwrap();
    let text = String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("partial"));
    assert!(text.contains("late"));
    assert!(text.contains("\"error\""));
    assert!(!text.contains("[DONE]"));
    assert!(!text.contains("\"finish_reason\":\"stop\""));
    assert_eq!(
        route_fixture(credentials, "auto", prompt(false))
            .await
            .unwrap_err()
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn checksum_transport_and_incomplete_tools_do_not_turn_into_success() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    let good = eventstream::frame(
        "event",
        "assistantResponseEvent",
        &json!({"content":"valid"}),
    );
    let mut bad = good.clone();
    bad[8] ^= 1;
    fixture.state.reply.lock().unwrap().bytes = bad;
    assert!(route_fixture(credentials.clone(), "auto", prompt(true))
        .await
        .unwrap_err()
        .message
        .contains("checksum"));
    fixture.state.reply.lock().unwrap().bytes = good[..good.len() - 1].to_vec();
    assert!(route_fixture(credentials.clone(), "auto", prompt(false))
        .await
        .unwrap_err()
        .message
        .contains("inside a frame"));
    fixture.events(&[(
        "toolUseEvent",
        json!({"toolUseId":"one","name":"read","input":"{\"path\":"}),
    )]);
    let mut body = prompt(false);
    body["tools"] = json!([{"type":"function","function":{"name":"read"}}]);
    assert!(route_fixture(credentials.clone(), "auto", body.clone())
        .await
        .unwrap_err()
        .message
        .contains("incomplete"));
    body["stream"] = json!(true);
    let response = route_fixture(credentials, "auto", body).await.unwrap();
    let text = String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("incomplete"));
    assert!(!text.contains("[DONE]"));
}

#[tokio::test]
async fn unsupported_constraints_fail_without_inference_and_none_omits_unused_tools() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    for (key, value) in [
        ("tool_choice", json!("required")),
        (
            "tool_choice",
            json!({"type":"function","function":{"name":"read"}}),
        ),
        ("parallel_tool_calls", json!(false)),
        ("response_format", json!({"type":"json_object"})),
        ("temperature", json!(0.2)),
        ("top_p", json!(0.5)),
        ("max_tokens", json!(0)),
        ("max_completion_tokens", json!(1.5)),
    ] {
        let mut body = prompt(false);
        body[key] = value;
        assert_eq!(
            route_fixture(credentials.clone(), "auto", body)
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    assert!(fixture.state.seen.lock().unwrap().is_empty());
    let mut body = prompt(false);
    body["tool_choice"] = json!("none");
    body["tools"] = json!([{"type":"function","function":{"name":"read","strict":true}}]);
    body["max_tokens"] = json!(1000000);
    route_fixture(credentials.clone(), "auto", body.clone())
        .await
        .unwrap();
    assert!(
        fixture.state.seen.lock().unwrap().last().unwrap()["body"]["conversationState"]
            ["currentMessage"]["userInputMessage"]
            .get("userInputMessageContext")
            .is_none()
    );
    fixture.events(&[(
        "toolUseEvent",
        json!({"toolUseId":"unexpected","name":"read","input":"{}","stop":true}),
    )]);
    assert!(route_fixture(credentials, "auto", body)
        .await
        .unwrap_err()
        .message
        .contains("not offered"));
}

#[tokio::test]
async fn model_image_capabilities_invalid_inline_data_and_http_errors_are_preserved() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    let mut body = prompt(false);
    body["messages"][0]["content"] =
        json!([{"type":"image_url","image_url":{"url":"data:image/png;base64,AQID"}}]);
    assert_eq!(
        route_fixture(credentials.clone(), "text-only", body.clone())
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    for image in [
        json!({"url":"https://example.com/image.png"}),
        json!({"url":"data:image/svg+xml;base64,AQID"}),
        json!({"url":"data:image/png;base64,invalid!"}),
        json!({"url":"data:image/png;base64,AQID","detail":"high"}),
    ] {
        body["messages"][0]["content"][0]["image_url"] = image;
        assert!(route_fixture(credentials.clone(), "auto", body.clone())
            .await
            .is_err());
    }
    assert_eq!(fixture.state.seen.lock().unwrap().len(), 2);
    *fixture.state.reply.lock().unwrap() = Reply {
        status: StatusCode::FORBIDDEN,
        content_type: "application/json",
        bytes: json!({"message":"capacity unavailable","reason":"INSUFFICIENT_MODEL_CAPACITY"})
            .to_string()
            .into_bytes(),
    };
    let error = route_fixture(credentials.clone(), "auto", prompt(false))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.model_scope);
    *fixture.state.reply.lock().unwrap() = Reply {
        status: StatusCode::OK,
        content_type: "text/event-stream",
        bytes: b"data: [DONE]\n\n".to_vec(),
    };
    assert!(route_fixture(credentials, "auto", prompt(false))
        .await
        .unwrap_err()
        .message
        .contains("did not return an event stream"));
}

#[tokio::test]
async fn malformed_repeated_or_empty_directory_and_invalid_regions_never_fabricate_models() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    fixture.state.pages.lock().unwrap()[1]["nextToken"] = json!("page-two");
    assert!(list_models(credentials.clone(), &reqwest::Client::new())
        .await
        .unwrap_err()
        .message
        .contains("pagination"));
    assert!(cached_model_labels(credentials["accountId"].as_str().unwrap()).is_empty());
    fixture.state.pages.lock().unwrap()[0] = json!({"models":[]});
    assert!(list_models(credentials.clone(), &reqwest::Client::new())
        .await
        .unwrap_err()
        .message
        .contains("no available models"));
    fixture.state.pages.lock().unwrap()[0] = json!({"models":[{"modelName":"missing ID"}]});
    assert!(list_models(credentials.clone(), &reqwest::Client::new())
        .await
        .unwrap_err()
        .message
        .contains("without an ID"));
    let count = fixture.state.seen.lock().unwrap().len();
    for region in ["evil.example/", "https://evil", "us"] {
        let mut bad = credentials.clone();
        bad["region"] = json!(region);
        assert!(list_models(bad, &reqwest::Client::new()).await.is_err());
    }
    let mut bad = credentials;
    bad["profileArn"] = json!("arn:aws:codewhisperer:eu-central-1:111111111111:profile/fixture");
    assert!(list_models(bad, &reqwest::Client::new()).await.is_err());
    assert_eq!(fixture.state.seen.lock().unwrap().len(), count);
}

#[test]
fn historical_removed_tools_missing_results_orphan_results_and_schemas_are_explicit() {
    let model = Model {
        id: "auto".into(),
        name: "Auto".into(),
        images: true,
    };
    let body = json!({"messages":[{"role":"user","content":"start"},{"role":"assistant","content":"calling","tool_calls":[{"id":"one","type":"function","function":{"name":"removed","arguments":"{}"}}]}],"tool_choice":"none"});
    let built = request::build(&body, &model, None).unwrap();
    let context = &built.body["conversationState"]["currentMessage"]["userInputMessage"]
        ["userInputMessageContext"];
    assert!(context.get("tools").is_none());
    assert_eq!(
        built.body["conversationState"]["history"][0]["userInputMessage"]
            ["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
        "removed"
    );
    assert_eq!(context["toolResults"][0]["status"], "error");
    assert!(built.allowed.is_empty());
    let mut auto = body.clone();
    auto["tool_choice"] = json!("auto");
    let built = request::build(&auto, &model, None).unwrap();
    assert!(
        built.body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]
            .get("tools")
            .is_none()
    );
    assert!(built.allowed.is_empty());
    auto["tools"] = json!([{"type":"function","function":{"name":"read"}}]);
    let built = request::build(&auto, &model, None).unwrap();
    assert_eq!(
        built.body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        built.body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
        "read"
    );
    assert_eq!(
        built.body["conversationState"]["history"][0]["userInputMessage"]
            ["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
        "removed"
    );
    assert_eq!(built.allowed, BTreeSet::from(["read".into()]));
    let orphan = json!({"messages":[{"role":"user","content":"start"},{"role":"tool","tool_call_id":"unknown","content":"result"}]});
    assert!(request::build(&orphan, &model, None).is_err());
    let mut strict = prompt(false);
    strict["tools"] = json!([{"type":"function","function":{"name":"read","strict":true}}]);
    assert!(request::build(&strict, &model, None).is_err());
    let mut collision = prompt(false);
    collision["tools"] = json!([{"type":"function","function":{"name":"same"}},{"type":"function","function":{"name":"same"}}]);
    assert!(request::build(&collision, &model, None).is_err());
}

#[tokio::test]
async fn header_only_unmodeled_errors_keep_auth_throttle_and_message_before_or_after_output() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    for (kind, status) in [
        ("UnauthorizedException", StatusCode::UNAUTHORIZED),
        ("ThrottlingException", StatusCode::TOO_MANY_REQUESTS),
    ] {
        let error = eventstream::raw_frame(
            &[
                (":message-type", "error"),
                (":error-code", kind),
                (":error-message", "header-only refusal"),
            ],
            &[],
        );
        fixture.state.reply.lock().unwrap().bytes = error.clone();
        for streaming in [true, false] {
            let failure = route_fixture(credentials.clone(), "auto", prompt(streaming))
                .await
                .unwrap_err();
            assert_eq!(failure.status, status);
            assert!(failure.message.contains("header-only refusal"));
        }
        let mut bytes = eventstream::frame(
            "event",
            "assistantResponseEvent",
            &json!({"content":"partial"}),
        );
        bytes.extend(error);
        fixture.state.reply.lock().unwrap().bytes = bytes;
        let response = route_fixture(credentials.clone(), "auto", prompt(true))
            .await
            .unwrap();
        let text = String::from_utf8(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("partial"));
        assert!(text.contains("header-only refusal"));
        assert!(!text.contains("[DONE]"));
    }
}

#[tokio::test]
async fn complete_json_without_tool_stop_is_not_a_complete_call_and_empty_stopped_call_is_valid() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    let mut body = prompt(false);
    body["tools"] = json!([{"type":"function","function":{"name":"read"}}]);
    for value in [
        json!({"toolUseId":"one","name":"read","input":"{}"}),
        json!({"toolUseId":"one","name":"read","input":"{}","stop":false}),
    ] {
        fixture.events(&[("toolUseEvent", value)]);
        assert!(route_fixture(credentials.clone(), "auto", body.clone())
            .await
            .unwrap_err()
            .message
            .contains("incomplete function call"));
    }
    fixture.events(&[(
        "toolUseEvent",
        json!({"toolUseId":"one","name":"read","input":{},"stop":true}),
    )]);
    let returned = json_body(route_fixture(credentials, "auto", body).await.unwrap()).await;
    assert_eq!(
        returned["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(returned["choices"][0]["finish_reason"], "tool_calls");
}

#[tokio::test]
async fn existing_responses_and_anthropic_conversions_roundtrip_tools_and_native_binary_response() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    fixture.events(&[
        ("assistantResponseEvent", json!({"content":"accepted"})),
        (
            "toolUseEvent",
            json!({"toolUseId":"fresh","name":"read","input":"{\"path\":\"next\"}","stop":true}),
        ),
    ]);
    let request = json!({"model":"auto","stream":false,"max_output_tokens":64,"reasoning":{"effort":"medium","summary":"auto"},"instructions":"Responses instructions","tools":[{"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}},"strict":false}],"input":[
        {"type":"message","role":"user","content":[{"type":"input_text","text":"Read a file"}]},
        {"type":"function_call","call_id":"previous","name":"read","arguments":"{\"path\":\"old\"}"},
        {"type":"function_call_output","call_id":"previous","output":"previous output"}
    ]});
    let mut chat = super::super::responses_to_chat(&request).unwrap();
    super::super::strip_response_metadata(&mut chat);
    let response = route_fixture(credentials.clone(), "auto", chat)
        .await
        .unwrap();
    let response = super::super::chat_to_responses_with_tools(response, false, &request).await;
    let returned = json_body(response).await;
    assert_eq!(returned["object"], "response");
    assert_eq!(returned["status"], "completed");
    let call = returned["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(call["name"], "read");
    assert_eq!(call["call_id"], "fresh");
    assert_eq!(call["arguments"], "{\"path\":\"next\"}");
    let anthropic = json!({"model":"auto","max_tokens":4096,"system":"Anthropic instructions","tools":[{"name":"read","input_schema":{"type":"object","properties":{"path":{"type":"string"}}}}],"tool_choice":{"type":"auto"},"messages":[
        {"role":"user","content":"Read a file"},
        {"role":"assistant","content":[{"type":"tool_use","id":"previous","name":"read","input":{"path":"old"}}]},
        {"role":"user","content":[{"type":"tool_result","tool_use_id":"previous","content":"previous output"}]}
    ]});
    let chat = crate::services::local_llm::proxy::anthropic_to_openai(&anthropic);
    let returned = json_body(route_fixture(credentials, "auto", chat).await.unwrap()).await;
    let returned = crate::services::local_llm::proxy::openai_to_anthropic(&returned);
    assert_eq!(returned["type"], "message");
    let call = returned["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "tool_use")
        .unwrap();
    assert_eq!(call["id"], "fresh");
    assert_eq!(call["name"], "read");
    assert_eq!(call["input"]["path"], "next");
    let seen = fixture.state.seen.lock().unwrap();
    for request in seen
        .iter()
        .filter(|request| request["path"] == "/generateAssistantResponse")
    {
        let native = &request["body"];
        assert!(native.get("_echobird_responses").is_none());
        assert!(native.get("max_tokens").is_none());
        assert_eq!(
            native["conversationState"]["currentMessage"]["userInputMessage"]
                ["userInputMessageContext"]["toolResults"][0]["content"][0]["text"],
            "previous output"
        );
    }
}

#[tokio::test]
async fn null_or_missing_usage_stays_unknown_and_actual_zero_stays_zero() {
    let fixture = Fixture::start().await;
    let credentials = fixture.credentials();
    for (native, expected) in [
        (
            json!({"uncachedInputTokens":null,"outputTokens":null,"totalTokens":null,"cacheReadInputTokens":null,"cacheWriteInputTokens":null}),
            json!({}),
        ),
        (
            json!({"uncachedInputTokens":0,"outputTokens":1,"totalTokens":1,"cacheReadInputTokens":null,"cacheWriteInputTokens":null}),
            json!({"prompt_tokens":0,"completion_tokens":1,"total_tokens":1}),
        ),
        (
            json!({"outputTokens":7,"cacheReadInputTokens":null}),
            json!({"completion_tokens":7}),
        ),
    ] {
        fixture.events(&[
            ("assistantResponseEvent", json!({"content":"answer"})),
            ("metadataEvent", json!({"tokenUsage":native})),
        ]);
        let returned = json_body(
            route_fixture(credentials.clone(), "auto", prompt(false))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(returned["usage"], expected);
    }
}
