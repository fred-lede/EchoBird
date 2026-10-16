use super::convert::{self, ChatSseAdapter};
use crate::services::{codex_catalog, tool_config_manager::ModelInfo};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use futures_util::{stream::BoxStream, StreamExt};
use serde_json::{json, Value};
use std::{collections::VecDeque, convert::Infallible, path::Path, path::PathBuf, time::Duration};

#[derive(Clone)]
struct AppState {
    http: reqwest::Client,
    relay_path: PathBuf,
}

pub(super) fn router(relay_path: PathBuf) -> Result<Router, String> {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .tcp_keepalive(Duration::from_secs(60))
        .build()
        .map_err(|error| error.to_string())?;
    Ok(Router::new()
        .route("/v1/muse-code/models", get(models))
        .route("/muse-code/models", get(models))
        .route("/v1/models", get(models))
        .route("/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/responses", post(responses))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(AppState { http, relay_path }))
}

pub(super) async fn run(listener: std::net::TcpListener, app: Router) -> Result<(), String> {
    let listener =
        tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
    axum::serve(listener, app)
        .await
        .map_err(|error| error.to_string())
}

fn read_relay(path: &Path) -> Result<ModelInfo, String> {
    let bytes = std::fs::read(path).map_err(|_| "No active Muse model configured in EchoBird")?;
    let relay: ModelInfo =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid Muse model configuration")?;
    if relay.model.as_deref().unwrap_or("").is_empty()
        || relay.base_url.as_deref().unwrap_or("").is_empty()
        || relay.protocol.as_deref() != Some("openai")
    {
        return Err("Invalid Muse model configuration".into());
    }
    Ok(relay)
}

async fn models(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return failure(StatusCode::FORBIDDEN, "Browser requests are not accepted");
    }
    match read_relay(&state.relay_path) {
        Ok(relay) => Json(catalog(&relay)).into_response(),
        Err(error) => failure(StatusCode::SERVICE_UNAVAILABLE, &error),
    }
}

fn catalog(relay: &ModelInfo) -> Value {
    let model = relay.model.as_deref().unwrap_or("");
    let template =
        codex_catalog::template_for_model(relay.base_url.as_deref().unwrap_or(""), model);
    let documented = template
        .as_ref()
        .filter(|template| template["slug"] == model);
    let context = documented
        .and_then(|template| template["context_window"].as_u64())
        .unwrap_or(32_768);
    let input: Vec<Value> = documented
        .and_then(|template| template["input_modalities"].as_array())
        .map(|modalities| {
            modalities
                .iter()
                .filter(|value| matches!(value.as_str(), Some("text" | "image")))
                .cloned()
                .collect()
        })
        .filter(|modalities: &Vec<Value>| !modalities.is_empty())
        .unwrap_or_else(|| vec![json!("text")]);
    json!({
        "object": "list",
        "data": [{
            "id": model,
            "object": "model",
            "created": 0,
            "owned_by": "echobird",
            "metadata": { "muse-code": {
                "name": relay.name.as_deref().unwrap_or(model),
                "family": "echobird",
                "is_hidden": false,
                "attachment": input.iter().any(|value| value == "image"),
                "reasoning": false,
                "temperature": false,
                "tool_call": true,
                "modalities": { "input": input, "output": ["text"] },
                "limit": { "context": context, "output": 4096 },
                "options": { "include": [] },
                "variants": {}
            } }
        }]
    })
}

fn upstream_url(base: &str) -> Result<url::Url, String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid model Base URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Invalid model Base URL".into());
    }
    let path = url.path().trim_end_matches('/');
    if !path.ends_with("/chat/completions") {
        url.set_path(&format!("{path}/chat/completions"));
    }
    Ok(url)
}

async fn responses(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return failure(StatusCode::FORBIDDEN, "Browser requests are not accepted");
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return failure(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "Invalid Responses JSON"),
    };
    let relay = match read_relay(&state.relay_path) {
        Ok(relay) => relay,
        Err(error) => return failure(StatusCode::SERVICE_UNAVAILABLE, &error),
    };
    let namespaces = match convert::tool_namespaces(&request) {
        Ok(namespaces) => namespaces,
        Err(error) => return failure(StatusCode::BAD_REQUEST, &error),
    };
    let mut chat = match convert::responses_to_chat(&request) {
        Ok(chat) => chat,
        Err(error) => return failure(StatusCode::BAD_REQUEST, &error),
    };
    let model = relay.model.as_deref().unwrap_or("");
    chat["model"] = json!(model);
    let url = match upstream_url(relay.base_url.as_deref().unwrap_or("")) {
        Ok(url) => url,
        Err(error) => return failure(StatusCode::SERVICE_UNAVAILABLE, &error),
    };
    let stream = request["stream"].as_bool().unwrap_or(false);
    let mut upstream = state
        .http
        .post(url)
        .header(
            header::ACCEPT,
            if stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .json(&chat);
    if let Some(key) = relay.api_key.as_deref().filter(|key| !key.is_empty()) {
        upstream = upstream.bearer_auth(key);
    }
    let response = match upstream.send().await {
        Ok(response) => response,
        Err(error) => return failure(StatusCode::BAD_GATEWAY, &error.to_string()),
    };
    if !response.status().is_success() {
        let status = response.status();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(error) => return failure(StatusCode::BAD_GATEWAY, &error.to_string()),
        };
        return (status, [(header::CONTENT_TYPE, "application/json")], body).into_response();
    }
    if stream {
        return streaming_response(response, ChatSseAdapter::new(model, namespaces));
    }
    let data: Value = match response.json().await {
        Ok(data) => data,
        Err(error) => return failure(StatusCode::BAD_GATEWAY, &error.to_string()),
    };
    match convert::chat_to_response(&data, model, &namespaces) {
        Ok(response) => Json(response).into_response(),
        Err(error) => failure(StatusCode::BAD_GATEWAY, &error),
    }
}

struct StreamState {
    upstream: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    adapter: ChatSseAdapter,
    pending: VecDeque<Bytes>,
    ended: bool,
}

fn streaming_response(response: reqwest::Response, adapter: ChatSseAdapter) -> Response {
    // This stream owns the upstream. Dropping the client's body cancels its read.
    let stream = futures_util::stream::unfold(
        StreamState {
            upstream: response.bytes_stream().boxed(),
            adapter,
            pending: VecDeque::new(),
            ended: false,
        },
        |mut state| async move {
            loop {
                if let Some(chunk) = state.pending.pop_front() {
                    return Some((Ok::<_, Infallible>(chunk), state));
                }
                if state.ended || state.adapter.is_finished() {
                    return None;
                }
                match tokio::time::timeout(Duration::from_secs(300), state.upstream.next()).await {
                    Ok(Some(Ok(chunk))) => match state.adapter.push(&chunk) {
                        Ok(events) => state.pending.extend(events),
                        Err(error) => {
                            state.pending.push_back(state.adapter.failed(&error));
                            state.ended = true;
                        }
                    },
                    Ok(None) => {
                        match state.adapter.finish() {
                            Ok(events) => state.pending.extend(events),
                            Err(error) => state.pending.push_back(state.adapter.failed(&error)),
                        }
                        state.ended = true;
                    }
                    Ok(Some(Err(error))) => {
                        state
                            .pending
                            .push_back(state.adapter.failed(&error.to_string()));
                        state.ended = true;
                    }
                    Err(_) => {
                        state
                            .pending
                            .push_back(state.adapter.failed("Upstream stream stalled"));
                        state.ended = true;
                    }
                }
            }
        },
    );
    (
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

fn failure(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({"error":{"message":message,"type":"invalid_request_error"}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (address, task)
    }

    fn relay_file(root: &Path, base: &str, model: &str) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join("musecode.json");
        std::fs::write(&path, json!({"model":model,"baseUrl":base,"apiKey":"fixture-key","name":"Fixture","protocol":"openai"}).to_string()).unwrap();
        path
    }

    #[tokio::test]
    async fn catalog_is_local_and_each_request_reads_the_current_model() {
        let requests = Arc::new(Mutex::new(Vec::<(HeaderMap, Value)>::new()));
        let captured = requests.clone();
        let (upstream, upstream_task) = serve(Router::new().route("/v1/chat/completions", post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let captured = captured.clone();
            async move {
                captured.lock().unwrap().push((headers, body.clone()));
                Json(json!({"choices":[{"message":{"content":"fixture"},"finish_reason":"stop"}]}))
            }
        }))).await;
        let root =
            std::env::temp_dir().join(format!("echobird-muse-proxy-{}", uuid::Uuid::new_v4()));
        let path = relay_file(&root, &format!("{upstream}/v1"), "first");
        let (proxy, proxy_task) = serve(router(path.clone()).unwrap()).await;
        let http = reqwest::Client::new();
        for route in [
            "/v1/muse-code/models",
            "/muse-code/models",
            "/v1/models",
            "/models",
        ] {
            let catalog: Value = http
                .get(format!("{proxy}{route}"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(catalog["object"], "list");
            assert_eq!(catalog["data"][0]["id"], "first");
            assert_eq!(
                catalog["data"][0]["metadata"]["muse-code"]["limit"]["context"],
                32768
            );
        }
        assert!(requests.lock().unwrap().is_empty());
        let request = json!({"model":"native-default","input":"hello","stream":false});
        let browser = http
            .post(format!("{proxy}/v1/responses"))
            .header(header::ORIGIN, "https://example.com")
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(browser.status(), StatusCode::FORBIDDEN);
        let plain = http
            .post(format!("{proxy}/v1/responses"))
            .header(header::CONTENT_TYPE, "text/plain")
            .body(request.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(plain.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let browser_catalog = http
            .get(format!("{proxy}/models"))
            .header(header::ORIGIN, "https://example.com")
            .send()
            .await
            .unwrap();
        assert_eq!(browser_catalog.status(), StatusCode::FORBIDDEN);
        assert!(requests.lock().unwrap().is_empty());
        let response: Value = http
            .post(format!("{proxy}/v1/responses"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["model"], "first");
        relay_file(&root, &format!("{upstream}/v1"), "second");
        let response: Value = http
            .post(format!("{proxy}/responses"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["model"], "second");
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0].0[header::AUTHORIZATION], "Bearer fixture-key");
        assert_eq!(captured[0].1["model"], "first");
        assert_eq!(captured[1].1["model"], "second");
        assert!(captured[0].1.get("input").is_none());
        assert_eq!(captured[0].1["messages"][0]["content"], "hello");
        drop(captured);
        proxy_task.abort();
        upstream_task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upstream_urls_keep_existing_endpoint_and_query() {
        assert_eq!(
            upstream_url("https://example.com/v1/").unwrap().as_str(),
            "https://example.com/v1/chat/completions"
        );
        assert_eq!(
            upstream_url("https://example.com/chat/completions?api-version=test")
                .unwrap()
                .as_str(),
            "https://example.com/chat/completions?api-version=test"
        );
        assert!(upstream_url("file:///fixture").is_err());
    }

    #[tokio::test]
    async fn upstream_failures_are_visible_and_truncated_streams_never_complete() {
        let (upstream, upstream_task) = serve(Router::new()
            .route("/denied/chat/completions", post(|| async { (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":"fixture denied"}}))) }))
            .route("/truncated/chat/completions", post(|| async {
                ([(header::CONTENT_TYPE, "text/event-stream")], "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n")
            }))).await;
        let root =
            std::env::temp_dir().join(format!("echobird-muse-errors-{}", uuid::Uuid::new_v4()));
        let path = relay_file(&root, &format!("{upstream}/denied"), "fixture");
        let (proxy, proxy_task) = serve(router(path).unwrap()).await;
        let http = reqwest::Client::new();
        let denied = http
            .post(format!("{proxy}/v1/responses"))
            .json(&json!({"input":"hello","stream":false}))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            denied.json::<Value>().await.unwrap()["error"]["message"],
            "fixture denied"
        );
        relay_file(&root, &format!("{upstream}/truncated"), "fixture");
        let response = http
            .post(format!("{proxy}/v1/responses"))
            .json(&json!({"input":"hello","stream":true}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(response.contains("response.failed"));
        assert!(!response.contains("response.completed"));
        proxy_task.abort();
        upstream_task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }

    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn client_disconnect_drops_the_upstream_stream() {
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = dropped.clone();
        let (upstream, upstream_task) = serve(Router::new().route("/chat/completions", post(move || {
            let guard = DropSignal(signal.clone());
            async move {
                let body = futures_util::stream::unfold((false, guard), |(sent, guard)| async move {
                    if !sent {
                        Some((Ok::<_, Infallible>(Bytes::from_static(b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n")), (true, guard)))
                    } else {
                        std::future::pending().await
                    }
                });
                ([(header::CONTENT_TYPE, "text/event-stream")], Body::from_stream(body))
            }
        }))).await;
        let root =
            std::env::temp_dir().join(format!("echobird-muse-cancel-{}", uuid::Uuid::new_v4()));
        let path = relay_file(&root, &upstream, "fixture");
        let (proxy, proxy_task) = serve(router(path).unwrap()).await;
        let response = reqwest::Client::new()
            .post(format!("{proxy}/responses"))
            .json(&json!({"input":"hello","stream":true}))
            .send()
            .await
            .unwrap();
        let mut stream = response.bytes_stream();
        assert!(stream.next().await.unwrap().is_ok());
        assert!(!dropped.load(Ordering::SeqCst));
        drop(stream);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        proxy_task.abort();
        upstream_task.abort();
        std::fs::remove_dir_all(root).unwrap();
    }
}
