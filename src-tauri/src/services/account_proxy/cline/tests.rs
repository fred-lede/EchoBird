use super::*;
use crate::services::minimax_accounts::test_support::{json as response_json, MockServer};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, HeaderMap},
    response::Response,
    routing::any,
    Router,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

#[tokio::test]
async fn free_metadata_uses_the_account_directory_and_preserves_it_on_failure() {
    let catalog = json!({"data":[
        {"id":"native/flag-free","free":true},
        {"id":"native/string-free","free":"true"},
        {"id":"native/paid","free":false,"pricing":{"prompt":"0.01","completion":"0.02"}},
        {"id":"native/unknown-free"},
        {"id":"native/malformed","free":" true "},
        {"id":"native/denied-recommendation"},
        {"id":"native/disabled","free":true,"disabled":true},
        {"id":"native/image","free":true,"architecture":{"output_modalities":["image"]}},
        {"id":"native/no-api","free":true,"supported_in_api":false},
        {"id":"cline-cloud/exclusive","free":true}
    ]});
    let recommended = json!({"recommended":[{"id":"native/recommended-flag","free":true}],
        "clinePass":[{"id":"native/pass-flag","free":"true"}],"free":[
        {"id":"native/group-free"},
        {"id":"native/flag-free"},
        {"id":"native/disabled"},
        {"id":"native/image"},
        {"id":"native/no-api"},
        {"id":"native/denied-recommendation","disabled":true},
        {"id":"cline-cloud/exclusive"}
    ]});
    let state = Arc::new(Mutex::new((StatusCode::OK, catalog, recommended)));
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = calls.clone();
    let directory = state.clone();
    let server = MockServer::start(move |request| {
        requests.fetch_add(1, Ordering::SeqCst);
        let (status, catalog, recommended) = directory.lock().unwrap().clone();
        async move {
            assert_eq!(
                request.headers[header::AUTHORIZATION],
                "Bearer workos:fixture"
            );
            let body = match request.path.as_str() {
                "/api/v1/ai/cline/models" => catalog,
                "/api/v1/ai/cline/recommended-models" => recommended,
                _ => panic!("Unexpected directory request"),
            };
            response_json(status.as_u16(), body)
        }
    })
    .await;
    let account_id = uuid::Uuid::new_v4().to_string();
    let credentials = json!({"accessToken":"workos:fixture","baseUrl":server.url,
        "_echobird_saved_account_id":account_id});
    let client = reqwest::Client::new();
    let models = list_models(credentials.clone(), &client).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        models,
        [
            "native/denied-recommendation",
            "native/flag-free",
            "native/group-free",
            "native/malformed",
            "native/paid",
            "native/pass-flag",
            "native/recommended-flag",
            "native/string-free",
            "native/unknown-free"
        ]
    );
    let expected = [
        "native/flag-free",
        "native/group-free",
        "native/pass-flag",
        "native/recommended-flag",
        "native/string-free",
    ];
    assert_eq!(
        providers::cached_free_model_ids(AccountProvider::Cline, &account_id),
        expected
    );
    assert!(providers::cached_free_model_ids(AccountProvider::Kilo, &account_id).is_empty());
    assert!(providers::cached_free_model_ids(AccountProvider::Cline, "another-account").is_empty());
    state.lock().unwrap().0 = StatusCode::INTERNAL_SERVER_ERROR;
    assert!(list_models(credentials.clone(), &client).await.is_err());
    assert_eq!(
        providers::cached_free_model_ids(AccountProvider::Cline, &account_id),
        expected
    );
    *state.lock().unwrap() = (StatusCode::OK, json!({"data":[]}), json!({"free":[]}));
    assert!(list_models(credentials.clone(), &client).await.is_err());
    assert_eq!(
        providers::cached_free_model_ids(AccountProvider::Cline, &account_id),
        expected
    );
    *state.lock().unwrap() = (
        StatusCode::OK,
        json!({"data":[{"id":"native/paid"}]}),
        json!({"free":[]}),
    );
    list_models(credentials, &client).await.unwrap();
    assert!(providers::cached_free_model_ids(AccountProvider::Cline, &account_id).is_empty());
}

#[tokio::test]
async fn catalog_preserves_native_namespaces_and_chat_preserves_tools_and_errors() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    async fn gateway(State(seen): State<Arc<Mutex<Vec<Value>>>>, request: Request) -> Response {
        if request
            .headers()
            .get(header::AUTHORIZATION)
            .is_some_and(|token| token == "Bearer workos:expired")
        {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::from("expired"))
                .unwrap();
        }
        assert_eq!(
            request.headers()[header::AUTHORIZATION],
            "Bearer workos:fixture"
        );
        let path = request.uri().path().to_string();
        if path == "/api/v1/ai/cline/models" {
            return super::super::json_response(&json!({"data":[
                {"id":"vendor/chat","architecture":{"output_modalities":["text"]}},
                {"id":"vendor/image","architecture":{"output_modalities":["text","image"]}},
                {"id":"vendor/embedding","architecture":{"output_modalities":["embedding"]}},
                {"id":"cline-cloud/exclusive"}]}));
        }
        if path == "/api/v1/ai/cline/recommended-models" {
            return super::super::json_response(&json!({"recommended":[{"id":"vendor/chat"}],
                "free":[{"id":"cline-free/native/free"},{"id":"vendor/image"}],"clinePass":[{"id":"cline-pass/native"}],
                "clineCloud":[{"id":"cloud-only"}]}));
        }
        assert_eq!(path, "/api/v1/chat/completions");
        let body: Value =
            serde_json::from_slice(&to_bytes(request.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        seen.lock().unwrap().push(body.clone());
        if body["model"] == "rejected" {
            let mut response = super::super::json_response(
                &json!({"error":{"message":"Unknown model","code":"model_not_found"}}),
            );
            *response.status_mut() = StatusCode::NOT_FOUND;
            return response;
        }
        Response::builder().header(header::CONTENT_TYPE,"text/event-stream").body(Body::from(concat!(
            "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"accepted\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"))).unwrap()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .fallback(any(gateway))
        .with_state(seen.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let credentials = json!({"accessToken":"workos:fixture","baseUrl":base});
    assert_eq!(
        list_models(credentials.clone(), &client).await.unwrap(),
        ["cline-free/native/free", "cline-pass/native", "vendor/chat"]
    );
    let expired = list_models(
        json!({"accessToken":"workos:expired","baseUrl":base}),
        &client,
    )
    .await
    .unwrap_err();
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    let body = json!({"stream":false,"messages":[{"role":"user","content":"original"},{"role":"assistant","content":null,
        "tool_calls":[{"id":"call_fixture","type":"function","function":{"name":"probe","arguments":"{}"}}]},
        {"role":"tool","tool_call_id":"call_fixture","content":"private result"}],
        "tools":[{"type":"function","function":{"name":"probe","parameters":{"type":"object"}}}],"tool_choice":"none",
        "temperature":0.3,"_echobird_responses":{"input":"must not leak"}});
    let route_credentials =
        json!({"accessToken":"workos:fixture","baseUrl":format!("{base}/api/v1")});
    let response = super::super::route(
        crate::models::model::AccountProvider::Cline,
        route_credentials.clone(),
        "cline-free/native/free",
        body.clone(),
        &client,
    )
    .await
    .unwrap();
    let returned: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(returned["choices"][0]["message"]["content"], "accepted");
    let mut expected = body.clone();
    expected
        .as_object_mut()
        .unwrap()
        .remove("_echobird_responses");
    expected["stream"] = json!(true);
    expected["model"] = json!("cline-free/native/free");
    assert_eq!(seen.lock().unwrap()[0], expected);
    let mut streaming = body.clone();
    streaming["stream"] = json!(true);
    let response = super::super::route(
        crate::models::model::AccountProvider::Cline,
        route_credentials.clone(),
        "cline-pass/native",
        streaming,
        &client,
    )
    .await
    .unwrap();
    let returned = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(std::str::from_utf8(&returned)
        .unwrap()
        .contains("data: [DONE]"));
    let error = super::super::route(
        crate::models::model::AccountProvider::Cline,
        route_credentials,
        "rejected",
        body,
        &client,
    )
    .await
    .unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.model_scope);
    assert_eq!(
        providers::upstream_error(
            StatusCode::UNAUTHORIZED,
            &HeaderMap::new(),
            &json!({"error":{"message":"Expired"}})
        )
        .status,
        StatusCode::UNAUTHORIZED
    );
    task.abort();
}
