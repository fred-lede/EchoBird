//! Account credentials stay in the backend. These adapters never switch native clients.

mod bridge;
mod cline;
mod grok;
mod kiro;
mod minimax;
mod protocol;
mod providers;
mod qoder;
mod responses;
mod tools;
mod trae;
mod zcode;

use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode},
    response::Response,
};
use serde_json::{json, Value};

use crate::models::model::AccountProvider;

pub(crate) use protocol::SseParser;
pub use providers::responses_response;
pub(crate) use providers::{context_limit_error, stream_error};
pub use responses::{chat_to_responses, chat_to_responses_with_tools, responses_to_chat};
pub(crate) use responses::{normalize_responses_tool_rounds, GoogleThought};

pub(crate) fn chat_request_to_responses(body: &Value, model: &str) -> Value {
    protocol::chat_to_responses(body, model)
}

pub(crate) async fn convert_public_model_stream(
    response: reqwest::Response,
    model: &str,
    streaming: bool,
    responses: bool,
) -> Result<Response, ProxyError> {
    let protocol = if responses {
        protocol::WireProtocol::Responses
    } else {
        protocol::WireProtocol::Chat
    };
    providers::converted(response, protocol, model, streaming, true).await
}

pub(crate) fn forget_preparation_metadata(provider: AccountProvider, account_id: &str) {
    providers::forget_metadata(provider, account_id);
}

pub(crate) fn cached_qoder_model_labels(
    provider: AccountProvider,
    account_id: &str,
) -> std::collections::BTreeMap<String, String> {
    qoder::cached_model_labels(provider, account_id)
}

pub(crate) fn cached_kiro_model_labels(
    account_id: &str,
) -> std::collections::BTreeMap<String, String> {
    kiro::cached_model_labels(account_id)
}

pub(crate) fn cached_free_model_ids(provider: AccountProvider, account_id: &str) -> Vec<String> {
    providers::cached_free_model_ids(provider, account_id)
}

pub(crate) fn cached_qoder_free_model_ids(
    provider: AccountProvider,
    account_id: &str,
) -> Vec<String> {
    qoder::cached_free_model_ids(provider, account_id)
}

/// Internal Responses state belongs only on a Responses-capable account hop.
pub fn strip_response_metadata(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        object.remove(tools::RESPONSES_REQUEST);
    }
    for message in body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(object) = message.as_object_mut() {
            object.remove("_echobird_response_items");
        }
        for call in message
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if let Some(object) = call.as_object_mut() {
                object.remove("_echobird_response_item_id");
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyError {
    pub status: StatusCode,
    pub message: String,
    pub error_code: Option<String>,
    pub error_type: Option<String>,
    pub retry_after: Option<HeaderValue>,
    pub model_scope: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProxyFailureMetadata {
    pub model_scope: bool,
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProxyError {}

impl ProxyError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            error_code: None,
            error_type: None,
            retry_after: None,
            model_scope: false,
        }
    }

    pub fn into_response(self) -> Response {
        let mut error = json!({"message":self.message,"type":self.error_type.as_deref().unwrap_or("upstream_error")});
        if let Some(code) = self.error_code {
            error["code"] = json!(code);
        }
        let mut response = json_response(&json!({"error":error}));
        *response.status_mut() = self.status;
        if let Some(retry_after) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, retry_after);
        }
        response.extensions_mut().insert(ProxyFailureMetadata {
            model_scope: self.model_scope,
        });
        response
    }

    pub(crate) fn upstream(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            format!("Account upstream failed: {error}"),
        )
    }
}

pub async fn route(
    provider: AccountProvider,
    credentials: Value,
    model: &str,
    body: Value,
    client: &reqwest::Client,
) -> Result<Response, ProxyError> {
    if provider == AccountProvider::Grok {
        grok::route(credentials, model, body, client).await
    } else if provider == AccountProvider::Kiro {
        kiro::route(credentials, model, body, client).await
    } else if provider == AccountProvider::Minimax {
        minimax::route(credentials, model, body, client).await
    } else if matches!(provider, AccountProvider::Qoder | AccountProvider::QoderCn) {
        qoder::route(provider, credentials, model, body, client).await
    } else if matches!(provider, AccountProvider::Trae | AccountProvider::Traecn) {
        trae::route(provider, credentials, model, body, client).await
    } else if provider == AccountProvider::ClaudeCode {
        bridge::route(credentials, model, body).await
    } else if provider == AccountProvider::Zcode {
        zcode::route(credentials, model, body, client).await
    } else {
        providers::route(provider, credentials, model, body, client).await
    }
}

pub async fn route_responses(
    provider: AccountProvider,
    credentials: Value,
    model: &str,
    body: Value,
    client: &reqwest::Client,
) -> Result<Option<Response>, ProxyError> {
    if provider == AccountProvider::Grok {
        return grok::route_responses(credentials, model, body, client)
            .await
            .map(Some);
    }
    providers::route_responses(provider, credentials, model, body, client).await
}

pub async fn list_models(
    provider: AccountProvider,
    credentials: Value,
    client: &reqwest::Client,
) -> Result<Vec<String>, ProxyError> {
    if provider == AccountProvider::Grok {
        grok::list_models(credentials, client).await
    } else if provider == AccountProvider::Kiro {
        kiro::list_models(credentials, client).await
    } else if provider == AccountProvider::Cline {
        cline::list_models(credentials, client).await
    } else if provider == AccountProvider::Minimax {
        minimax::list_models(credentials, client).await
    } else if matches!(provider, AccountProvider::Qoder | AccountProvider::QoderCn) {
        qoder::list_models(provider, credentials, client).await
    } else if matches!(provider, AccountProvider::Trae | AccountProvider::Traecn) {
        trae::list_models(provider, credentials, client).await
    } else if provider == AccountProvider::ClaudeCode {
        bridge::list_models(credentials).await
    } else if provider == AccountProvider::Zcode {
        zcode::list_models(credentials, client).await
    } else {
        providers::list_models(provider, credentials, client).await
    }
}

pub(crate) fn json_response(value: &Value) -> Response {
    Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(value.to_string()))
        .expect("fixed response headers")
}

pub(crate) fn stream_response(body: Body) -> Response {
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(body)
        .expect("fixed response headers")
}
