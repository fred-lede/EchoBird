use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, Request},
    response::Response,
    Router,
};
use std::{future::Future, path::PathBuf, sync::Arc};

pub(crate) struct RequestData {
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: String,
}
pub(crate) struct MockServer {
    pub(crate) url: String,
    task: tokio::task::JoinHandle<()>,
}
impl MockServer {
    pub(crate) async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn(RequestData) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handler = Arc::new(handler);
        let router = Router::new().fallback(move |request: Request<Body>| {
            let handler = handler.clone();
            async move {
                let (parts, body) = request.into_parts();
                let request = RequestData {
                    path: parts.uri.path().into(),
                    query: parts.uri.query().unwrap_or_default().into(),
                    headers: parts.headers,
                    body: String::from_utf8(to_bytes(body, 1_048_576).await.unwrap().to_vec())
                        .unwrap(),
                };
                handler(request).await
            }
        });
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, task }
    }
}
impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct Fixture(pub(crate) PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("echobird-minimax-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        Self(directory)
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.0.join("accounts.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn json(status: u16, value: serde_json::Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .unwrap()
}
pub(crate) fn jwt(user: &str, edition: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    format!(
        "e30.{}.fixture",
        URL_SAFE_NO_PAD.encode(serde_json::json!({"sub":user,"generation":edition}).to_string())
    )
}
