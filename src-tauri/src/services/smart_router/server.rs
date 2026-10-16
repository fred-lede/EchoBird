use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{stream, StreamExt};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_stream::wrappers::ReceiverStream;

use super::{
    api_key_for_use, candidate_ids, request_config_for_use, PublicActivity,
    SMART_ROUTER_INTERNAL_ID, SMART_ROUTER_MODEL_ID,
};
use crate::models::model::AccountProvider;
use crate::services::{free_model_directory, model_manager};
use crate::utils::platform::echobird_dir;

#[path = "zen_free.rs"]
mod zen_free;

const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_STREAM_PREFLIGHT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BINDINGS: usize = 512;
const RESPONSE_BINDING_LIFETIME_MS: u64 = 24 * 60 * 60 * 1_000;
const RESPONSES_CAPABILITY_LIFETIME_MS: u64 = 5 * 60 * 1_000;
pub const MAX_ROUTE_CANDIDATES: usize = 20;
const FALLBACK_TIME_BUDGET: Duration = Duration::from_secs(45);
const UPSTREAM_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const UPSTREAM_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSIENT_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_TRANSIENT_COOLDOWN: Duration = Duration::from_secs(5 * 60);
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(60);
const AUTH_OR_BILLING_COOLDOWN: Duration = Duration::from_secs(60 * 60);
const MODEL_UNAVAILABLE_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

static ROUTE_MEMORY: OnceLock<Arc<Mutex<RouteMemory>>> = OnceLock::new();
static ROUTE_ACTIVITY: OnceLock<Mutex<RouteActivity>> = OnceLock::new();

#[derive(Default)]
struct RouteActivity {
    next_attempt_id: u64,
    active_attempts: BTreeMap<u64, String>,
    last_candidate_id: Option<String>,
    sequence: u64,
    updated_at_ms: u64,
}

impl RouteActivity {
    fn begin(&mut self, candidate_id: &str, updated_at_ms: u64) -> u64 {
        self.next_attempt_id = self.next_attempt_id.saturating_add(1);
        let attempt_id = self.next_attempt_id;
        self.active_attempts
            .insert(attempt_id, candidate_id.to_string());
        self.last_candidate_id = Some(candidate_id.to_string());
        self.sequence = self.sequence.saturating_add(1);
        self.updated_at_ms = updated_at_ms;
        attempt_id
    }

    fn end(&mut self, attempt_id: u64, updated_at_ms: u64) {
        if self.active_attempts.remove(&attempt_id).is_none() {
            return;
        }
        if let Some((_, candidate_id)) = self.active_attempts.last_key_value() {
            self.last_candidate_id = Some(candidate_id.clone());
        }
        self.sequence = self.sequence.saturating_add(1);
        self.updated_at_ms = updated_at_ms;
    }

    fn snapshot(&self) -> PublicActivity {
        PublicActivity {
            candidate_id: self.last_candidate_id.clone(),
            active: !self.active_attempts.is_empty(),
            sequence: self.sequence,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

struct RouteActivityGuard {
    attempt_id: Option<u64>,
}

impl RouteActivityGuard {
    fn begin(candidate_id: &str) -> Self {
        let attempt_id = ROUTE_ACTIVITY
            .get_or_init(|| Mutex::new(RouteActivity::default()))
            .lock()
            .ok()
            .map(|mut activity| activity.begin(candidate_id, now_ms()));
        Self { attempt_id }
    }
}

impl Drop for RouteActivityGuard {
    fn drop(&mut self) {
        let Some(attempt_id) = self.attempt_id else {
            return;
        };
        let Some(activity) = ROUTE_ACTIVITY.get() else {
            return;
        };
        if let Ok(mut activity) = activity.lock() {
            activity.end(attempt_id, now_ms());
        }
    }
}

pub(crate) fn public_activity() -> PublicActivity {
    ROUTE_ACTIVITY
        .get_or_init(|| Mutex::new(RouteActivity::default()))
        .lock()
        .map(|activity| activity.snapshot())
        .unwrap_or(PublicActivity {
            candidate_id: None,
            active: false,
            sequence: 0,
            updated_at_ms: 0,
        })
}

#[derive(Clone)]
struct AppState {
    http_client: reqwest::Client,
    route_memory: Arc<Mutex<RouteMemory>>,
    route_memory_path: Option<PathBuf>,
    #[cfg(test)]
    fixture_responses: Option<Arc<Mutex<HashMap<String, Response>>>>,
    #[cfg(test)]
    fixture_authentication: Option<Arc<Mutex<AuthenticationFixture>>>,
}

#[cfg(test)]
struct AuthenticationFixture {
    result: Result<Value, String>,
    delay: Duration,
    rejections: Vec<(crate::models::model::AccountSource, Value)>,
    retry_responses: std::collections::VecDeque<Response>,
    sent_credentials: Vec<Value>,
}

impl AppState {
    fn new() -> Result<Self, String> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("reqwest client build failed: {e}"))?;
        Ok(Self {
            http_client,
            route_memory: shared_route_memory(),
            route_memory_path: Some(route_memory_path()),
            #[cfg(test)]
            fixture_responses: None,
            #[cfg(test)]
            fixture_authentication: None,
        })
    }

    #[cfg(test)]
    fn for_tests() -> Result<Self, String> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .map_err(|e| format!("reqwest client build failed: {e}"))?;
        Ok(Self {
            http_client,
            route_memory: Arc::new(Mutex::new(RouteMemory::default())),
            route_memory_path: None,
            fixture_responses: None,
            fixture_authentication: None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteMemory {
    #[serde(default = "route_memory_version")]
    version: u32,
    #[serde(default)]
    candidates: HashMap<String, CandidateHealth>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    accounts: HashMap<String, CandidateHealth>,
    #[serde(default)]
    revision: u64,
    #[serde(skip)]
    credential_versions: HashMap<String, (String, u64)>,
    #[serde(skip)]
    account_epochs: HashMap<String, u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    response_bindings: HashMap<String, ResponseBinding>,
    #[serde(skip)]
    unsupported_responses: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseBinding {
    candidate_id: String,
    fingerprint: String,
    expires_at_ms: u64,
    #[serde(default)]
    ambiguous: bool,
}

impl Default for RouteMemory {
    fn default() -> Self {
        Self {
            version: route_memory_version(),
            candidates: HashMap::new(),
            accounts: HashMap::new(),
            revision: 0,
            credential_versions: HashMap::new(),
            account_epochs: HashMap::new(),
            response_bindings: HashMap::new(),
            unsupported_responses: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CandidateHealth {
    #[serde(default)]
    consecutive_failures: u32,
    #[serde(default)]
    cooldown_until_ms: Option<u64>,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    failure: Option<FailureClass>,
    #[serde(default)]
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_key: Option<String>,
}

#[derive(Debug)]
struct AttemptHealth {
    revision: u64,
    account_epoch: u64,
    credential_version: Mutex<Option<String>>,
}

#[derive(Debug, Clone)]
struct Candidate {
    internal_id: String,
    model_id: String,
    base_url: String,
    api_key: String,
    account_source: Option<crate::models::model::AccountSource>,
    fingerprint: String,
    attempt: Option<Arc<AttemptHealth>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailureClass {
    Authentication,
    Billing,
    ModelUnavailable,
    RateLimit,
    Transient,
    RequestIncompatible,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptRecord {
    candidate_id: String,
    model_id: String,
    failure: FailureClass,
    status: Option<u16>,
}

#[derive(Clone, Copy)]
struct RoutePolicy {
    max_attempts: usize,
    time_budget: Duration,
    header_timeout: Duration,
    first_byte_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum RouteProtocol {
    #[default]
    Chat,
    Responses,
}

impl Default for RoutePolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_ROUTE_CANDIDATES,
            time_budget: FALLBACK_TIME_BUDGET,
            header_timeout: UPSTREAM_HEADER_TIMEOUT,
            first_byte_timeout: UPSTREAM_FIRST_BYTE_TIMEOUT,
        }
    }
}

fn route_memory_version() -> u32 {
    1
}

fn route_memory_path() -> PathBuf {
    echobird_dir()
        .join("config")
        .join("smart-router-state.json")
}

fn shared_route_memory() -> Arc<Mutex<RouteMemory>> {
    ROUTE_MEMORY
        .get_or_init(|| Arc::new(Mutex::new(load_route_memory(&route_memory_path()))))
        .clone()
}

fn load_route_memory(path: &Path) -> RouteMemory {
    if !path.exists() {
        return RouteMemory::default();
    }
    fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|content| serde_json::from_str(&content).map_err(|e| e.to_string()))
        .unwrap_or_else(|error| {
            log::warn!(
                "[SmartRouter] Ignoring invalid route memory {}: {error}",
                path.display()
            );
            RouteMemory::default()
        })
}

fn save_route_memory(path: &Path, memory: &RouteMemory) {
    let Some(parent) = path.parent() else {
        return;
    };
    let result = fs::create_dir_all(parent)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::to_string_pretty(memory).map_err(|e| e.to_string()))
        .and_then(|content| fs::write(path, content).map_err(|e| e.to_string()));
    if let Err(error) = result {
        log::warn!(
            "[SmartRouter] Failed to save route memory {}: {error}",
            path.display()
        );
    }
}

fn persist_route_memory(state: &AppState, memory: &RouteMemory) {
    if let Some(path) = &state.route_memory_path {
        save_route_memory(path, memory);
    }
}

pub(crate) fn retain_candidate_memory(candidate_ids: &[String]) {
    let memory = shared_route_memory();
    let Ok(mut memory) = memory.lock() else {
        return;
    };
    let valid: HashSet<&str> = candidate_ids.iter().map(String::as_str).collect();
    let previous_len = memory.candidates.len();
    memory
        .candidates
        .retain(|candidate_id, _| valid.contains(candidate_id.as_str()));
    if memory.candidates.len() != previous_len {
        save_route_memory(&route_memory_path(), &memory);
    }
}

pub(crate) fn forget_candidate_memory(candidate_id: &str) {
    let memory = shared_route_memory();
    let Ok(mut memory) = memory.lock() else {
        return;
    };
    if clear_candidate_health(&mut memory, candidate_id) {
        save_route_memory(&route_memory_path(), &memory);
    }
}

fn clear_candidate_health(memory: &mut RouteMemory, candidate_id: &str) -> bool {
    memory.candidates.remove(candidate_id).is_some()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn candidate_fingerprint(model_id: &str, base_url: &str, api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model_id.as_bytes());
    hasher.update([0]);
    hasher.update(base_url.as_bytes());
    hasher.update([0]);
    hasher.update(api_key.as_bytes());
    hex::encode(hasher.finalize())
}

pub(super) fn start(
    listener: std::net::TcpListener,
) -> Result<super::runtime::ServerControl, String> {
    let state = AppState::new()?;
    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/v1/models", get(handle_models))
        .route("/models", get(handle_models))
        .route("/v1/chat/completions", post(handle_chat))
        .route("/chat/completions", post(handle_chat))
        .route("/v1/responses", post(handle_responses))
        .route("/responses", post(handle_responses))
        .route("/v1/messages", post(handle_messages))
        .route("/messages", post(handle_messages))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state);
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();

    log::info!("[SmartRouter] listening on 127.0.0.1:{port}");
    Ok(super::runtime::ServerControl::start(listener, app))
}

async fn handle_health() -> Json<Value> {
    let ids = candidate_ids();
    Json(json!({
        "status": "ok",
        "model": SMART_ROUTER_MODEL_ID,
        "candidates": ids.len(),
        "usableCandidates": super::usable_candidate_count(&ids),
    }))
}

async fn handle_models(headers: HeaderMap) -> Response {
    let expected_key = match api_key_for_use() {
        Ok(key) => key,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if !is_authorized(&headers, &expected_key) {
        return openai_error(StatusCode::UNAUTHORIZED, "Invalid EchoBird router API key");
    }

    Json(json!({
        "object": "list",
        "data": [{
            "id": SMART_ROUTER_MODEL_ID,
            "object": "model",
            "created": 0,
            "owned_by": "echobird"
        }]
    }))
    .into_response()
}

async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_config = match request_config_for_use() {
        Ok(config) => config,
        Err(e) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if !is_authorized(&headers, &request_config.api_key) {
        return openai_error(StatusCode::UNAUTHORIZED, "Invalid EchoBird router API key");
    }

    let requested_model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !is_supported_router_model(requested_model) {
        return openai_error(
            StatusCode::NOT_FOUND,
            &format!("Unknown router model: {requested_model}"),
        );
    }

    let candidates = resolve_candidates(request_config.candidate_ids);
    route_chat_with_candidates(&state, &headers, body, candidates).await
}

fn is_supported_router_model(model_id: &str) -> bool {
    matches!(
        model_id,
        SMART_ROUTER_MODEL_ID | "echobird/auto" | "echobird-auto" | SMART_ROUTER_INTERNAL_ID
    )
}

async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_config = match request_config_for_use() {
        Ok(config) => config,
        Err(error) => return openai_error(StatusCode::INTERNAL_SERVER_ERROR, &error),
    };
    if !is_authorized(&headers, &request_config.api_key) {
        return openai_error(StatusCode::UNAUTHORIZED, "Invalid EchoBird router API key");
    }
    if !is_supported_router_model(body["model"].as_str().unwrap_or_default()) {
        return openai_error(StatusCode::NOT_FOUND, "Unknown router model");
    }
    route_responses_with_candidates(
        &state,
        &headers,
        body,
        resolve_candidates(request_config.candidate_ids),
    )
    .await
}

async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_config = match request_config_for_use() {
        Ok(config) => config,
        Err(e) => return anthropic_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
    };
    if !is_authorized(&headers, &request_config.api_key) {
        return anthropic_error(StatusCode::UNAUTHORIZED, "Invalid EchoBird router API key");
    }

    route_messages_with_candidates(
        &state,
        &headers,
        body,
        resolve_candidates(request_config.candidate_ids),
    )
    .await
}

async fn route_messages_with_candidates(
    state: &AppState,
    headers: &HeaderMap,
    body: Value,
    candidates: Vec<Candidate>,
) -> Response {
    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut openai_body = crate::services::local_llm::proxy::anthropic_to_openai(&body);
    openai_body["model"] = Value::String(SMART_ROUTER_MODEL_ID.to_string());
    let response = route_chat_with_candidates(state, headers, openai_body, candidates).await;

    if !response.status().is_success() {
        return openai_error_to_anthropic(response).await;
    }
    if stream_requested {
        openai_stream_to_anthropic(response)
    } else {
        openai_response_to_anthropic(response).await
    }
}

fn resolve_candidates(ids: Vec<String>) -> Vec<Candidate> {
    resolve_candidates_from(
        ids,
        &model_manager::get_user_models(),
        &crate::services::local_llm::get_server_info_sync(),
    )
}

fn resolve_candidates_from(
    ids: Vec<String>,
    user_models: &[crate::models::model::ModelConfig],
    local_server: &crate::services::local_llm::LocalServerInfo,
) -> Vec<Candidate> {
    ids.into_iter()
        .filter_map(|id| {
            if id == "local-server" && local_server.running {
                let model_id = local_server.model_name.clone();
                let base_url = format!("http://127.0.0.1:{}/v1", local_server.port);
                let api_key = local_server.api_key.clone();
                return Some(Candidate {
                    attempt: None,
                    internal_id: id,
                    fingerprint: candidate_fingerprint(&model_id, &base_url, &api_key),
                    model_id,
                    base_url,
                    api_key,
                    account_source: None,
                });
            }

            let model = user_models.iter().find(|model| model.internal_id == id)?;
            let model_id = model.model_id.as_deref()?.trim();
            if model_id.is_empty() || model.base_url.trim().is_empty() {
                return None;
            }
            let api_key = model_manager::decrypt_key_for_use(&model.api_key);
            if api_key.trim().is_empty()
                && model.account_source.is_none()
                && !free_model_directory::is_keyless_model(&model.base_url, model_id)
            {
                return None;
            }

            Some(Candidate {
                attempt: None,
                internal_id: model.internal_id.clone(),
                fingerprint: candidate_fingerprint(
                    model_id,
                    &model.base_url,
                    &model
                        .account_source
                        .as_ref()
                        .map(|source| serde_json::to_string(source).unwrap_or_default())
                        .unwrap_or_else(|| model.api_key.clone()),
                ),
                model_id: model_id.to_string(),
                base_url: model.base_url.clone(),
                api_key,
                account_source: model.account_source.clone(),
            })
        })
        .filter(|candidate| !super::is_router_url(&candidate.base_url))
        .collect()
}

async fn route_chat_with_candidates(
    state: &AppState,
    inbound_headers: &HeaderMap,
    body: Value,
    candidates: Vec<Candidate>,
) -> Response {
    route_chat_with_policy(
        state,
        inbound_headers,
        body,
        candidates,
        RoutePolicy::default(),
    )
    .await
}

async fn route_responses_with_candidates(
    state: &AppState,
    inbound_headers: &HeaderMap,
    body: Value,
    candidates: Vec<Candidate>,
) -> Response {
    route_with_policy(
        state,
        inbound_headers,
        body,
        candidates,
        RoutePolicy::default(),
        RouteProtocol::Responses,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn route_live_account_probe(
    source: crate::models::model::AccountSource,
    model_id: String,
    body: Value,
) -> Result<Response, String> {
    let state = AppState::for_tests()?;
    let mut policy = RoutePolicy::default();
    if let Some(seconds) = std::env::var("ECHOBIRD_LIVE_PROXY_BUDGET_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (45..=180).contains(seconds))
    {
        policy.time_budget = Duration::from_secs(seconds);
    }
    let source_json = serde_json::to_string(&source).map_err(|_| "Invalid account source")?;
    let candidate = Candidate {
        attempt: None,
        internal_id: "live-account-probe".into(),
        fingerprint: candidate_fingerprint(&model_id, "account://echobird", &source_json),
        model_id,
        base_url: "account://echobird".into(),
        api_key: String::new(),
        account_source: Some(source),
    };
    // Use production routing without changing the user's candidates or health file.
    Ok(route_chat_with_policy(&state, &HeaderMap::new(), body, vec![candidate], policy).await)
}

async fn send_candidate(
    state: &AppState,
    headers: &HeaderMap,
    candidate: &Candidate,
    mut body: Value,
    protocol: RouteProtocol,
    refreshed_credentials: Option<Value>,
) -> Result<Response, String> {
    #[cfg(test)]
    if let Some(responses) = &state.fixture_responses {
        if let Some(credentials) = refreshed_credentials {
            let mut fixture = state
                .fixture_authentication
                .as_ref()
                .expect("Refresh fixture is missing")
                .lock()
                .unwrap();
            fixture.sent_credentials.push(credentials.clone());
            bind_attempt_credentials(state, candidate, &credentials);
            return fixture
                .retry_responses
                .pop_front()
                .ok_or_else(|| "Fixture retry response is missing".into());
        }
        let response = responses
            .lock()
            .map_err(|_| "Fixture response lock poisoned".to_string())?
            .remove(&candidate.internal_id)
            .ok_or_else(|| "Fixture response is missing".to_string())?;
        if let Some(credentials) = response.extensions().get::<RejectedAccountCredentials>() {
            bind_attempt_credentials(state, candidate, &credentials.0);
        }
        return Ok(response);
    }
    if let Some(source) = &candidate.account_source {
        let credentials = match refreshed_credentials {
            Some(credentials) => credentials,
            None => {
                match crate::services::account_model_sources::credentials(source, false).await {
                    Ok(value) => value,
                    Err(error) => {
                        return Ok(
                            crate::services::account_model_sources::credential_error(error)
                                .into_response(),
                        )
                    }
                }
            }
        };
        bind_attempt_credentials(state, candidate, &credentials);
        let rejected_credentials = credentials.clone();
        let result = send_account_request(
            state,
            source,
            credentials,
            &candidate.model_id,
            body.clone(),
            protocol,
        )
        .await;
        let mut response = match result {
            Ok(response) => response,
            Err(error) => error.into_response(),
        };
        response
            .extensions_mut()
            .insert(RejectedAccountCredentials(rejected_credentials));
        return Ok(response);
    }
    if zen_free::is_candidate(candidate) {
        return zen_free::send(state, headers, candidate, &body, protocol).await;
    }
    if protocol == RouteProtocol::Responses {
        let continuing = has_previous_response(&body);
        let unsupported = state.route_memory.lock().ok().is_some_and(|memory| {
            memory
                .unsupported_responses
                .get(&candidate.fingerprint)
                .is_some_and(|until| *until > now_ms())
        });
        if !unsupported || continuing {
            let response = send_api_request(state, headers, candidate, &body, protocol).await?;
            if matches!(response.status().as_u16(), 400 | 404 | 405 | 501) {
                let status = response.status();
                let (parts, response_body) = response.into_parts();
                let bytes = axum::body::to_bytes(response_body, 64 * 1024)
                    .await
                    .map_err(|error| error.to_string())?;
                if !responses_endpoint_unsupported(status, &bytes) || continuing {
                    return Ok(Response::from_parts(parts, Body::from(bytes)));
                }
                remember_unsupported_responses(state, candidate);
            } else {
                return Ok(response);
            }
        }
        body = match responses_chat_request(&body) {
            Ok(chat) => chat,
            Err(error) => return Ok(error.into_response()),
        };
        let response =
            send_api_request(state, headers, candidate, &body, RouteProtocol::Chat).await?;
        return responses_from_chat(response, &body)
            .await
            .map_err(|error| error.to_string());
    }
    crate::services::account_proxy::strip_response_metadata(&mut body);
    send_api_request(state, headers, candidate, &body, protocol).await
}

#[derive(Clone)]
struct RejectedAccountCredentials(Value);

async fn refresh_rejected_account(
    _state: &AppState,
    candidate: &Candidate,
    rejected: &RejectedAccountCredentials,
    started: Instant,
    policy: RoutePolicy,
) -> Result<Value, crate::services::account_proxy::ProxyError> {
    if let Some(source) = &candidate.account_source {
        crate::services::account_proxy::forget_preparation_metadata(
            source.provider,
            &source.account_id,
        );
    }
    let refresh = async {
        #[cfg(test)]
        if let Some(fixture) = &_state.fixture_authentication {
            let (delay, result) = {
                let mut fixture = fixture.lock().unwrap();
                fixture.rejections.push((
                    candidate.account_source.clone().unwrap(),
                    rejected.0.clone(),
                ));
                (fixture.delay, fixture.result.clone())
            };
            tokio::time::sleep(delay).await;
            return result;
        }
        crate::services::account_model_sources::credentials_after_rejection(
            candidate
                .account_source
                .as_ref()
                .expect("Only account requests bind credentials"),
            &rejected.0,
        )
        .await
    };
    let timeout = remaining_timeout(started, policy.time_budget, policy.header_timeout)
        .ok_or_else(|| {
            crate::services::account_proxy::ProxyError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "Account refresh exceeded the routing budget",
            )
        })?;
    tokio::time::timeout(timeout, refresh)
        .await
        .map_err(|_| {
            crate::services::account_proxy::ProxyError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "Account refresh exceeded the routing budget",
            )
        })?
        .map_err(crate::services::account_model_sources::credential_error)
}

#[derive(Clone, Copy)]
struct NativeResponses;

async fn send_account_request(
    state: &AppState,
    source: &crate::models::model::AccountSource,
    credentials: Value,
    model: &str,
    body: Value,
    protocol: RouteProtocol,
) -> Result<Response, crate::services::account_proxy::ProxyError> {
    if protocol == RouteProtocol::Responses {
        if let Some(mut response) = crate::services::account_proxy::route_responses(
            source.provider,
            credentials.clone(),
            model,
            body.clone(),
            &state.http_client,
        )
        .await?
        {
            response.extensions_mut().insert(NativeResponses);
            return Ok(response);
        }
        let chat = responses_chat_request(&body)?;
        let response = crate::services::account_proxy::route(
            source.provider,
            credentials,
            model,
            chat.clone(),
            &state.http_client,
        )
        .await?;
        return responses_from_chat(response, &chat).await;
    }
    crate::services::account_proxy::route(
        source.provider,
        credentials,
        model,
        body,
        &state.http_client,
    )
    .await
}

fn responses_chat_request(
    body: &Value,
) -> Result<Value, crate::services::account_proxy::ProxyError> {
    let mut chat =
        crate::services::account_proxy::responses_to_chat(body).map_err(|mut error| {
            error.error_code = Some("responses_conversion_unsupported".into());
            error
        })?;
    // Collect after headers, within the router's existing completion budget.
    chat["stream"] = json!(true);
    Ok(chat)
}

async fn responses_from_chat(
    response: Response,
    chat: &Value,
) -> Result<Response, crate::services::account_proxy::ProxyError> {
    let original = &chat["_echobird_responses"];
    let response =
        crate::services::account_proxy::chat_to_responses_with_tools(response, true, original)
            .await;
    crate::services::account_proxy::responses_response(
        response,
        original["stream"].as_bool().unwrap_or(false),
    )
}

async fn send_api_request(
    state: &AppState,
    headers: &HeaderMap,
    candidate: &Candidate,
    body: &Value,
    protocol: RouteProtocol,
) -> Result<Response, String> {
    let mut body = body.clone();
    if protocol == RouteProtocol::Chat {
        crate::services::account_proxy::strip_response_metadata(&mut body);
    } else {
        crate::services::account_proxy::normalize_responses_tool_rounds(&mut body);
    }
    let mut request = state
        .http_client
        .post(if protocol == RouteProtocol::Responses {
            responses_url(&candidate.base_url)
        } else {
            chat_completions_url(&candidate.base_url)
        })
        .header(header::CONTENT_TYPE, "application/json")
        .header("HTTP-Referer", "https://echobird.ai")
        .header("X-OpenRouter-Title", "EchoBird")
        .header("X-OpenRouter-Categories", "programming-app,personal-agent")
        .json(&body);
    if let Some(api_key) = upstream_bearer_token(candidate) {
        request = request.bearer_auth(api_key);
    }
    if let Some(user_agent) = headers.get(header::USER_AGENT) {
        request = request.header(header::USER_AGENT, user_agent.clone());
    }
    let upstream = request.send().await.map_err(|e| e.to_string())?;
    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    if protocol == RouteProtocol::Responses {
        response.extensions_mut().insert(NativeResponses);
        crate::services::account_proxy::responses_response(
            response,
            body["stream"].as_bool().unwrap_or(false),
        )
        .map_err(|error| error.to_string())
    } else {
        Ok(response)
    }
}

fn upstream_bearer_token(candidate: &Candidate) -> Option<&str> {
    (!candidate.api_key.trim().is_empty()).then_some(candidate.api_key.as_str())
}

async fn route_chat_with_policy(
    state: &AppState,
    inbound_headers: &HeaderMap,
    body: Value,
    candidates: Vec<Candidate>,
    policy: RoutePolicy,
) -> Response {
    route_with_policy(
        state,
        inbound_headers,
        body,
        candidates,
        policy,
        RouteProtocol::Chat,
    )
    .await
}

async fn route_with_policy(
    state: &AppState,
    inbound_headers: &HeaderMap,
    body: Value,
    candidates: Vec<Candidate>,
    policy: RoutePolicy,
    protocol: RouteProtocol,
) -> Response {
    if protocol == RouteProtocol::Responses && body["background"] == true {
        return openai_error(StatusCode::BAD_REQUEST, "Background Responses require a separate task lifecycle and are not supported by Auto Router");
    }
    if protocol == RouteProtocol::Responses
        && body
            .get("conversation")
            .is_some_and(|value| !value.is_null())
    {
        return openai_error(StatusCode::BAD_REQUEST, "Conversation objects are not supported by Auto Router; use complete input or a bound previous_response_id");
    }
    let bound_continuation = protocol == RouteProtocol::Responses && has_previous_response(&body);
    let bound_candidates = match response_candidates(state, &body, candidates.clone(), protocol) {
        Ok(candidates) => candidates,
        Err(error) => return openai_error(StatusCode::BAD_REQUEST, error),
    };
    if candidates.is_empty() {
        return openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No usable models are configured for Auto Router",
        );
    }

    let mut available = prioritized_candidates(state, candidates);
    if bound_continuation {
        available.retain(|candidate| {
            bound_candidates
                .iter()
                .any(|bound| bound.internal_id == candidate.internal_id)
        });
    }
    if available.is_empty() {
        return openai_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "All Auto Router models are cooling down",
        );
    }

    let stream_requested = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut attempts = Vec::new();
    let mut incompatible_response = None;
    let routing_started = Instant::now();

    let mut available = available.into_iter();
    let mut authentication_retry: Option<(Candidate, Option<Value>)> = None;
    while let Some((mut candidate, refreshed_credentials)) = authentication_retry
        .take()
        .or_else(|| available.next().map(|candidate| (candidate, None)))
    {
        if candidate_is_cooling_down(state, &candidate) {
            continue;
        }
        if attempts.len() >= policy.max_attempts {
            break;
        }
        let Some(header_timeout) = remaining_timeout(
            routing_started,
            policy.time_budget,
            if !stream_requested
                && candidate.account_source.as_ref().is_some_and(|source| {
                    source.provider == crate::models::model::AccountProvider::ClaudeCode
                })
            {
                policy.time_budget
            } else {
                policy.header_timeout
            },
        ) else {
            break;
        };
        let mut upstream_body = body.clone();
        let authentication_retried = refreshed_credentials.is_some();
        if !authentication_retried {
            begin_attempt(state, &mut candidate);
        }
        upstream_body["model"] = Value::String(candidate.model_id.clone());
        let activity_guard = RouteActivityGuard::begin(&candidate.internal_id);
        let sent = tokio::time::timeout(
            header_timeout,
            send_candidate(
                state,
                inbound_headers,
                &candidate,
                upstream_body,
                protocol,
                refreshed_credentials,
            ),
        )
        .await;
        let mut response = match sent {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                mark_failure(state, &candidate, FailureClass::Transient, None);
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure: FailureClass::Transient,
                    status: None,
                });
                log::warn!(
                    "[SmartRouter] {} transport failure: {}",
                    candidate.internal_id,
                    error
                );
                continue;
            }
            Err(_) => {
                mark_failure(state, &candidate, FailureClass::Transient, None);
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure: FailureClass::Transient,
                    status: None,
                });
                log::warn!(
                    "[SmartRouter] {} timed out before response headers",
                    candidate.internal_id
                );
                continue;
            }
        };

        let rejected_credentials = (!authentication_retried)
            .then(|| {
                response
                    .extensions()
                    .get::<RejectedAccountCredentials>()
                    .cloned()
            })
            .flatten();
        if response.status() == StatusCode::UNAUTHORIZED {
            if let Some(rejected) = &rejected_credentials {
                match refresh_rejected_account(state, &candidate, rejected, routing_started, policy)
                    .await
                {
                    Ok(credentials) => {
                        authentication_retry = Some((candidate, Some(credentials)));
                        continue;
                    }
                    Err(error) => response = error.into_response(),
                }
            }
        }
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        let cache_control = response.headers().get(header::CACHE_CONTROL).cloned();
        let response_headers = response.headers().clone();
        let native_responses = response.extensions().get::<NativeResponses>().is_some();
        let model_scope = response
            .extensions()
            .get::<crate::services::account_proxy::ProxyFailureMetadata>()
            .is_some_and(|metadata| metadata.model_scope);
        if !status.is_success() {
            let retry_after = retry_after(response.headers());
            let Some(error_body_timeout) =
                remaining_timeout(routing_started, policy.time_budget, policy.header_timeout)
            else {
                mark_failure(state, &candidate, FailureClass::Transient, None);
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure: FailureClass::Transient,
                    status: Some(status.as_u16()),
                });
                break;
            };
            let response_body = match tokio::time::timeout(
                error_body_timeout,
                response.into_body().collect(),
            )
            .await
            {
                Ok(Ok(body)) => body.to_bytes(),
                Ok(Err(_)) | Err(_) => {
                    mark_failure(state, &candidate, FailureClass::Transient, None);
                    attempts.push(AttemptRecord {
                        candidate_id: candidate.internal_id.clone(),
                        model_id: candidate.model_id.clone(),
                        failure: FailureClass::Transient,
                        status: Some(status.as_u16()),
                    });
                    break;
                }
            };
            if bound_continuation && is_continuation_failure(status, &response_body) {
                let mut response = upstream_response(
                    status,
                    content_type,
                    cache_control,
                    response_body,
                    &candidate,
                );
                copy_response_headers(&mut response, &response_headers);
                return response;
            }
            if is_context_limit_body(&response_body) {
                let mut response = upstream_response(
                    StatusCode::BAD_REQUEST,
                    content_type,
                    cache_control,
                    response_body,
                    &candidate,
                );
                copy_response_headers(&mut response, &response_headers);
                return response;
            }
            if let Some(failure) = classify_failure(status, &response_body) {
                if failure == FailureClass::RequestIncompatible {
                    incompatible_response = Some(upstream_response(
                        status,
                        content_type.clone(),
                        cache_control.clone(),
                        response_body.clone(),
                        &candidate,
                    ));
                }
                mark_failure_scoped(state, &candidate, failure, retry_after, model_scope);
                if protocol == RouteProtocol::Responses && has_previous_response(&body) {
                    let mut response = upstream_response(
                        status,
                        content_type,
                        cache_control,
                        response_body,
                        &candidate,
                    );
                    copy_response_headers(&mut response, &response_headers);
                    return response;
                }
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure,
                    status: Some(status.as_u16()),
                });
                log::warn!(
                    "[SmartRouter] {} returned {}, trying next candidate",
                    candidate.internal_id,
                    status.as_u16()
                );
                continue;
            }

            let mut response = upstream_response(
                status,
                content_type,
                cache_control,
                response_body,
                &candidate,
            );
            if protocol == RouteProtocol::Responses {
                copy_response_headers(&mut response, &response_headers);
            }
            return response;
        }

        if stream_requested {
            let mut completion = StreamCompletion {
                headers: response.headers().clone(),
                protocol,
                ..StreamCompletion::default()
            };
            let mut upstream_stream = response.into_body().into_data_stream();
            let Some(first_byte_timeout) = remaining_timeout(
                routing_started,
                policy.time_budget,
                policy.first_byte_timeout,
            ) else {
                mark_failure(state, &candidate, FailureClass::Transient, None);
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure: FailureClass::Transient,
                    status: Some(status.as_u16()),
                });
                break;
            };
            let first = match tokio::time::timeout(first_byte_timeout, async {
                while let Some(chunk) = upstream_stream.next().await {
                    let chunk = chunk?;
                    if !chunk.is_empty() {
                        return Ok(chunk);
                    }
                }
                Err(axum::Error::new(empty_response_error()))
            })
            .await
            {
                Ok(Ok(chunk)) => {
                    preflight_stream(
                        chunk,
                        &mut upstream_stream,
                        &mut completion,
                        bound_continuation && native_responses,
                    )
                    .await
                }
                Ok(Err(error)) => Err(error),
                Err(_) => {
                    mark_failure(state, &candidate, FailureClass::Transient, None);
                    attempts.push(AttemptRecord {
                        candidate_id: candidate.internal_id.clone(),
                        model_id: candidate.model_id.clone(),
                        failure: FailureClass::Transient,
                        status: Some(status.as_u16()),
                    });
                    continue;
                }
            };
            match first {
                Ok(buffered) => {
                    let stream_state = state.clone();
                    let stream_candidate = candidate.clone();
                    let body_stream = stream::unfold(
                        (buffered, upstream_stream, completion, false, activity_guard),
                        move |(mut buffered, mut upstream, mut completion, finished, guard)| {
                            let stream_state = stream_state.clone();
                            let stream_candidate = stream_candidate.clone();
                            async move {
                                if finished {
                                    return None;
                                }
                                let (item, observed) = if let Some(bytes) = buffered.pop_front() {
                                    (Some(Ok(bytes)), true)
                                } else {
                                    (upstream.next().await, false)
                                };
                                let (result, finished) = match item {
                                    Some(Ok(bytes)) => match if observed {
                                        Ok(())
                                    } else {
                                        completion.push(&bytes)
                                    } {
                                        Ok(()) => {
                                            if completion.done && buffered.is_empty() {
                                                if completion.failure.is_none() {
                                                    mark_success(&stream_state, &stream_candidate);
                                                    if native_responses {
                                                        remember_response(
                                                            &stream_state,
                                                            &stream_candidate,
                                                            completion.response_id.as_deref(),
                                                        );
                                                    }
                                                } else if let Some(error) = &completion.failure {
                                                    let _ = mark_response_failure(
                                                        &stream_state,
                                                        &stream_candidate,
                                                        error,
                                                        bound_continuation,
                                                    );
                                                }
                                            }
                                            (Ok(bytes), completion.done && buffered.is_empty())
                                        }
                                        Err(error) => {
                                            let _ = mark_response_failure(
                                                &stream_state,
                                                &stream_candidate,
                                                &error,
                                                bound_continuation,
                                            );
                                            (Err(error), true)
                                        }
                                    },
                                    Some(Err(error)) => {
                                        let _ = mark_response_failure(
                                            &stream_state,
                                            &stream_candidate,
                                            &error,
                                            bound_continuation,
                                        );
                                        (Err(std::io::Error::other(error)), true)
                                    }
                                    None => match completion.finish() {
                                        Ok(()) => {
                                            if let Some(error) = &completion.failure {
                                                let _ = mark_response_failure(
                                                    &stream_state,
                                                    &stream_candidate,
                                                    error,
                                                    bound_continuation,
                                                );
                                            } else {
                                                mark_success(&stream_state, &stream_candidate);
                                                if native_responses {
                                                    remember_response(
                                                        &stream_state,
                                                        &stream_candidate,
                                                        completion.response_id.as_deref(),
                                                    );
                                                }
                                            }
                                            return None;
                                        }
                                        Err(error) => {
                                            let _ = mark_response_failure(
                                                &stream_state,
                                                &stream_candidate,
                                                &error,
                                                bound_continuation,
                                            );
                                            (Err(error), true)
                                        }
                                    },
                                };
                                Some((result, (buffered, upstream, completion, finished, guard)))
                            }
                        },
                    );
                    let mut result = streaming_response(
                        content_type,
                        cache_control,
                        Body::from_stream(body_stream),
                        &candidate,
                    );
                    if protocol == RouteProtocol::Responses {
                        copy_response_headers(&mut result, &response_headers);
                    }
                    return result;
                }
                Err(mut error) => {
                    if proxy_error_from_chain(&error)
                        .is_some_and(|error| error.status == StatusCode::UNAUTHORIZED)
                    {
                        if let Some(rejected) = &rejected_credentials {
                            match refresh_rejected_account(
                                state,
                                &candidate,
                                rejected,
                                routing_started,
                                policy,
                            )
                            .await
                            {
                                Ok(credentials) => {
                                    authentication_retry = Some((candidate, Some(credentials)));
                                    continue;
                                }
                                Err(refresh_error) => error = axum::Error::new(refresh_error),
                            }
                        }
                    }
                    let Some(failure) =
                        mark_response_failure(state, &candidate, &error, bound_continuation)
                    else {
                        return proxy_error_from_chain(&error)
                            .expect("Unclassified body failure has a typed upstream error")
                            .clone()
                            .into_response();
                    };
                    if bound_continuation {
                        return proxy_error_from_chain(&error)
                            .map(|error| error.clone().into_response())
                            .unwrap_or_else(|| {
                                openai_error(
                                    StatusCode::BAD_GATEWAY,
                                    "The bound Responses stream failed",
                                )
                            });
                    }
                    attempts.push(AttemptRecord {
                        candidate_id: candidate.internal_id.clone(),
                        model_id: candidate.model_id.clone(),
                        failure,
                        status: Some(status.as_u16()),
                    });
                    continue;
                }
            }
        }

        let Some(body_timeout) =
            remaining_timeout(routing_started, policy.time_budget, policy.time_budget)
        else {
            mark_failure(state, &candidate, FailureClass::Transient, None);
            attempts.push(AttemptRecord {
                candidate_id: candidate.internal_id.clone(),
                model_id: candidate.model_id.clone(),
                failure: FailureClass::Transient,
                status: Some(status.as_u16()),
            });
            break;
        };
        match tokio::time::timeout(body_timeout, response.into_body().collect()).await {
            Ok(Ok(response_body)) => {
                let bytes = response_body.to_bytes();
                if protocol == RouteProtocol::Responses {
                    let value = match serde_json::from_slice::<Value>(&bytes) {
                        Ok(value) => value,
                        Err(error) => {
                            mark_stream_failure(state, &candidate, &error);
                            attempts.push(AttemptRecord {
                                candidate_id: candidate.internal_id.clone(),
                                model_id: candidate.model_id.clone(),
                                failure: FailureClass::Transient,
                                status: Some(status.as_u16()),
                            });
                            continue;
                        }
                    };
                    if let Some(error) = crate::services::account_proxy::stream_error(
                        if value["status"] == "failed" {
                            "response.failed"
                        } else {
                            ""
                        },
                        &value.to_string(),
                        &response_headers,
                    ) {
                        mark_response_failure(state, &candidate, &error, bound_continuation);
                        if crate::services::account_proxy::context_limit_error(
                            error.error_code.as_deref(),
                            &error.message,
                        ) {
                            return error.into_response();
                        }
                    } else if matches!(value["status"].as_str(), Some("completed" | "incomplete"))
                        && value["object"] == "response"
                        && value["id"].as_str().is_some_and(|id| !id.is_empty())
                    {
                        if empty_completed_response(&value) {
                            let error = empty_response_error();
                            mark_response_failure(state, &candidate, &error, bound_continuation);
                            if bound_continuation {
                                return error.into_response();
                            }
                            attempts.push(AttemptRecord {
                                candidate_id: candidate.internal_id.clone(),
                                model_id: candidate.model_id.clone(),
                                failure: FailureClass::Transient,
                                status: Some(status.as_u16()),
                            });
                            continue;
                        }
                        mark_success(state, &candidate);
                        if native_responses {
                            remember_response(state, &candidate, value["id"].as_str());
                        }
                    } else {
                        mark_failure(state, &candidate, FailureClass::Transient, None);
                        attempts.push(AttemptRecord {
                            candidate_id: candidate.internal_id.clone(),
                            model_id: candidate.model_id.clone(),
                            failure: FailureClass::Transient,
                            status: Some(status.as_u16()),
                        });
                        continue;
                    }
                    let mut response =
                        upstream_response(status, content_type, cache_control, bytes, &candidate);
                    copy_response_headers(&mut response, &response_headers);
                    return response;
                }
                if serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .is_some_and(|value| empty_chat_response(&value))
                {
                    mark_failure(state, &candidate, FailureClass::Transient, None);
                    attempts.push(AttemptRecord {
                        candidate_id: candidate.internal_id.clone(),
                        model_id: candidate.model_id.clone(),
                        failure: FailureClass::Transient,
                        status: Some(status.as_u16()),
                    });
                    continue;
                }
                mark_success(state, &candidate);
                return upstream_response(status, content_type, cache_control, bytes, &candidate);
            }
            Ok(Err(mut error)) => {
                if proxy_error_from_chain(&error)
                    .is_some_and(|error| error.status == StatusCode::UNAUTHORIZED)
                {
                    if let Some(rejected) = &rejected_credentials {
                        match refresh_rejected_account(
                            state,
                            &candidate,
                            rejected,
                            routing_started,
                            policy,
                        )
                        .await
                        {
                            Ok(credentials) => {
                                authentication_retry = Some((candidate, Some(credentials)));
                                continue;
                            }
                            Err(refresh_error) => error = axum::Error::new(refresh_error),
                        }
                    }
                }
                let Some(failure) =
                    mark_response_failure(state, &candidate, &error, bound_continuation)
                else {
                    return proxy_error_from_chain(&error)
                        .expect("Unclassified body failure has a typed upstream error")
                        .clone()
                        .into_response();
                };
                if protocol == RouteProtocol::Responses && has_previous_response(&body) {
                    return proxy_error_from_chain(&error)
                        .map(|error| error.clone().into_response())
                        .unwrap_or_else(|| {
                            openai_error(
                                StatusCode::BAD_GATEWAY,
                                "The bound Responses stream failed",
                            )
                        });
                }
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure,
                    status: Some(status.as_u16()),
                });
                log::warn!(
                    "[SmartRouter] {} response body failure: {}",
                    candidate.internal_id,
                    error
                );
            }
            Err(_) => {
                mark_failure(state, &candidate, FailureClass::Transient, None);
                attempts.push(AttemptRecord {
                    candidate_id: candidate.internal_id.clone(),
                    model_id: candidate.model_id.clone(),
                    failure: FailureClass::Transient,
                    status: Some(status.as_u16()),
                });
                log::warn!(
                    "[SmartRouter] {} response body exceeded the routing budget",
                    candidate.internal_id
                );
                break;
            }
        }
    }

    let detail = serde_json::to_value(&attempts).unwrap_or_else(|_| json!([]));
    if !attempts.is_empty()
        && attempts
            .iter()
            .all(|attempt| attempt.failure == FailureClass::RequestIncompatible)
    {
        if let Some(response) = incompatible_response {
            return response;
        }
    }
    log::error!("[SmartRouter] all candidates failed: {detail}");
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "All Auto Router models failed",
        Some(detail),
    )
}

#[derive(Debug, Default)]
struct ToolCallBuffer {
    id: String,
    name: String,
    arguments: String,
}

struct AnthropicSseAdapter {
    parser: crate::services::account_proxy::SseParser,
    google_thought: crate::services::account_proxy::GoogleThought,
    message_id: String,
    text_index: Option<usize>,
    thinking_index: Option<usize>,
    next_block_index: usize,
    tool_calls: BTreeMap<u64, ToolCallBuffer>,
    finish_reason: Option<String>,
    output_tokens: u64,
    finished: bool,
}

impl AnthropicSseAdapter {
    fn new() -> Self {
        Self {
            parser: crate::services::account_proxy::SseParser::default(),
            google_thought: crate::services::account_proxy::GoogleThought::default(),
            message_id: format!("msg_{}", chrono::Utc::now().timestamp_millis()),
            text_index: None,
            thinking_index: None,
            next_block_index: 0,
            tool_calls: BTreeMap::new(),
            finish_reason: None,
            output_tokens: 0,
            finished: false,
        }
    }

    fn start_event(&self) -> Bytes {
        anthropic_sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": self.message_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": SMART_ROUTER_MODEL_ID,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }),
        )
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Bytes>, String> {
        let mut events = Vec::new();
        for (event, data) in self.parser.push(chunk) {
            events.extend(self.process_frame(&event, &data)?);
        }
        Ok(events)
    }

    fn finish(mut self) -> Result<Vec<Bytes>, String> {
        let mut events = Vec::new();
        for (event, data) in self.parser.finish() {
            events.extend(self.process_frame(&event, &data)?);
        }
        if !self.finished && self.finish_reason.is_none() {
            return Err("Smart Router stream ended before completion".to_string());
        }
        events.extend(self.finish_events());
        Ok(events)
    }

    fn process_frame(&mut self, event: &str, data: &str) -> Result<Vec<Bytes>, String> {
        if self.finished {
            return Ok(Vec::new());
        }
        if data.trim() == "[DONE]" {
            return Ok(self.finish_events());
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|error| format!("Invalid Smart Router stream: {error}"))?;
        if event == "error"
            || chunk.get("error").is_some_and(|error| !error.is_null())
            || chunk["type"] == "error"
        {
            return Err(chunk["error"]["message"]
                .as_str()
                .or_else(|| chunk["message"].as_str())
                .unwrap_or("Smart Router upstream stream failed")
                .to_string());
        }

        if let Some(tokens) = chunk
            .get("usage")
            .and_then(|usage| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
        {
            self.output_tokens = tokens;
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return Ok(Vec::new());
        };
        if let Some(reason) = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
        {
            self.finish_reason = Some(reason.to_string());
        }

        let Some(delta) = choice.get("delta") else {
            return Ok(Vec::new());
        };
        let mut events = Vec::new();
        if let Some(text) = delta["reasoning_content"].as_str() {
            self.push_content(true, text, &mut events);
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            let marked = delta["extra_content"]["google"]["thought"] == true;
            for (thinking, text) in self.google_thought.push(text, marked) {
                self.push_content(thinking, &text, &mut events);
            }
        }
        if let Some(text) = delta["refusal"].as_str() {
            self.push_content(false, text, &mut events);
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                let index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let buffered = self.tool_calls.entry(index).or_default();
                if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                    buffered.id.push_str(id);
                }
                if let Some(function) = tool_call.get("function") {
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        buffered.name.push_str(name);
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        buffered.arguments.push_str(arguments);
                    }
                }
            }
        }
        Ok(events)
    }

    fn push_content(&mut self, thinking: bool, text: &str, events: &mut Vec<Bytes>) {
        if text.is_empty() {
            return;
        }
        let previous = if thinking {
            self.text_index.take()
        } else {
            self.thinking_index.take()
        };
        if let Some(index) = previous {
            events.push(anthropic_sse_event(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ));
        }
        let current = if thinking {
            &mut self.thinking_index
        } else {
            &mut self.text_index
        };
        let index = match *current {
            Some(index) => index,
            None => {
                let index = self.next_block_index;
                self.next_block_index += 1;
                *current = Some(index);
                events.push(anthropic_sse_event(
                    "content_block_start",
                    json!({"type":"content_block_start","index":index,"content_block":if thinking { json!({"type":"thinking","thinking":""}) } else { json!({"type":"text","text":""}) }}),
                ));
                index
            }
        };
        events.push(anthropic_sse_event(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":if thinking { json!({"type":"thinking_delta","thinking":text}) } else { json!({"type":"text_delta","text":text}) }}),
        ));
    }

    fn finish_events(&mut self) -> Vec<Bytes> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();

        if let Some((thinking, text)) = self.google_thought.finish() {
            self.push_content(thinking, &text, &mut events);
        }

        for index in [self.thinking_index, self.text_index].into_iter().flatten() {
            events.push(anthropic_sse_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }

        for tool_call in self.tool_calls.values() {
            let index = self.next_block_index;
            self.next_block_index += 1;
            let id = if tool_call.id.is_empty() {
                format!("call_{index}")
            } else {
                tool_call.id.clone()
            };
            events.push(anthropic_sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": id,
                        "name": tool_call.name,
                        "input": {}
                    }
                }),
            ));
            events.push(anthropic_sse_event(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": if tool_call.arguments.is_empty() {
                            "{}"
                        } else {
                            tool_call.arguments.as_str()
                        }
                    }
                }),
            ));
            events.push(anthropic_sse_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }

        if self.next_block_index == 0 && self.tool_calls.is_empty() {
            events.push(anthropic_sse_event(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                }),
            ));
            events.push(anthropic_sse_event(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ));
        }

        let stop_reason = if !self.tool_calls.is_empty() {
            "tool_use"
        } else {
            match self.finish_reason.as_deref() {
                Some("length") => "max_tokens",
                Some("tool_calls") => "tool_use",
                _ => "end_turn",
            }
        };
        events.push(anthropic_sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": self.output_tokens}
            }),
        ));
        events.push(anthropic_sse_event(
            "message_stop",
            json!({"type": "message_stop"}),
        ));
        events
    }
}

fn anthropic_sse_event(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

fn openai_stream_to_anthropic(response: Response) -> Response {
    let routed_via = response.headers().get("X-EchoBird-Routed-Via").cloned();
    let mut source = response.into_body().into_data_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(32);

    tokio::spawn(async move {
        let mut adapter = AnthropicSseAdapter::new();
        if tx.send(Ok(adapter.start_event())).await.is_err() {
            return;
        }
        loop {
            let item = tokio::select! {
                _ = tx.closed() => return,
                item = source.next() => item,
            };
            let Some(item) = item else { break };
            match item {
                Ok(chunk) => {
                    let events = match adapter.push(&chunk) {
                        Ok(events) => events,
                        Err(error) => {
                            let _ = tx.send(Err(std::io::Error::other(error))).await;
                            return;
                        }
                    };
                    for event in events {
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(std::io::Error::other(error))).await;
                    return;
                }
            }
        }
        let events = match adapter.finish() {
            Ok(events) => events,
            Err(error) => {
                let _ = tx.send(Err(std::io::Error::other(error))).await;
                return;
            }
        };
        for event in events {
            if tx.send(Ok(event)).await.is_err() {
                return;
            }
        }
    });

    anthropic_response(
        StatusCode::OK,
        "text/event-stream",
        Body::from_stream(ReceiverStream::new(rx)),
        routed_via,
    )
}

async fn openai_response_to_anthropic(response: Response) -> Response {
    let routed_via = response.headers().get("X-EchoBird-Routed-Via").cloned();
    let body = match response.into_body().collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            return anthropic_error(
                StatusCode::BAD_GATEWAY,
                &format!("Failed to read Smart Router response: {error}"),
            )
        }
    };
    let data: Value = match serde_json::from_slice(&body) {
        Ok(data) => data,
        Err(error) => {
            return anthropic_error(
                StatusCode::BAD_GATEWAY,
                &format!("Invalid Smart Router response: {error}"),
            )
        }
    };
    let converted = crate::services::local_llm::proxy::openai_to_anthropic(&data);
    anthropic_response(
        StatusCode::OK,
        "application/json",
        Body::from(converted.to_string()),
        routed_via,
    )
}

async fn openai_error_to_anthropic(response: Response) -> Response {
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .map(|collected| collected.to_bytes())
        .unwrap_or_default();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let message = parsed
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("Auto Router request failed");
    anthropic_error(status, message)
}

fn anthropic_error(status: StatusCode, message: &str) -> Response {
    let error_type = match status {
        StatusCode::BAD_REQUEST => "invalid_request_error",
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => "authentication_error",
        StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "api_error",
    };
    anthropic_response(
        status,
        "application/json",
        Body::from(
            json!({
                "type": "error",
                "error": {"type": error_type, "message": message}
            })
            .to_string(),
        ),
        None,
    )
}

fn anthropic_response(
    status: StatusCode,
    content_type: &'static str,
    body: Body,
    routed_via: Option<HeaderValue>,
) -> Response {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "no-cache");
    if let Some(routed_via) = routed_via {
        response = response.header("X-EchoBird-Routed-Via", routed_via);
    }
    response.body(body).unwrap_or_else(|_| {
        openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to build Anthropic router response",
        )
    })
}

fn prioritized_candidates(state: &AppState, candidates: Vec<Candidate>) -> Vec<Candidate> {
    let Ok(mut memory) = state.route_memory.lock() else {
        return candidates;
    };
    let now = now_ms();
    let fingerprints: HashMap<&str, &str> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.internal_id.as_str(),
                candidate.fingerprint.as_str(),
            )
        })
        .collect();
    let previous_len = memory.candidates.len();
    let account_keys: HashSet<String> = candidates.iter().filter_map(account_health_key).collect();
    let previous_accounts = memory.accounts.len();
    memory.accounts.retain(|key, _| account_keys.contains(key));
    memory.candidates.retain(|candidate_id, health| {
        fingerprints
            .get(candidate_id.as_str())
            .is_some_and(|fingerprint| health.fingerprint == *fingerprint)
    });
    if memory.candidates.len() != previous_len || memory.accounts.len() != previous_accounts {
        persist_route_memory(state, &memory);
    }

    candidates
        .into_iter()
        .filter(|candidate| {
            !account_health_key(candidate)
                .and_then(|key| memory.accounts.get(&key))
                .and_then(|health| health.cooldown_until_ms)
                .is_some_and(|until| until > now)
                && !memory
                    .candidates
                    .get(&candidate.internal_id)
                    .and_then(|health| health.cooldown_until_ms)
                    .is_some_and(|until| until > now)
        })
        .collect()
}

fn account_health_key(candidate: &Candidate) -> Option<String> {
    candidate.account_source.as_ref().map(source_health_key)
}

fn source_health_key(source: &crate::models::model::AccountSource) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(source).unwrap_or_default(),
    ))
}

fn candidate_is_cooling_down(state: &AppState, candidate: &Candidate) -> bool {
    let Ok(memory) = state.route_memory.lock() else {
        return false;
    };
    let now = now_ms();
    memory
        .candidates
        .get(&candidate.internal_id)
        .and_then(|health| health.cooldown_until_ms)
        .is_some_and(|until| until > now)
        || account_health_key(candidate)
            .and_then(|key| memory.accounts.get(&key))
            .and_then(|health| health.cooldown_until_ms)
            .is_some_and(|until| until > now)
}

#[derive(Default)]
struct StreamCompletion {
    parser: crate::services::account_proxy::SseParser,
    headers: HeaderMap,
    protocol: RouteProtocol,
    response_id: Option<String>,
    failure: Option<crate::services::account_proxy::ProxyError>,
    meaningful: bool,
    accounted: bool,
    legitimate_empty: bool,
    finish_reason: bool,
    done: bool,
}

impl StreamCompletion {
    fn push(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        let frames = self.parser.push(bytes);
        self.observe(frames)
    }

    fn observe(&mut self, frames: Vec<(String, String)>) -> Result<(), std::io::Error> {
        for (event, data) in frames {
            if self.done || data.trim().is_empty() {
                continue;
            }
            if data.trim() == "[DONE]" {
                if self.protocol == RouteProtocol::Chat {
                    self.done = true;
                    self.require_output()?;
                }
                continue;
            }
            let value: Value = serde_json::from_str(&data)
                .map_err(|_| std::io::Error::other("Invalid upstream event stream"))?;
            if let Some(error) =
                crate::services::account_proxy::stream_error(&event, &data, &self.headers)
            {
                if self.protocol == RouteProtocol::Responses {
                    self.failure = Some(error);
                    self.done = true;
                    continue;
                }
                return Err(std::io::Error::other(error));
            }
            if self.protocol == RouteProtocol::Responses {
                let kind = value["type"].as_str().unwrap_or(&event);
                self.meaningful |= matches!(
                    kind,
                    "response.output_text.delta"
                        | "response.reasoning_summary_text.delta"
                        | "response.reasoning_text.delta"
                        | "response.refusal.delta"
                        | "response.audio.delta"
                        | "response.output_audio.delta"
                        | "response.audio_transcript.delta"
                        | "response.output_audio_transcript.delta"
                ) && nonempty_text(&value["delta"]);
                self.meaningful |= output_item_has_content(&value["item"])
                    || content_part_has_content(&value["part"]);
                self.accounted |= positive_token_usage(&value["response"]["usage"]);
                if matches!(kind, "response.completed" | "response.incomplete") {
                    if !value["response"]["error"].is_null() {
                        self.failure = crate::services::account_proxy::stream_error(
                            "response.failed",
                            &data,
                            &self.headers,
                        );
                        self.done = true;
                        continue;
                    }
                    let expected_status = kind.trim_start_matches("response.");
                    if (!event.is_empty() && event != kind)
                        || value["response"]["object"] != "response"
                        || value["response"]["status"] != expected_status
                        || !value["response"]["id"]
                            .as_str()
                            .is_some_and(|id| !id.is_empty())
                    {
                        return Err(std::io::Error::other(
                            "Responses terminal event has no matching terminal response",
                        ));
                    }
                    self.done = true;
                    self.response_id = value["response"]["id"].as_str().map(str::to_owned);
                    self.meaningful |= response_has_content(&value["response"]);
                    self.legitimate_empty |= legitimate_empty_response(&value["response"]);
                    self.require_output()?;
                }
                continue;
            }
            self.accounted |= positive_token_usage(&value["usage"]);
            if let Some(choices) = value["choices"].as_array() {
                for choice in choices {
                    self.meaningful |= chat_message_has_content(&choice["delta"]);
                    self.legitimate_empty |= matches!(
                        choice["finish_reason"].as_str(),
                        Some("length" | "content_filter")
                    );
                }
            }
            self.finish_reason |=
                value
                    .get("choices")
                    .and_then(Value::as_array)
                    .is_some_and(|choices| {
                        choices.iter().any(|choice| {
                            choice
                                .get("finish_reason")
                                .and_then(Value::as_str)
                                .is_some_and(|reason| !reason.is_empty())
                        })
                    });
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), std::io::Error> {
        let frames = self.parser.finish();
        self.observe(frames)?;
        if self.done || self.finish_reason {
            if self.failure.is_some() {
                Ok(())
            } else {
                self.require_output()
            }
        } else {
            Err(std::io::Error::other(
                "Upstream stream ended before completion",
            ))
        }
    }

    fn require_output(&self) -> Result<(), std::io::Error> {
        if self.meaningful || self.accounted || self.legitimate_empty {
            Ok(())
        } else {
            Err(std::io::Error::other(empty_response_error()))
        }
    }
}

async fn preflight_stream<S>(
    first_chunk: Bytes,
    upstream: &mut S,
    completion: &mut StreamCompletion,
    preserve_native_failure: bool,
) -> Result<VecDeque<Bytes>, axum::Error>
where
    S: futures_util::Stream<Item = Result<Bytes, axum::Error>> + Unpin,
{
    let mut buffered = VecDeque::new();
    let mut buffered_bytes = 0usize;
    let mut first = Some(first_chunk);
    loop {
        let chunk = if let Some(chunk) = first.take() {
            chunk
        } else {
            match upstream.next().await {
                Some(chunk) => chunk?,
                None => break,
            }
        };
        if chunk.is_empty() {
            continue;
        }
        completion.push(&chunk).map_err(axum::Error::new)?;
        if let Some(error) = &completion.failure {
            if !preserve_native_failure
                || crate::services::account_proxy::context_limit_error(
                    error.error_code.as_deref(),
                    &error.message,
                )
            {
                return Err(axum::Error::new(error.clone()));
            }
        }
        buffered_bytes = buffered_bytes.saturating_add(chunk.len());
        buffered.push_back(chunk);
        if completion.meaningful
            || completion.done
            || (completion.finish_reason && (completion.accounted || completion.legitimate_empty))
        {
            return Ok(buffered);
        }
        if buffered_bytes > MAX_STREAM_PREFLIGHT_BYTES {
            return Err(axum::Error::new(std::io::Error::other(
                "Upstream stream sent too much metadata before any output",
            )));
        }
    }
    completion.finish().map_err(axum::Error::new)?;
    if let Some(error) = &completion.failure {
        if !preserve_native_failure
            || crate::services::account_proxy::context_limit_error(
                error.error_code.as_deref(),
                &error.message,
            )
        {
            return Err(axum::Error::new(error.clone()));
        }
    }
    Ok(buffered)
}

fn nonempty_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn positive_token_usage(usage: &Value) -> bool {
    [
        "input_tokens",
        "output_tokens",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
    ]
    .iter()
    .any(|field| usage[*field].as_f64().is_some_and(|tokens| tokens > 0.0))
}

fn content_part_has_content(part: &Value) -> bool {
    nonempty_text(&part["text"])
        || nonempty_text(&part["refusal"])
        || nonempty_text(&part["thinking"])
        || part["type"].as_str().is_some_and(|kind| {
            !kind.is_empty()
                && !matches!(
                    kind,
                    "text" | "input_text" | "output_text" | "summary_text" | "refusal" | "thinking"
                )
        })
}

fn output_item_has_content(item: &Value) -> bool {
    match item["type"].as_str() {
        Some("message" | "reasoning") => {
            nonempty_text(&item["content"])
                || nonempty_text(&item["encrypted_content"])
                || (item["type"] == "reasoning" && nonempty_text(&item["id"]))
                || ["content", "summary"].iter().any(|field| {
                    item[*field]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(content_part_has_content))
                })
        }
        Some(kind) => !kind.is_empty(),
        None => false,
    }
}

fn response_has_content(response: &Value) -> bool {
    response["output"]
        .as_array()
        .is_some_and(|items| items.iter().any(output_item_has_content))
}

fn chat_message_has_content(message: &Value) -> bool {
    nonempty_text(&message["content"])
        || nonempty_text(&message["reasoning_content"])
        || nonempty_text(&message["refusal"])
        || nonempty_text(&message["audio"]["data"])
        || nonempty_text(&message["audio"]["transcript"])
        || message["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(content_part_has_content))
        || message["images"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(content_part_has_content))
        || message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
        || message["function_call"].is_object()
}

fn empty_completed_response(response: &Value) -> bool {
    response["status"] == "completed"
        && !response_has_content(response)
        && !positive_token_usage(&response["usage"])
        && !legitimate_empty_response(response)
}

fn empty_chat_response(response: &Value) -> bool {
    let Some(choices) = response["choices"].as_array() else {
        return false;
    };
    !positive_token_usage(&response["usage"])
        && choices.iter().all(|choice| {
            let Some(message) = choice["message"].as_object() else {
                return false;
            };
            message.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "role"
                        | "content"
                        | "reasoning_content"
                        | "refusal"
                        | "tool_calls"
                        | "function_call"
                        | "audio"
                        | "images"
                )
            }) && !chat_message_has_content(&choice["message"])
                && !matches!(
                    choice["finish_reason"].as_str(),
                    Some("length" | "content_filter")
                )
        })
}

fn legitimate_empty_response(response: &Value) -> bool {
    response["status"] == "incomplete"
        || matches!(
            response["incomplete_details"]["reason"].as_str(),
            Some("max_output_tokens" | "content_filter")
        )
}

fn empty_response_error() -> crate::services::account_proxy::ProxyError {
    let mut error = crate::services::account_proxy::ProxyError::new(
        StatusCode::BAD_GATEWAY,
        "Upstream completed without any output or token usage",
    );
    error.error_code = Some("empty_response".into());
    error
}

fn proxy_error_from_chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a crate::services::account_proxy::ProxyError> {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(proxy) = error.downcast_ref::<crate::services::account_proxy::ProxyError>() {
            return Some(proxy);
        }
        cause = error
            .downcast_ref::<std::io::Error>()
            .and_then(|error| {
                error
                    .get_ref()
                    .map(|inner| inner as &(dyn std::error::Error + 'static))
            })
            .or_else(|| error.source());
    }
    None
}

fn mark_stream_failure(
    state: &AppState,
    candidate: &Candidate,
    error: &(dyn std::error::Error + 'static),
) -> Option<FailureClass> {
    if let Some(proxy) = proxy_error_from_chain(error) {
        if let Some(source) = &candidate.account_source {
            if (matches!(
                source.provider,
                AccountProvider::Antigravity | AccountProvider::Copilot
            ) && matches!(
                proxy.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            )) || (source.provider == AccountProvider::Copilot
                && matches!(
                    proxy.error_code.as_deref(),
                    Some("model_not_found" | "model_not_supported")
                ))
            {
                crate::services::account_proxy::forget_preparation_metadata(
                    source.provider,
                    &source.account_id,
                );
            }
        }
        let mut headers = HeaderMap::new();
        if let Some(value) = &proxy.retry_after {
            headers.insert(header::RETRY_AFTER, value.clone());
        }
        let body = json!({"error":{"message":proxy.message,"code":proxy.error_code,"type":proxy.error_type}}).to_string();
        let failure = classify_failure(proxy.status, body.as_bytes())?;
        mark_failure_scoped(
            state,
            candidate,
            failure,
            retry_after(&headers),
            proxy.model_scope,
        );
        return Some(failure);
    }
    mark_failure(state, candidate, FailureClass::Transient, None);
    Some(FailureClass::Transient)
}

fn begin_attempt(state: &AppState, candidate: &mut Candidate) {
    let Ok(memory) = state.route_memory.lock() else {
        return;
    };
    candidate.attempt = Some(Arc::new(AttemptHealth {
        revision: memory.revision,
        account_epoch: account_health_key(candidate)
            .and_then(|key| memory.account_epochs.get(&key).copied())
            .unwrap_or(0),
        credential_version: Mutex::new(None),
    }));
}

fn attempt_is_current(memory: &RouteMemory, candidate: &Candidate) -> bool {
    let Some(attempt) = &candidate.attempt else {
        return true;
    };
    let Some(key) = account_health_key(candidate) else {
        return true;
    };
    if memory.account_epochs.get(&key).copied().unwrap_or(0) != attempt.account_epoch {
        return false;
    }
    let version = attempt.credential_version.lock().ok();
    match (
        version.as_deref().and_then(|value| value.as_ref()),
        memory.credential_versions.get(&key),
    ) {
        (Some(attempted), Some((current, _))) => attempted == current,
        _ => true,
    }
}

fn clear_account_authentication(
    memory: &mut RouteMemory,
    key: &str,
    candidate_ids: &HashSet<String>,
) -> bool {
    let before = memory.candidates.len() + memory.accounts.len();
    memory.candidates.retain(|id, health| {
        !(health.failure == Some(FailureClass::Authentication)
            && (health.account_key.as_deref() == Some(key) || candidate_ids.contains(id)))
    });
    if memory
        .accounts
        .get(key)
        .is_some_and(|health| health.failure == Some(FailureClass::Authentication))
    {
        memory.accounts.remove(key);
    }
    before != memory.candidates.len() + memory.accounts.len()
}

pub(crate) fn forget_account_authentication(
    provider: crate::models::model::AccountProvider,
    account_id: &str,
) {
    crate::services::account_proxy::forget_preparation_metadata(provider, account_id);
    let source = crate::models::model::AccountSource {
        provider,
        account_id: account_id.into(),
    };
    let key = source_health_key(&source);
    let candidate_ids = model_manager::get_user_models()
        .into_iter()
        .filter(|model| model.account_source.as_ref() == Some(&source))
        .map(|model| model.internal_id)
        .collect();
    let memory = shared_route_memory();
    let Ok(mut memory) = memory.lock() else {
        return;
    };
    let epoch = memory.account_epochs.entry(key.clone()).or_default();
    *epoch = epoch.saturating_add(1);
    memory.credential_versions.remove(&key);
    memory.revision = memory.revision.saturating_add(1);
    if clear_account_authentication(&mut memory, &key, &candidate_ids) {
        save_route_memory(&route_memory_path(), &memory);
    }
}

fn bind_attempt_credentials(state: &AppState, candidate: &Candidate, credentials: &Value) {
    let Some(attempt) = &candidate.attempt else {
        return;
    };
    let Some(key) = account_health_key(candidate) else {
        return;
    };
    let version = hex::encode(Sha256::digest(credentials.to_string().as_bytes()));
    let Ok(mut memory) = state.route_memory.lock() else {
        return;
    };
    if memory.account_epochs.get(&key).copied().unwrap_or(0) != attempt.account_epoch {
        return;
    }
    let Ok(mut attempted_version) = attempt.credential_version.lock() else {
        return;
    };
    let current = memory.credential_versions.get(&key);
    let may_update = current.map_or(true, |(current, revision)| {
        current == &version
            || attempted_version.as_ref() == Some(current)
            || *revision <= attempt.revision
    });
    *attempted_version = Some(version.clone());
    if may_update && current.map_or(true, |(current, _)| current != &version) {
        memory.revision = memory.revision.saturating_add(1);
        let revision = memory.revision;
        memory
            .credential_versions
            .insert(key.clone(), (version, revision));
        if clear_account_authentication(&mut memory, &key, &HashSet::new()) {
            persist_route_memory(state, &memory);
        }
    }
}

fn mark_success(state: &AppState, candidate: &Candidate) {
    let Ok(mut memory) = state.route_memory.lock() else {
        return;
    };
    if !attempt_is_current(&memory, candidate) {
        return;
    }
    let revision = candidate
        .attempt
        .as_ref()
        .map_or(u64::MAX, |attempt| attempt.revision);
    let may_clear = |health: &CandidateHealth| {
        health.revision <= revision
            && !(matches!(
                health.failure,
                Some(FailureClass::Billing | FailureClass::RateLimit)
            ) && health
                .cooldown_until_ms
                .is_some_and(|until| until > now_ms()))
    };
    let removed_failure = memory
        .candidates
        .get(&candidate.internal_id)
        .is_some_and(may_clear)
        && memory.candidates.remove(&candidate.internal_id).is_some();
    let removed_account = account_health_key(candidate).is_some_and(|key| {
        memory.accounts.get(&key).is_some_and(may_clear) && memory.accounts.remove(&key).is_some()
    });
    if removed_failure || removed_account {
        persist_route_memory(state, &memory);
    }
}

fn mark_failure(
    state: &AppState,
    candidate: &Candidate,
    failure: FailureClass,
    retry_after: Option<Duration>,
) {
    mark_failure_scoped(state, candidate, failure, retry_after, false);
}

fn mark_failure_scoped(
    state: &AppState,
    candidate: &Candidate,
    failure: FailureClass,
    retry_after: Option<Duration>,
    model_scope: bool,
) {
    if failure == FailureClass::RequestIncompatible {
        return;
    }
    let Ok(mut memory) = state.route_memory.lock() else {
        return;
    };
    if !attempt_is_current(&memory, candidate) {
        return;
    }
    memory.revision = memory.revision.saturating_add(1);
    let revision = memory.revision;
    let entry = memory
        .candidates
        .entry(candidate.internal_id.clone())
        .or_default();
    if entry.fingerprint != candidate.fingerprint {
        *entry = CandidateHealth {
            fingerprint: candidate.fingerprint.clone(),
            ..CandidateHealth::default()
        };
    }
    entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
    let cooldown = cooldown_for_failure(failure, retry_after, entry.consecutive_failures);
    let deadline = now_ms().saturating_add(cooldown.as_millis().min(u128::from(u64::MAX)) as u64);
    if !entry
        .cooldown_until_ms
        .is_some_and(|until| until > deadline)
    {
        entry.failure = Some(failure);
        entry.cooldown_until_ms = Some(deadline);
    }
    entry.revision = revision;
    entry.account_key = account_health_key(candidate);
    let consecutive_failures = entry.consecutive_failures;
    if !model_scope
        && matches!(
            failure,
            FailureClass::Authentication | FailureClass::Billing | FailureClass::RateLimit
        )
    {
        if let Some(key) = account_health_key(candidate) {
            let existing = memory.accounts.get(&key);
            if !existing
                .and_then(|entry| entry.cooldown_until_ms)
                .is_some_and(|until| until > deadline)
            {
                memory.accounts.insert(
                    key.clone(),
                    CandidateHealth {
                        consecutive_failures,
                        cooldown_until_ms: Some(deadline),
                        fingerprint: candidate.fingerprint.clone(),
                        failure: Some(failure),
                        revision,
                        account_key: Some(key),
                    },
                );
            }
        }
    }
    persist_route_memory(state, &memory);
}

fn cooldown_for_failure(
    failure: FailureClass,
    retry_after: Option<Duration>,
    consecutive_failures: u32,
) -> Duration {
    match failure {
        FailureClass::Authentication | FailureClass::Billing => AUTH_OR_BILLING_COOLDOWN,
        FailureClass::ModelUnavailable => MODEL_UNAVAILABLE_COOLDOWN,
        FailureClass::RateLimit => retry_after
            .unwrap_or(RATE_LIMIT_COOLDOWN)
            .min(MAX_RETRY_AFTER),
        FailureClass::Transient => {
            let multiplier = 1_u32 << consecutive_failures.saturating_sub(1).min(3);
            TRANSIENT_COOLDOWN
                .saturating_mul(multiplier)
                .min(MAX_TRANSIENT_COOLDOWN)
        }
        FailureClass::RequestIncompatible => Duration::ZERO,
    }
}

fn remaining_timeout(started: Instant, budget: Duration, cap: Duration) -> Option<Duration> {
    budget
        .checked_sub(started.elapsed())
        .map(|remaining| remaining.min(cap))
        .filter(|remaining| !remaining.is_zero())
}

fn classify_failure(status: StatusCode, body: &[u8]) -> Option<FailureClass> {
    if is_context_limit_body(body) {
        return None;
    }
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    if [
        "content policy",
        "safety policy",
        "content moderation",
        "blocked by safety",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return None;
    }
    if [
        "does not support tool",
        "tools are not supported",
        "tool use is not supported",
        "function calling is not supported",
        "tools_not_supported",
        "does not support image",
        "images are not supported",
        "vision is not supported",
        "responses_conversion_unsupported",
        "unsupported_parameter",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return Some(FailureClass::RequestIncompatible);
    }
    if [
        "model_not_found",
        "model not found",
        "model does not exist",
        "model is not available",
        "model unavailable",
        "unknown model",
        "invalid model",
        "model_not_supported",
        "model has been deprecated",
        "model has been decommissioned",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return Some(FailureClass::ModelUnavailable);
    }
    if [
        "insufficient quota",
        "insufficient_quota",
        "quota exceeded",
        "credit balance",
        "credits exhausted",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
    {
        return Some(FailureClass::Billing);
    }
    let status_failure = match status.as_u16() {
        401 | 403 => Some(FailureClass::Authentication),
        402 => Some(FailureClass::Billing),
        404 => Some(FailureClass::ModelUnavailable),
        408 | 409 => Some(FailureClass::Transient),
        429 => Some(FailureClass::RateLimit),
        500..=599 => Some(FailureClass::Transient),
        _ => None,
    };
    if status_failure.is_some() {
        return status_failure;
    }
    if ["rate limit", "rate_limit", "too many requests"]
        .iter()
        .any(|pattern| message.contains(pattern))
    {
        return Some(FailureClass::RateLimit);
    }
    None
}

fn is_context_limit_body(body: &[u8]) -> bool {
    let value = serde_json::from_slice::<Value>(body).unwrap_or(Value::Null);
    let error = value
        .pointer("/response/error")
        .filter(|error| !error.is_null())
        .or_else(|| value.get("error").filter(|error| !error.is_null()))
        .unwrap_or(&value);
    let code = error["code"].as_str().or_else(|| error["type"].as_str());
    let message = String::from_utf8_lossy(body);
    crate::services::account_proxy::context_limit_error(code, &message)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(MAX_RETRY_AFTER));
    }

    let retry_at = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let wait = retry_at.signed_duration_since(chrono::Utc::now());
    Some(wait.to_std().unwrap_or_default().min(MAX_RETRY_AFTER))
}

fn chat_completions_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else {
        format!("{base_url}/chat/completions")
    }
}

fn responses_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/responses") {
        return base.to_string();
    }
    let base = base.strip_suffix("/chat/completions").unwrap_or(base);
    format!("{base}/responses")
}

fn has_previous_response(body: &Value) -> bool {
    body.get("previous_response_id")
        .is_some_and(|value| !value.is_null())
}

fn response_candidates(
    state: &AppState,
    body: &Value,
    candidates: Vec<Candidate>,
    protocol: RouteProtocol,
) -> Result<Vec<Candidate>, &'static str> {
    if protocol != RouteProtocol::Responses || !has_previous_response(body) {
        return Ok(candidates);
    }
    let id = body["previous_response_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or("previous_response_id must be a response ID")?;
    let binding = state
        .route_memory
        .lock()
        .ok()
        .and_then(|memory| memory.response_bindings.get(id).cloned())
        .filter(|binding| binding.expires_at_ms > now_ms())
        .ok_or("Unknown or expired response ID; send the complete conversation input")?;
    if binding.ambiguous {
        return Err("Ambiguous response ID; send the complete conversation input");
    }
    let candidate = candidates.into_iter()
        .find(|candidate| candidate.internal_id == binding.candidate_id && candidate.fingerprint == binding.fingerprint)
        .ok_or("The original response provider configuration has changed; send the complete conversation input")?;
    Ok(vec![candidate])
}

fn remember_response(state: &AppState, candidate: &Candidate, id: Option<&str>) {
    let Some(id) = id.filter(|id| !id.is_empty() && id.len() <= 1024) else {
        return;
    };
    let Ok(mut memory) = state.route_memory.lock() else {
        return;
    };
    if !attempt_is_current(&memory, candidate) {
        return;
    }
    let now = now_ms();
    memory
        .response_bindings
        .retain(|_, binding| binding.expires_at_ms > now);
    if let Some(binding) = memory.response_bindings.get_mut(id) {
        if binding.ambiguous
            || binding.candidate_id != candidate.internal_id
            || binding.fingerprint != candidate.fingerprint
        {
            binding.ambiguous = true;
            binding.expires_at_ms = now.saturating_add(RESPONSE_BINDING_LIFETIME_MS);
            persist_route_memory(state, &memory);
            return;
        }
    }
    if memory.response_bindings.len() >= MAX_RESPONSE_BINDINGS
        && !memory.response_bindings.contains_key(id)
    {
        let oldest = memory
            .response_bindings
            .iter()
            .min_by_key(|(_, binding)| binding.expires_at_ms)
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            memory.response_bindings.remove(&oldest);
        }
    }
    memory.response_bindings.insert(
        id.into(),
        ResponseBinding {
            candidate_id: candidate.internal_id.clone(),
            fingerprint: candidate.fingerprint.clone(),
            expires_at_ms: now.saturating_add(RESPONSE_BINDING_LIFETIME_MS),
            ambiguous: false,
        },
    );
    persist_route_memory(state, &memory);
}

fn remember_unsupported_responses(state: &AppState, candidate: &Candidate) {
    let Ok(mut memory) = state.route_memory.lock() else {
        return;
    };
    let now = now_ms();
    memory.unsupported_responses.retain(|_, until| *until > now);
    if memory.unsupported_responses.len() >= 128 {
        let oldest = memory
            .unsupported_responses
            .iter()
            .min_by_key(|(_, until)| *until)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            memory.unsupported_responses.remove(&oldest);
        }
    }
    memory.unsupported_responses.insert(
        candidate.fingerprint.clone(),
        now.saturating_add(RESPONSES_CAPABILITY_LIFETIME_MS),
    );
}

fn responses_endpoint_unsupported(status: StatusCode, body: &[u8]) -> bool {
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    if continuation_error(body) || message.contains("safety") || message.contains("content policy")
    {
        return false;
    }
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST)
        && [
            "responses_not_supported",
            "model does not support responses",
            "model does not support the responses",
            "responses is not supported for this model",
            "responses are not supported for this model",
        ]
        .iter()
        .any(|pattern| message.contains(pattern))
    {
        return true;
    }
    if message.contains("model") && !message.contains("endpoint_not_found") {
        return false;
    }
    if matches!(
        status,
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    ) {
        return true;
    }
    if status == StatusCode::NOT_FOUND
        && matches!(message.trim(), "" | "not found" | "404 page not found")
    {
        return true;
    }
    matches!(status, StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST)
        && [
            "endpoint_not_found",
            "unsupported_endpoint",
            "unsupported endpoint",
            "responses is not supported",
            "responses api is not supported",
            "responses endpoint is not supported",
            "post /v1/responses is not supported",
        ]
        .iter()
        .any(|pattern| message.contains(pattern))
}

fn continuation_error(body: &[u8]) -> bool {
    let message = String::from_utf8_lossy(body).to_ascii_lowercase();
    [
        "previous_response",
        "previous response",
        "response_not_found",
        "response not found",
        "response has expired",
    ]
    .iter()
    .any(|pattern| message.contains(pattern))
}

fn is_continuation_failure(status: StatusCode, body: &[u8]) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::CONFLICT
            | StatusCode::UNPROCESSABLE_ENTITY
    ) && continuation_error(body)
}

fn mark_response_failure(
    state: &AppState,
    candidate: &Candidate,
    error: &(dyn std::error::Error + 'static),
    bound_continuation: bool,
) -> Option<FailureClass> {
    if bound_continuation && proxy_error_from_chain(error).is_some_and(|proxy| {
        let body = json!({"error":{"message":proxy.message,"code":proxy.error_code,"type":proxy.error_type}}).to_string();
        is_continuation_failure(proxy.status, body.as_bytes())
    }) {
        return None;
    }
    mark_stream_failure(state, candidate, error)
}

fn copy_response_headers(response: &mut Response, headers: &HeaderMap) {
    for (name, value) in headers {
        if name != header::CONTENT_LENGTH
            && name != header::TRANSFER_ENCODING
            && name != header::CONNECTION
            && name != header::CONTENT_ENCODING
            && name != "x-echobird-routed-via"
        {
            response.headers_mut().insert(name.clone(), value.clone());
        }
    }
}

fn is_authorized(headers: &HeaderMap, expected_key: &str) -> bool {
    let bearer_matches = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|key| key == expected_key);
    bearer_matches
        || headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|key| key == expected_key)
}

fn routed_via(candidate: &Candidate) -> HeaderValue {
    let value = format!("{}/{}", candidate.internal_id, candidate.model_id);
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("echobird/unknown"))
}

fn upstream_response(
    status: StatusCode,
    content_type: Option<HeaderValue>,
    cache_control: Option<HeaderValue>,
    body: Bytes,
    candidate: &Candidate,
) -> Response {
    response_with_headers(
        status,
        content_type,
        cache_control,
        Body::from(body),
        candidate,
    )
}

fn streaming_response(
    content_type: Option<HeaderValue>,
    cache_control: Option<HeaderValue>,
    body: Body,
    candidate: &Candidate,
) -> Response {
    response_with_headers(StatusCode::OK, content_type, cache_control, body, candidate)
}

fn response_with_headers(
    status: StatusCode,
    content_type: Option<HeaderValue>,
    cache_control: Option<HeaderValue>,
    body: Body,
    candidate: &Candidate,
) -> Response {
    let mut response = Response::builder()
        .status(status)
        .header("X-EchoBird-Routed-Via", routed_via(candidate));
    if let Some(content_type) = content_type {
        response = response.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(cache_control) = cache_control {
        response = response.header(header::CACHE_CONTROL, cache_control);
    }
    response.body(body).unwrap_or_else(|_| {
        openai_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to build router response",
        )
    })
}

fn openai_error(status: StatusCode, message: &str) -> Response {
    json_error(status, message, None)
}

fn json_error(status: StatusCode, message: &str, attempts: Option<Value>) -> Response {
    let mut error = json!({
        "message": message,
        "type": "echobird_router_error",
        "code": status.as_u16(),
    });
    if let Some(attempts) = attempts {
        error["attempts"] = attempts;
    }
    (status, Json(json!({ "error": error }))).into_response()
}

#[cfg(test)]
#[path = "responses_tests.rs"]
mod responses_tests;

#[cfg(test)]
#[path = "audit_tests.rs"]
mod audit_tests;

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::post;
    use http_body_util::BodyExt;
    use tokio_stream::wrappers::ReceiverStream;

    use super::*;

    fn candidate(addr: SocketAddr, id: &str) -> Candidate {
        let model_id = format!("{id}-model");
        let base_url = format!("http://{addr}/v1");
        let api_key = "upstream-key".to_string();
        Candidate {
            attempt: None,
            internal_id: id.to_string(),
            fingerprint: candidate_fingerprint(&model_id, &base_url, &api_key),
            model_id,
            base_url,
            api_key,
            account_source: None,
        }
    }

    fn inbound_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer local-key"),
        );
        headers
    }

    async fn spawn_mock(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    #[test]
    fn resolves_keyless_catalog_entries_without_admitting_other_blank_keys() {
        let models: Vec<crate::models::model::ModelConfig> = serde_json::from_value(json!([
            {"internalId":"kilo", "name":"Kilo", "modelId":"kilo-auto/free",
                "baseUrl":"https://api.kilo.ai/api/gateway", "apiKey":""},
            {"internalId":"zen", "name":"Zen", "modelId":"space-bunny-free",
                "baseUrl":"https://opencode.ai/zen/v1", "apiKey":"   "},
            {"internalId":"zen-free", "name":"Zen Free", "modelId":"space-bunny-free",
                "baseUrl":"https://opencode.ai/inference/v1", "apiKey":""},
            {"internalId":"paid", "name":"Paid", "modelId":"claude-opus-5",
                "baseUrl":"https://opencode.ai/zen/v1", "apiKey":""},
            {"internalId":"spoof", "name":"Spoof", "modelId":"kilo-auto/free",
                "baseUrl":"https://example.com/v1", "apiKey":""},
            {"internalId":"blank", "name":"Blank", "modelId":"ordinary-model",
                "baseUrl":"https://example.com/v1", "apiKey":""},
            {"internalId":"account", "name":"Account", "modelId":"account-model",
                "baseUrl":"account://kilo", "apiKey":"",
                "accountSource":{"provider":"kilo", "accountId":"saved-account"}},
            {"internalId":"loop", "name":"Loop", "modelId":"auto",
                "baseUrl":"http://127.0.0.1:53683/v1", "apiKey":"key"}
        ]))
        .unwrap();
        let resolved = resolve_candidates_from(
            [
                "paid", "zen", "account", "missing", "kilo", "zen-free", "spoof", "blank", "loop",
            ]
            .map(String::from)
            .to_vec(),
            &models,
            &crate::services::local_llm::LocalServerInfo::default(),
        );
        assert_eq!(
            resolved
                .iter()
                .map(|item| item.internal_id.as_str())
                .collect::<Vec<_>>(),
            ["account", "kilo", "zen-free"]
        );
        assert!(resolved.iter().all(|item| item.api_key.trim().is_empty()));
        assert!(resolved[0].account_source.is_some());
    }

    #[tokio::test]
    async fn blank_upstream_keys_send_no_authorization_for_chat_or_responses() {
        let captured = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let handler = {
            let captured = captured.clone();
            move |headers: HeaderMap| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(
                        headers
                            .get(header::AUTHORIZATION)
                            .map(|value| value.to_str().unwrap().to_string()),
                    );
                    Json(json!({
                        "id":"response-auth", "object":"response", "status":"completed",
                        "model":"fixture-model", "output":[],
                        "choices":[{"message":{"role":"assistant", "content":"ok"}}]
                    }))
                }
            }
        };
        let addr = spawn_mock(
            Router::new()
                .route("/v1/chat/completions", post(handler.clone()))
                .route("/v1/responses", post(handler)),
        )
        .await;
        let state = AppState::for_tests().unwrap();
        for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
            for key in ["", " \t ", "upstream-key"] {
                let mut upstream = candidate(addr, "keyless");
                upstream.api_key = key.into();
                let response = send_api_request(
                    &state,
                    &inbound_headers(),
                    &upstream,
                    &json!({"model":"fixture-model", "stream":false}),
                    protocol,
                )
                .await
                .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            }
        }
        assert_eq!(
            *captured.lock().unwrap(),
            vec![
                None,
                None,
                Some("Bearer upstream-key".into()),
                None,
                None,
                Some("Bearer upstream-key".into())
            ]
        );
    }

    #[test]
    fn zen_requires_a_user_key_even_for_models_named_free() {
        let mut upstream = candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "free");
        upstream.api_key.clear();
        upstream.base_url = "https://opencode.ai/zen/v1".into();
        upstream.model_id = "space-bunny-free".into();
        assert_eq!(upstream_bearer_token(&upstream), None);
        upstream.api_key = "real-account-key".into();
        assert_eq!(upstream_bearer_token(&upstream), Some("real-account-key"));
        upstream.api_key.clear();
        upstream.model_id = "claude-opus-5".into();
        assert_eq!(upstream_bearer_token(&upstream), None);
        upstream.model_id = "space-bunny-free".into();
        upstream.base_url = "https://example.com/zen/v1".into();
        assert_eq!(upstream_bearer_token(&upstream), None);
        upstream.base_url = "https://api.kilo.ai/api/gateway".into();
        upstream.model_id = "kilo-auto/free".into();
        assert_eq!(upstream_bearer_token(&upstream), None);
    }

    #[tokio::test]
    #[ignore = "Calls a public upstream model; run explicitly for live verification"]
    async fn live_keyless_kilo_routes_through_the_actual_candidate_resolver() {
        let model: crate::models::model::ModelConfig = serde_json::from_value(json!({
            "internalId":"live-keyless-kilo", "name":"Kilo AI",
            "modelId":"kilo-auto/free", "baseUrl":"https://api.kilo.ai/api/gateway",
            "apiKey":""
        }))
        .unwrap();
        let candidates = resolve_candidates_from(
            vec![model.internal_id.clone()],
            &[model],
            &crate::services::local_llm::LocalServerInfo::default(),
        );
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].api_key.is_empty());
        let state = AppState::for_tests().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_str(&format!("EchoBird/{}", env!("CARGO_PKG_VERSION"))).unwrap(),
        );
        let response = route_chat_with_candidates(
            &state,
            &headers,
            json!({"model":SMART_ROUTER_MODEL_ID, "stream":false, "max_tokens":128,
                "messages":[{"role":"user", "content":"Reply with exactly OK and nothing else."}]}),
            candidates,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            reply["choices"][0]["message"]["content"]
                .as_str()
                .map(str::trim),
            Some("OK")
        );
    }

    #[test]
    fn route_activity_tracks_the_latest_in_flight_candidate() {
        let mut activity = RouteActivity::default();
        let first = activity.begin("first", 10);
        let second = activity.begin("second", 20);

        let snapshot = activity.snapshot();
        assert_eq!(snapshot.candidate_id.as_deref(), Some("second"));
        assert!(snapshot.active);

        activity.end(second, 30);
        let snapshot = activity.snapshot();
        assert_eq!(snapshot.candidate_id.as_deref(), Some("first"));
        assert!(snapshot.active);

        activity.end(first, 40);
        let snapshot = activity.snapshot();
        assert_eq!(snapshot.candidate_id.as_deref(), Some("first"));
        assert!(!snapshot.active);
        assert_eq!(snapshot.sequence, 4);
        assert_eq!(snapshot.updated_at_ms, 40);
    }

    #[test]
    fn classifies_only_failover_worthy_statuses() {
        assert_eq!(
            classify_failure(StatusCode::UNAUTHORIZED, b""),
            Some(FailureClass::Authentication)
        );
        assert_eq!(
            classify_failure(StatusCode::TOO_MANY_REQUESTS, b""),
            Some(FailureClass::RateLimit)
        );
        assert_eq!(
            classify_failure(
                StatusCode::TOO_MANY_REQUESTS,
                br#"{"error":{"code":"insufficient_quota"}}"#
            ),
            Some(FailureClass::Billing)
        );
        assert_eq!(
            classify_failure(StatusCode::REQUEST_TIMEOUT, b""),
            Some(FailureClass::Transient)
        );
        assert_eq!(
            classify_failure(StatusCode::BAD_GATEWAY, b""),
            Some(FailureClass::Transient)
        );
        assert_eq!(
            classify_failure(StatusCode::BAD_REQUEST, b"bad input"),
            None
        );
        assert_eq!(
            classify_failure(StatusCode::FORBIDDEN, b"blocked by safety policy"),
            None
        );
        assert_eq!(
            classify_failure(StatusCode::PAYLOAD_TOO_LARGE, b"too large"),
            None
        );
        assert_eq!(
            classify_failure(StatusCode::BAD_REQUEST, b"model_not_found"),
            Some(FailureClass::ModelUnavailable)
        );
        assert_eq!(
            classify_failure(StatusCode::BAD_REQUEST, b"unknown model"),
            Some(FailureClass::ModelUnavailable)
        );
        assert_eq!(
            classify_failure(StatusCode::BAD_REQUEST, b"insufficient quota"),
            Some(FailureClass::Billing)
        );
        assert_eq!(
            classify_failure(
                StatusCode::FORBIDDEN,
                b"tools are not supported: blocked by safety policy"
            ),
            None
        );
    }

    #[test]
    fn accepts_current_and_legacy_router_model_ids() {
        assert!(is_supported_router_model("auto"));
        assert!(is_supported_router_model("echobird/auto"));
        assert!(is_supported_router_model("echobird-auto"));
        assert!(is_supported_router_model("smart-router"));
        assert!(!is_supported_router_model("other-model"));
    }

    #[test]
    fn appends_chat_completions_once() {
        assert_eq!(
            chat_completions_url("https://example.com/v1"),
            "https://example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://example.com/v1/chat/completions"),
            "https://example.com/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn sends_openrouter_app_attribution_headers() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["HTTP-Referer"], "https://echobird.ai");
                assert_eq!(headers["X-OpenRouter-Title"], "EchoBird");
                assert_eq!(
                    headers["X-OpenRouter-Categories"],
                    "programming-app,personal-agent"
                );
                Json(json!({
                    "id": "chatcmpl-attribution",
                    "model": body["model"],
                    "choices": [{"message": {"role": "assistant", "content": "ok"}}]
                }))
            }),
        );
        let addr = spawn_mock(upstream).await;
        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": []}),
            vec![candidate(addr, "openrouter")],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn configured_order_wins_over_previous_success_and_survives_reordering() {
        let state = AppState::for_tests().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        let first = candidate(addr, "first");
        let second = candidate(addr, "second");
        let third = candidate(addr, "third");

        mark_success(&state, &third);
        let ordered =
            prioritized_candidates(&state, vec![first.clone(), second.clone(), third.clone()]);
        assert_eq!(
            ordered
                .iter()
                .map(|model| model.internal_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );

        let reordered = prioritized_candidates(&state, vec![second, third, first]);
        assert_eq!(
            reordered
                .iter()
                .map(|model| model.internal_id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "third", "first"]
        );
    }

    #[test]
    fn first_candidate_regains_priority_after_cooldown() {
        let state = AppState::for_tests().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        let first = candidate(addr, "first");
        let second = candidate(addr, "second");

        mark_failure(&state, &first, FailureClass::RateLimit, None);
        mark_success(&state, &second);
        let available = prioritized_candidates(&state, vec![first.clone(), second.clone()]);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].internal_id, "second");

        state
            .route_memory
            .lock()
            .unwrap()
            .candidates
            .get_mut("first")
            .unwrap()
            .cooldown_until_ms = Some(now_ms().saturating_sub(1));
        let recovered = prioritized_candidates(&state, vec![first, second]);
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].internal_id, "first");
    }

    #[test]
    fn exhausted_accounts_cool_down_all_bound_models_but_model_errors_stay_local() {
        use crate::models::model::{AccountProvider, AccountSource};
        let address = "127.0.0.1:8080".parse().unwrap();
        let mut first = candidate(address, "first");
        first.account_source = Some(AccountSource {
            provider: AccountProvider::Kilo,
            account_id: "account-one".into(),
        });
        let mut sibling = candidate(address, "sibling");
        sibling.account_source = first.account_source.clone();
        let mut other = candidate(address, "other");
        other.account_source = Some(AccountSource {
            provider: AccountProvider::Kilo,
            account_id: "account-two".into(),
        });
        let state = AppState::for_tests().unwrap();
        mark_failure(&state, &first, FailureClass::RateLimit, None);
        assert!(candidate_is_cooling_down(&state, &sibling));
        assert_eq!(
            prioritized_candidates(&state, vec![first.clone(), sibling.clone(), other])
                .iter()
                .map(|c| c.internal_id.as_str())
                .collect::<Vec<_>>(),
            vec!["other"]
        );
        let state = AppState::for_tests().unwrap();
        mark_failure(&state, &first, FailureClass::ModelUnavailable, None);
        assert!(!candidate_is_cooling_down(&state, &sibling));
        assert_eq!(
            prioritized_candidates(&state, vec![first, sibling]).len(),
            1
        );
    }

    #[test]
    fn legacy_success_memory_does_not_override_configured_order() {
        let state = AppState::for_tests().unwrap();
        *state.route_memory.lock().unwrap() = serde_json::from_value(json!({
            "version": 1,
            "lastSuccessId": "second",
            "candidates": {}
        }))
        .unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        let available = prioritized_candidates(
            &state,
            vec![candidate(addr, "first"), candidate(addr, "second")],
        );
        assert_eq!(available[0].internal_id, "first");
    }

    #[test]
    fn route_memory_survives_round_trip_and_skips_cooldown() {
        let state = AppState::for_tests().unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        let first = candidate(addr, "first");
        let second = candidate(addr, "second");

        mark_success(&state, &second);
        let ordered = prioritized_candidates(&state, vec![first.clone(), second.clone()]);
        assert_eq!(ordered[0].internal_id, "first");

        mark_failure(&state, &second, FailureClass::RateLimit, None);
        let serialized = {
            let memory = state.route_memory.lock().unwrap();
            serde_json::to_string(&*memory).unwrap()
        };
        let restored: RouteMemory = serde_json::from_str(&serialized).unwrap();
        let restored_state = AppState {
            fixture_responses: None,
            fixture_authentication: None,
            http_client: state.http_client.clone(),
            route_memory: Arc::new(Mutex::new(restored)),
            route_memory_path: None,
        };
        let available = prioritized_candidates(&restored_state, vec![first, second]);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].internal_id, "first");
    }

    #[test]
    fn cooldowns_match_failure_type_and_cap_retry_after() {
        assert_eq!(
            cooldown_for_failure(FailureClass::RateLimit, None, 1),
            Duration::from_secs(60)
        );
        assert_eq!(
            cooldown_for_failure(FailureClass::Transient, None, 4),
            Duration::from_secs(4 * 60)
        );
        assert_eq!(
            cooldown_for_failure(FailureClass::ModelUnavailable, None, 1),
            Duration::from_secs(6 * 60 * 60)
        );
        assert_eq!(
            cooldown_for_failure(FailureClass::RateLimit, Some(Duration::MAX), 1),
            MAX_RETRY_AFTER
        );
    }

    #[test]
    fn parses_retry_after_seconds_and_http_dates() {
        let mut headers = HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("3600"));
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(3600)));

        let retry_at = chrono::Utc::now() + chrono::Duration::seconds(90);
        let http_date = retry_at.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        headers.insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(&http_date).unwrap(),
        );
        let parsed = retry_after(&headers).unwrap();
        assert!(parsed >= Duration::from_secs(88));
        assert!(parsed <= Duration::from_secs(90));
    }

    #[test]
    fn expired_transient_cooldown_keeps_failure_streak() {
        let state = AppState::for_tests().unwrap();
        let model = candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "transient");

        mark_failure(&state, &model, FailureClass::Transient, None);
        {
            let mut memory = state.route_memory.lock().unwrap();
            memory
                .candidates
                .get_mut(&model.internal_id)
                .unwrap()
                .cooldown_until_ms = Some(now_ms().saturating_sub(1));
        }
        assert_eq!(prioritized_candidates(&state, vec![model.clone()]).len(), 1);

        mark_failure(&state, &model, FailureClass::Transient, None);
        let memory = state.route_memory.lock().unwrap();
        let health = memory.candidates.get(&model.internal_id).unwrap();
        assert_eq!(health.consecutive_failures, 2);
        let remaining = health.cooldown_until_ms.unwrap().saturating_sub(now_ms());
        assert!(remaining >= Duration::from_secs(59).as_millis() as u64);
    }

    #[tokio::test]
    async fn reaches_twentieth_candidate_after_nineteen_fast_failures() {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let attempt = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt < MAX_ROUTE_CANDIDATES {
                        StatusCode::TOO_MANY_REQUESTS.into_response()
                    } else {
                        Json(json!({
                            "id": "chatcmpl-test",
                            "model": body["model"],
                            "choices": [{"message": {"role": "assistant", "content": "ok"}}]
                        }))
                        .into_response()
                    }
                }
            }),
        );
        let addr = spawn_mock(upstream).await;
        let candidates = (0..MAX_ROUTE_CANDIDATES)
            .map(|index| candidate(addr, &format!("candidate-{index}")))
            .collect();
        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": []}),
            candidates,
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(hits.load(Ordering::SeqCst), MAX_ROUTE_CANDIDATES);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "candidate-19/candidate-19-model"
        );
    }

    #[tokio::test]
    async fn fallback_budget_stops_starting_more_candidates() {
        let slow = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                StatusCode::TOO_MANY_REQUESTS
            }),
        );
        let slow_addr = spawn_mock(slow).await;

        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_counter = second_hits.clone();
        let second = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                second_counter.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::OK }
            }),
        );
        let second_addr = spawn_mock(second).await;
        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_policy(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": []}),
            vec![
                candidate(slow_addr, "slow"),
                candidate(second_addr, "second"),
            ],
            RoutePolicy {
                max_attempts: MAX_ROUTE_CANDIDATES,
                time_budget: Duration::from_millis(30),
                header_timeout: Duration::from_secs(1),
                first_byte_timeout: Duration::from_secs(1),
            },
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(second_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn success_body_cannot_outlive_routing_budget() {
        let hanging = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from_stream(stream::pending::<
                        Result<Bytes, Infallible>,
                    >()))
                    .unwrap()
            }),
        );
        let addr = spawn_mock(hanging).await;
        let state = AppState::for_tests().unwrap();
        let routed = tokio::time::timeout(
            Duration::from_millis(500),
            route_chat_with_policy(
                &state,
                &inbound_headers(),
                json!({"model": SMART_ROUTER_MODEL_ID, "messages": []}),
                vec![candidate(addr, "hanging")],
                RoutePolicy {
                    max_attempts: 1,
                    time_budget: Duration::from_millis(30),
                    header_timeout: Duration::from_secs(1),
                    first_byte_timeout: Duration::from_secs(1),
                },
            ),
        )
        .await
        .expect("router exceeded its own budget");

        assert_eq!(routed.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn falls_back_after_rate_limit() {
        let first_hits = Arc::new(AtomicUsize::new(0));
        let first_counter = first_hits.clone();
        let first = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                first_counter.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::TOO_MANY_REQUESTS }
            }),
        );
        let first_addr = spawn_mock(first).await;

        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_counter = second_hits.clone();
        let second = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                second_counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    Json(json!({
                        "id": "chatcmpl-test",
                        "model": body["model"],
                        "choices": [{"message": {"role": "assistant", "content": "ok"}}]
                    }))
                }
            }),
        );
        let second_addr = spawn_mock(second).await;

        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": [], "stream": false}),
            vec![
                candidate(first_addr, "first"),
                candidate(second_addr, "second"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["model"], "second-model");
        assert_eq!(first_hits.load(Ordering::SeqCst), 1);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_bound_account_never_uses_its_url_or_key_and_falls_back() {
        use crate::models::model::{AccountProvider, AccountSource};
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let upstream = Router::new().route("/v1/chat/completions",post(move |Json(body):Json<Value>| {
            counter.fetch_add(1,Ordering::SeqCst);
            async move { Json(json!({"id":"fixture","model":body["model"],"choices":[{"message":{"role":"assistant","content":"ok"}}]})) }
        }));
        let addr = spawn_mock(upstream).await;
        let mut missing = candidate(addr, "missing");
        missing.api_key.clear();
        missing.account_source = Some(AccountSource {
            provider: AccountProvider::Codex,
            account_id: "invalid-fixture-id".into(),
        });
        let mut sibling = missing.clone();
        sibling.internal_id = "same-account-sibling".into();
        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_policy(
            &state,
            &inbound_headers(),
            json!({"model":"auto","messages":[{"role":"user","content":"test"}]}),
            vec![missing, sibling, candidate(addr, "working")],
            RoutePolicy {
                max_attempts: 2,
                ..RoutePolicy::default()
            },
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "working/working-model"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.route_memory.lock().unwrap().candidates["missing"].failure,
            Some(FailureClass::Authentication)
        );
        assert!(!state
            .route_memory
            .lock()
            .unwrap()
            .candidates
            .contains_key("same-account-sibling"));
    }

    #[tokio::test]
    async fn never_falls_back_after_first_stream_chunk() {
        let first = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let (tx, rx) = tokio::sync::mpsc::channel(2);
                tokio::spawn(async move {
                    tx.send(Ok::<Bytes, std::io::Error>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\n",
                    )))
                    .await
                    .unwrap();
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    let _ = tx.send(Err(std::io::Error::other("stream failed"))).await;
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(ReceiverStream::new(rx)))
                    .unwrap()
            }),
        );
        let first_addr = spawn_mock(first).await;

        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_counter = second_hits.clone();
        let second = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                second_counter.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"choices": []})) }
            }),
        );
        let second_addr = spawn_mock(second).await;

        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": [], "stream": true}),
            vec![
                candidate(first_addr, "first"),
                candidate(second_addr, "second"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "first/first-model"
        );
        let collected = response.into_body().collect().await;
        assert!(collected.is_err());
        assert_eq!(second_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn falls_back_when_success_stream_has_no_first_byte() {
        let first = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::empty())
                    .unwrap()
            }),
        );
        let first_addr = spawn_mock(first).await;
        let second = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let chunks = stream::once(async {
                    Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"second\"}}]}\n\n",
                    ))
                });
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .unwrap()
            }),
        );
        let second_addr = spawn_mock(second).await;

        let state = AppState::for_tests().unwrap();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model": SMART_ROUTER_MODEL_ID, "messages": [], "stream": true}),
            vec![
                candidate(first_addr, "first"),
                candidate(second_addr, "second"),
            ],
        )
        .await;

        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model"
        );
    }

    #[tokio::test]
    async fn anthropic_request_falls_back_and_preserves_tool_use() {
        let first = Router::new().route(
            "/v1/chat/completions",
            post(|| async { StatusCode::TOO_MANY_REQUESTS }),
        );
        let first_addr = spawn_mock(first).await;
        let second = Router::new().route(
            "/v1/chat/completions",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body["model"], "second-model");
                assert_eq!(body["tool_choice"], "required");
                Json(json!({
                    "id": "chatcmpl-tool",
                    "model": body["model"],
                    "choices": [{
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "call_weather",
                                "type": "function",
                                "function": {"name": "weather", "arguments": "{\"city\":\"Shanghai\"}"}
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }],
                    "usage": {"prompt_tokens": 12, "completion_tokens": 8}
                }))
            }),
        );
        let second_addr = spawn_mock(second).await;

        let state = AppState::for_tests().unwrap();
        let response = route_messages_with_candidates(
            &state,
            &inbound_headers(),
            json!({
                "model": "claude-opus-5",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": "weather?"}],
                "tools": [{
                    "name": "weather",
                    "description": "Get weather",
                    "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
                }],
                "tool_choice": {"type": "any"}
            }),
            vec![
                candidate(first_addr, "first"),
                candidate(second_addr, "second"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "second/second-model"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["type"], "message");
        assert_eq!(body["stop_reason"], "tool_use");
        assert_eq!(body["content"][0]["type"], "tool_use");
        assert_eq!(body["content"][0]["name"], "weather");
        assert_eq!(body["content"][0]["input"]["city"], "Shanghai");
    }

    #[tokio::test]
    async fn anthropic_stream_converts_text_and_tool_events() {
        let upstream = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let chunks = stream::iter([
                    Ok::<Bytes, Infallible>(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
                    )),
                    Ok(Bytes::from_static(
                        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"completion_tokens\":9}}\n\n",
                    )),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ]);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(chunks))
                    .unwrap()
            }),
        );
        let upstream_addr = spawn_mock(upstream).await;

        let state = AppState::for_tests().unwrap();
        let response = route_messages_with_candidates(
            &state,
            &inbound_headers(),
            json!({
                "model": "claude-opus-5",
                "max_tokens": 1024,
                "stream": true,
                "messages": [{"role": "user", "content": "read a file"}]
            }),
            vec![candidate(upstream_addr, "stream")],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("event: message_start"));
        assert!(body.contains("\"type\":\"text_delta\""));
        assert!(body.contains("\"text\":\"hello\""));
        assert!(body.contains("\"type\":\"tool_use\""));
        assert!(body.contains("\"partial_json\":\"{\\\"path\\\":\\\"a.txt\\\"}\""));
        assert!(body.contains("\"stop_reason\":\"tool_use\""));
        assert!(body.contains("event: message_stop"));
    }

    #[test]
    fn anthropic_parser_preserves_split_utf8_multiline_frames_and_tool_fragments() {
        let text = "data: {\"choices\":\r\ndata: [{\"delta\":{\"content\":\"你好🌍\"}}]}\r\n\r\n";
        let first_tool = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_utf8","function":{"name":"read_file","arguments":"{\"路径\":"}}]}}]});
        let last_tool = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"你好\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"completion_tokens":7}});
        let input = format!("{text}data: {first_tool}\r\n\r\ndata: {last_tool}\r\n\r\n");
        let mut adapter = AnthropicSseAdapter::new();
        let mut output = Vec::new();
        for byte in input.as_bytes() {
            for chunk in adapter.push(std::slice::from_ref(byte)).unwrap() {
                output.extend_from_slice(&chunk);
            }
        }
        for chunk in adapter.finish().unwrap() {
            output.extend_from_slice(&chunk);
        }
        let mut parser = crate::services::account_proxy::SseParser::default();
        let frames: Vec<Value> = parser
            .push(&output)
            .into_iter()
            .map(|(_, data)| serde_json::from_str(&data).unwrap())
            .collect();
        assert!(frames
            .iter()
            .any(|frame| frame["delta"]["text"] == "你好🌍"));
        let tool = frames
            .iter()
            .find(|frame| frame["content_block"]["type"] == "tool_use")
            .unwrap();
        assert_eq!(tool["content_block"]["id"], "call_utf8");
        assert_eq!(tool["content_block"]["name"], "read_file");
        assert!(frames
            .iter()
            .any(|frame| frame["delta"]["partial_json"] == "{\"路径\":\"你好\"}"));
        assert!(frames
            .iter()
            .any(|frame| frame["delta"]["stop_reason"] == "tool_use"
                && frame["usage"]["output_tokens"] == 7));
        assert_eq!(frames.last().unwrap()["type"], "message_stop");
    }

    #[test]
    fn anthropic_parser_accepts_done_without_finish_reason() {
        let mut adapter = AnthropicSseAdapter::new();
        let events = adapter.push(b"data: [DONE]\n\n").unwrap();
        assert!(events
            .iter()
            .any(|event| event.starts_with(b"event: message_stop\n")));
        assert!(adapter.finish().unwrap().is_empty());
    }

    #[tokio::test]
    async fn anthropic_stream_rejects_errors_invalid_json_and_unfinished_eof() {
        let prefix = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n";
        for tail in [
            "",
            "data: {\"error\":{\"message\":\"upstream failed\"}}\n\n",
            "event: error\ndata: {\"message\":\"upstream failed\"}\n\n",
            "data: {invalid json}\n\n",
        ] {
            let response =
                openai_stream_to_anthropic(Response::new(Body::from(format!("{prefix}{tail}"))));
            let mut output = response.into_body().into_data_stream();
            let mut bytes = Vec::new();
            let mut failed = false;
            while let Some(item) = output.next().await {
                match item {
                    Ok(chunk) => bytes.extend_from_slice(&chunk),
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
            let text = String::from_utf8(bytes).unwrap();
            assert!(failed, "unfinished input must fail: {tail}");
            assert!(text.contains("event: message_start"));
            assert!(!text.contains("event: message_stop"));
            assert!(!text.contains("\"stop_reason\":\"end_turn\""));
        }
    }

    #[tokio::test]
    async fn dropping_anthropic_output_releases_pending_source_and_activity() {
        struct PendingSource {
            entered: Arc<tokio::sync::Notify>,
            dropped: Arc<tokio::sync::Notify>,
            drop_count: Arc<AtomicUsize>,
            activity_guard: Option<RouteActivityGuard>,
        }
        impl futures_util::Stream for PendingSource {
            type Item = Result<Bytes, std::io::Error>;

            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                self.entered.notify_one();
                std::task::Poll::Pending
            }
        }
        impl Drop for PendingSource {
            fn drop(&mut self) {
                self.activity_guard.take();
                self.drop_count.fetch_add(1, Ordering::SeqCst);
                self.dropped.notify_one();
            }
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(tokio::sync::Notify::new());
        let drop_count = Arc::new(AtomicUsize::new(0));
        let activity_guard = RouteActivityGuard::begin("pending-anthropic-fixture");
        let attempt_id = activity_guard.attempt_id.unwrap();
        let response =
            openai_stream_to_anthropic(Response::new(Body::from_stream(PendingSource {
                entered: entered.clone(),
                dropped: dropped.clone(),
                drop_count: drop_count.clone(),
                activity_guard: Some(activity_guard),
            })));
        let mut output = response.into_body().into_data_stream();
        assert!(output
            .next()
            .await
            .unwrap()
            .unwrap()
            .starts_with(b"event: message_start\n"));
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .unwrap();
        assert_eq!(drop_count.load(Ordering::SeqCst), 0);
        assert!(ROUTE_ACTIVITY
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .active_attempts
            .contains_key(&attempt_id));
        drop(output);
        tokio::time::timeout(Duration::from_secs(5), dropped.notified())
            .await
            .unwrap();
        assert_eq!(drop_count.load(Ordering::SeqCst), 1);
        assert!(!ROUTE_ACTIVITY
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .active_attempts
            .contains_key(&attempt_id));
    }

    fn bound_health_candidate(id: &str) -> Candidate {
        let mut model = candidate(SocketAddr::from(([127, 0, 0, 1], 1)), id);
        model.account_source = Some(crate::models::model::AccountSource {
            provider: crate::models::model::AccountProvider::Kilo,
            account_id: "health-fixture-account".into(),
        });
        model
    }

    #[tokio::test]
    async fn partial_stream_failures_accumulate_until_real_completion_without_replay() {
        let complete = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let upstream_complete = complete.clone();
        let first_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let complete = upstream_complete.clone();
                async move {
                    let mut data =
                        "data: {\"choices\":[{\"delta\":{\"content\":\"中文\"}}]}\n\n".to_string();
                    if complete.load(Ordering::SeqCst) {
                        data.push_str("data: [DONE]\n\n");
                    }
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .body(Body::from(data))
                        .unwrap()
                }
            }),
        ))
        .await;
        let backup_hits = Arc::new(AtomicUsize::new(0));
        let counter = backup_hits.clone();
        let backup_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"choices":[]})) }
            }),
        ))
        .await;
        let state = AppState::for_tests().unwrap();
        for expected_failures in 1..=3 {
            let started = now_ms();
            let response = route_chat_with_candidates(
                &state,
                &inbound_headers(),
                json!({"messages":[],"stream":true}),
                vec![
                    candidate(first_addr, "partial-stream"),
                    candidate(backup_addr, "partial-backup"),
                ],
            )
            .await;
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "partial-stream/partial-stream-model"
            );
            let mut stream = response.into_body().into_data_stream();
            let first = stream.next().await.unwrap().unwrap();
            assert!(String::from_utf8_lossy(&first).contains("中文"));
            assert!(stream.next().await.unwrap().is_err());
            let mut memory = state.route_memory.lock().unwrap();
            let health = memory.candidates.get_mut("partial-stream").unwrap();
            assert_eq!(health.consecutive_failures, expected_failures);
            let cooldown = cooldown_for_failure(FailureClass::Transient, None, expected_failures);
            assert!(health.cooldown_until_ms.unwrap() >= started + cooldown.as_millis() as u64);
            health.cooldown_until_ms = Some(started.saturating_sub(1));
        }
        complete.store(true, Ordering::SeqCst);
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"messages":[],"stream":true}),
            vec![candidate(first_addr, "partial-stream")],
        )
        .await;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("[DONE]"));
        assert!(!state
            .route_memory
            .lock()
            .unwrap()
            .candidates
            .contains_key("partial-stream"));
        assert_eq!(backup_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn raw_stream_error_preserves_http_retry_after_without_replaying_output() {
        let release_error = Arc::new(tokio::sync::Notify::new());
        let release = release_error.clone();
        let first_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let release = release.clone();
                async move {
                    let first = stream::once(async {
                        Ok::<_, std::io::Error>(Bytes::from_static(
                            b"data: {\"choices\":[{\"delta\":{\"content\":\"ready\"}}]}\n\n",
                        ))
                    });
                    let error = stream::once(async move {
                        release.notified().await;
                        Ok::<_, std::io::Error>(Bytes::from_static(
                            b"event: error\ndata: {\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit\"}}\n\n",
                        ))
                    });
                    Response::builder()
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .header(header::RETRY_AFTER, "120")
                        .body(Body::from_stream(first.chain(error)))
                        .unwrap()
                }
            }),
        ))
        .await;
        let backup_hits = Arc::new(AtomicUsize::new(0));
        let counter = backup_hits.clone();
        let backup_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { Json(json!({"choices":[]})) }
            }),
        ))
        .await;
        let state = AppState::for_tests().unwrap();
        let started = now_ms();
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"messages":[],"stream":true}),
            vec![
                candidate(first_addr, "raw-stream-limit"),
                candidate(backup_addr, "raw-limit-backup"),
            ],
        )
        .await;
        let mut body = response.into_body().into_data_stream();
        let first = body.next().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&first).contains("ready"));
        release_error.notify_one();
        assert!(tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(body.next().await.is_none());
        let memory = state.route_memory.lock().unwrap();
        let health = &memory.candidates["raw-stream-limit"];
        assert_eq!(health.failure, Some(FailureClass::RateLimit));
        assert!(health.cooldown_until_ms.unwrap() >= started + 120_000);
        assert_eq!(backup_hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn structured_body_failure_preserves_cooldown_and_explicit_model_scope() {
        let state = AppState::for_tests().unwrap();
        let model = bound_health_candidate("stream-quota");
        let mut error = crate::services::account_proxy::ProxyError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit",
        );
        error.error_code = Some("rate_limit_exceeded".into());
        error.retry_after = Some(HeaderValue::from_static("3600"));
        error.model_scope = true;
        let error = std::io::Error::other(std::io::Error::other(error));
        let started = now_ms();
        assert_eq!(
            mark_stream_failure(&state, &model, &error),
            Some(FailureClass::RateLimit)
        );
        let memory = state.route_memory.lock().unwrap();
        assert!(memory.accounts.is_empty());
        assert!(
            memory.candidates["stream-quota"].cooldown_until_ms.unwrap() >= started + 3_600_000
        );
    }

    #[tokio::test]
    async fn preparation_metadata_stream_errors_invalidate_only_known_account_failures() {
        use crate::models::model::AccountSource;

        for (provider, status, code, invalidates, expected_failure) in [
            (
                AccountProvider::Copilot,
                StatusCode::NOT_FOUND,
                Some("model_not_found"),
                true,
                Some(FailureClass::ModelUnavailable),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::BAD_REQUEST,
                Some("model_not_supported"),
                true,
                Some(FailureClass::ModelUnavailable),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::UNAUTHORIZED,
                None,
                true,
                Some(FailureClass::Authentication),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::FORBIDDEN,
                None,
                true,
                Some(FailureClass::Authentication),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::NOT_FOUND,
                None,
                false,
                Some(FailureClass::ModelUnavailable),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::BAD_REQUEST,
                Some("tools_not_supported"),
                false,
                Some(FailureClass::RequestIncompatible),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::PAYMENT_REQUIRED,
                Some("insufficient_quota"),
                false,
                Some(FailureClass::Billing),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::TOO_MANY_REQUESTS,
                Some("rate_limit_exceeded"),
                false,
                Some(FailureClass::RateLimit),
            ),
            (
                AccountProvider::Copilot,
                StatusCode::BAD_REQUEST,
                Some("context_length_exceeded"),
                false,
                None,
            ),
            (
                AccountProvider::Kilo,
                StatusCode::NOT_FOUND,
                Some("model_not_found"),
                false,
                Some(FailureClass::ModelUnavailable),
            ),
            (
                AccountProvider::Kilo,
                StatusCode::UNAUTHORIZED,
                None,
                false,
                Some(FailureClass::Authentication),
            ),
        ] {
            let catalogue_hits = Arc::new(AtomicUsize::new(0));
            let inference_hits = Arc::new(AtomicUsize::new(0));
            let catalogue_counter = catalogue_hits.clone();
            let inference_counter = inference_hits.clone();
            let addr = spawn_mock(
                Router::new()
                    .route("/v1/models", get(move || {
                        catalogue_counter.fetch_add(1, Ordering::SeqCst);
                        async { Json(json!({"data":[{"id":"fixture-model", "supported_endpoints":["/chat/completions"]}]})) }
                    }))
                    .route("/v1/chat/completions", post(move || {
                        inference_counter.fetch_add(1, Ordering::SeqCst);
                        async {
                            Response::builder()
                                .header(header::CONTENT_TYPE, "text/event-stream")
                                .body(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"fixture-answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                                .unwrap()
                        }
                    })),
            ).await;
            let account_id = uuid::Uuid::new_v4().to_string();
            let credentials = json!({
                "accessToken":"fixture-token", "baseUrl":format!("http://{addr}/v1"),
                "_echobird_saved_account_id":account_id,
            });
            let client = reqwest::Client::new();
            assert_eq!(
                crate::services::account_proxy::list_models(
                    AccountProvider::Copilot,
                    credentials.clone(),
                    &client,
                )
                .await
                .unwrap(),
                ["fixture-model"],
            );
            let route = || {
                crate::services::account_proxy::route(
                    AccountProvider::Copilot,
                    credentials.clone(),
                    "fixture-model",
                    json!({"messages":[], "stream":false}),
                    &client,
                )
            };
            let response = route().await.unwrap();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            assert!(String::from_utf8_lossy(&body).contains("fixture-answer"));
            assert_eq!(catalogue_hits.load(Ordering::SeqCst), 1);
            assert_eq!(inference_hits.load(Ordering::SeqCst), 1);

            let mut model = candidate(addr, "metadata-stream");
            model.account_source = Some(AccountSource {
                provider,
                account_id,
            });
            let mut error =
                crate::services::account_proxy::ProxyError::new(status, "Fixture upstream failure");
            error.error_code = code.map(str::to_owned);
            error.model_scope = matches!(
                code,
                Some("model_not_found" | "model_not_supported" | "tools_not_supported")
            );
            error.retry_after =
                (status == StatusCode::TOO_MANY_REQUESTS).then(|| HeaderValue::from_static("120"));
            let error = std::io::Error::other(std::io::Error::other(error));
            let state = AppState::for_tests().unwrap();
            assert_eq!(
                mark_stream_failure(&state, &model, &error),
                expected_failure
            );
            assert_eq!(catalogue_hits.load(Ordering::SeqCst), 1);
            assert_eq!(inference_hits.load(Ordering::SeqCst), 1);
            {
                let memory = state.route_memory.lock().unwrap();
                let records_failure = expected_failure
                    .is_some_and(|failure| failure != FailureClass::RequestIncompatible);
                assert_eq!(
                    memory.candidates.contains_key(&model.internal_id),
                    records_failure
                );
                let records_account = matches!(
                    expected_failure,
                    Some(
                        FailureClass::Authentication
                            | FailureClass::Billing
                            | FailureClass::RateLimit
                    )
                );
                assert_eq!(
                    memory
                        .accounts
                        .contains_key(&account_health_key(&model).unwrap()),
                    records_account
                );
                if records_failure {
                    assert_eq!(
                        memory.candidates[&model.internal_id].failure,
                        expected_failure
                    );
                }
            }

            let response = route().await.unwrap();
            let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap();
            assert!(String::from_utf8_lossy(&body).contains("fixture-answer"));
            assert_eq!(
                catalogue_hits.load(Ordering::SeqCst),
                1 + usize::from(invalidates),
                "{provider:?} {status} {code:?}"
            );
            assert_eq!(inference_hits.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn incompatible_tools_request_does_not_cool_down_plain_text_model() {
        let first_hits = Arc::new(AtomicUsize::new(0));
        let first_counter = first_hits.clone();
        let first_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let counter = first_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if body["tools"].as_array().is_some_and(|tools| !tools.is_empty()) {
                        return openai_error(StatusCode::BAD_REQUEST, "tools are not supported");
                    }
                    Json(json!({"choices":[{"message":{"role":"assistant","content":"plain text"},"finish_reason":"stop"}]})).into_response()
                }
            }),
        )).await;
        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_counter = second_hits.clone();
        let second_addr = spawn_mock(Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let counter = second_counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"choices":[{"message":{"role":"assistant","content":"tool capable"},"finish_reason":"stop"}]}))
                }
            }),
        )).await;
        let state = AppState::for_tests().unwrap();
        let models = vec![
            candidate(first_addr, "plain-first"),
            candidate(second_addr, "tools-second"),
        ];
        let with_tools = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model":SMART_ROUTER_MODEL_ID,"messages":[],"stream":false,
                "tools":[{"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}}]}),
            models.clone(),
        ).await;
        assert_eq!(with_tools.status(), StatusCode::OK);
        assert_eq!(
            with_tools.headers()["X-EchoBird-Routed-Via"],
            "tools-second/tools-second-model"
        );
        with_tools.into_body().collect().await.unwrap();
        assert!(!state
            .route_memory
            .lock()
            .unwrap()
            .candidates
            .contains_key("plain-first"));
        let plain = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model":SMART_ROUTER_MODEL_ID,"messages":[],"stream":false}),
            models,
        )
        .await;
        assert_eq!(plain.status(), StatusCode::OK);
        assert_eq!(
            plain.headers()["X-EchoBird-Routed-Via"],
            "plain-first/plain-first-model"
        );
        plain.into_body().collect().await.unwrap();
        assert_eq!(first_hits.load(Ordering::SeqCst), 2);
        assert_eq!(second_hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn old_success_cannot_clear_a_newer_rate_limit_revision() {
        let state = AppState::for_tests().unwrap();
        let mut old = bound_health_candidate("concurrent");
        begin_attempt(&state, &mut old);
        bind_attempt_credentials(&state, &old, &json!({"accessToken":"T0"}));
        let mut rejected = old.clone();
        begin_attempt(&state, &mut rejected);
        bind_attempt_credentials(&state, &rejected, &json!({"accessToken":"T0"}));
        mark_failure(
            &state,
            &rejected,
            FailureClass::RateLimit,
            Some(Duration::from_secs(600)),
        );
        let key = account_health_key(&old).unwrap();
        mark_success(&state, &old);
        {
            let mut memory = state.route_memory.lock().unwrap();
            assert_eq!(
                memory.candidates["concurrent"].failure,
                Some(FailureClass::RateLimit)
            );
            assert_eq!(memory.accounts[&key].failure, Some(FailureClass::RateLimit));
            memory
                .candidates
                .get_mut("concurrent")
                .unwrap()
                .cooldown_until_ms = Some(now_ms().saturating_sub(1));
            memory.accounts.get_mut(&key).unwrap().cooldown_until_ms =
                Some(now_ms().saturating_sub(1));
        }
        mark_success(&state, &old);
        assert!(state
            .route_memory
            .lock()
            .unwrap()
            .accounts
            .contains_key(&key));
        let mut fresh = old.clone();
        begin_attempt(&state, &mut fresh);
        bind_attempt_credentials(&state, &fresh, &json!({"accessToken":"T0"}));
        mark_success(&state, &fresh);
        let memory = state.route_memory.lock().unwrap();
        assert!(!memory.candidates.contains_key("concurrent"));
        assert!(!memory.accounts.contains_key(&key));
    }

    #[tokio::test]
    async fn unsupported_parameters_try_the_backup_without_cooling_down_the_account() {
        for streaming in [false, true] {
            let mut error = crate::services::account_proxy::ProxyError::new(
                StatusCode::BAD_REQUEST,
                "The selected model does not advertise this output limit",
            );
            error.error_code = Some("unsupported_parameter".into());
            error.model_scope = true;
            let backup = if streaming {
                Response::new(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"backup\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
            } else {
                crate::services::account_proxy::json_response(
                    &json!({"choices":[{"message":{"role":"assistant","content":"backup"},"finish_reason":"stop"}]}),
                )
            };
            let responses = Arc::new(Mutex::new(HashMap::from([
                ("unsupported-limit".to_string(), error.into_response()),
                ("limit-backup".to_string(), backup),
            ])));
            let mut state = AppState::for_tests().unwrap();
            state.fixture_responses = Some(responses.clone());
            let candidates = vec![
                bound_health_candidate("unsupported-limit"),
                bound_health_candidate("limit-backup"),
            ];
            let response = route_chat_with_candidates(
                &state,
                &inbound_headers(),
                json!({"messages":[],"max_tokens":1,"stream":streaming}),
                candidates.clone(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "limit-backup/limit-backup-model"
            );
            response.into_body().collect().await.unwrap();
            assert!(responses.lock().unwrap().is_empty());
            {
                let memory = state.route_memory.lock().unwrap();
                assert!(memory.candidates.is_empty());
                assert!(memory.accounts.is_empty());
            }
            responses.lock().unwrap().insert("unsupported-limit".into(), crate::services::account_proxy::json_response(&json!({"choices":[{"message":{"role":"assistant","content":"plain text"},"finish_reason":"stop"}]})));
            let response = route_chat_with_candidates(
                &state,
                &inbound_headers(),
                json!({"messages":[],"stream":false}),
                candidates,
            )
            .await;
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "unsupported-limit/unsupported-limit-model"
            );
            response.into_body().collect().await.unwrap();
        }
    }

    #[test]
    fn short_rate_limit_cannot_shorten_existing_candidate_or_account_deadline() {
        let state = AppState::for_tests().unwrap();
        let mut model = bound_health_candidate("long-rate-limit");
        begin_attempt(&state, &mut model);
        bind_attempt_credentials(&state, &model, &json!({"accessToken":"T0"}));
        mark_failure(
            &state,
            &model,
            FailureClass::RateLimit,
            Some(Duration::from_secs(600)),
        );
        let key = account_health_key(&model).unwrap();
        let long_deadline =
            state.route_memory.lock().unwrap().candidates["long-rate-limit"].cooldown_until_ms;
        begin_attempt(&state, &mut model);
        bind_attempt_credentials(&state, &model, &json!({"accessToken":"T0"}));
        mark_failure(
            &state,
            &model,
            FailureClass::RateLimit,
            Some(Duration::from_secs(60)),
        );
        let memory = state.route_memory.lock().unwrap();
        assert_eq!(
            memory.candidates["long-rate-limit"].cooldown_until_ms,
            long_deadline
        );
        assert_eq!(memory.accounts[&key].cooldown_until_ms, long_deadline);
        assert_eq!(memory.candidates["long-rate-limit"].consecutive_failures, 2);
    }

    #[test]
    fn another_model_success_preserves_active_account_billing_and_rate_limit() {
        for failure in [FailureClass::Billing, FailureClass::RateLimit] {
            let state = AppState::for_tests().unwrap();
            let mut failed = bound_health_candidate("account-failed-model");
            begin_attempt(&state, &mut failed);
            bind_attempt_credentials(&state, &failed, &json!({"accessToken":"T0"}));
            mark_failure(&state, &failed, failure, Some(Duration::from_secs(600)));
            let key = account_health_key(&failed).unwrap();
            let mut sibling = bound_health_candidate("account-success-model");
            begin_attempt(&state, &mut sibling);
            bind_attempt_credentials(&state, &sibling, &json!({"accessToken":"T0"}));
            mark_success(&state, &sibling);
            assert!(candidate_is_cooling_down(&state, &sibling));
            let mut memory = state.route_memory.lock().unwrap();
            assert!(!clear_account_authentication(
                &mut memory,
                &key,
                &HashSet::new()
            ));
            assert_eq!(memory.accounts[&key].failure, Some(failure));
            assert_eq!(
                memory.candidates["account-failed-model"].failure,
                Some(failure)
            );
        }
    }

    #[test]
    fn old_unauthorized_response_is_ignored_after_credentials_rotate() {
        let state = AppState::for_tests().unwrap();
        let mut old = bound_health_candidate("rotated-token");
        begin_attempt(&state, &mut old);
        bind_attempt_credentials(&state, &old, &json!({"accessToken":"T0"}));
        let mut fresh = old.clone();
        begin_attempt(&state, &mut fresh);
        bind_attempt_credentials(&state, &fresh, &json!({"accessToken":"T1"}));
        mark_failure(&state, &old, FailureClass::Authentication, None);
        {
            let memory = state.route_memory.lock().unwrap();
            assert!(memory.candidates.is_empty());
            assert!(memory.accounts.is_empty());
        }
        mark_failure(&state, &fresh, FailureClass::Authentication, None);
        let key = account_health_key(&fresh).unwrap();
        let memory = state.route_memory.lock().unwrap();
        assert_eq!(
            memory.candidates["rotated-token"].failure,
            Some(FailureClass::Authentication)
        );
        assert_eq!(
            memory.accounts[&key].failure,
            Some(FailureClass::Authentication)
        );
    }

    #[test]
    fn late_first_credential_bind_cannot_restore_rejected_token_version() {
        let state = AppState::for_tests().unwrap();
        let mut old = bound_health_candidate("late-token-bind");
        begin_attempt(&state, &mut old);
        let mut fresh = old.clone();
        begin_attempt(&state, &mut fresh);
        bind_attempt_credentials(&state, &fresh, &json!({"accessToken":"T1"}));
        let key = account_health_key(&fresh).unwrap();
        let current_version = state.route_memory.lock().unwrap().credential_versions[&key].clone();
        bind_attempt_credentials(&state, &old, &json!({"accessToken":"T0"}));
        mark_failure(&state, &old, FailureClass::Authentication, None);
        {
            let memory = state.route_memory.lock().unwrap();
            assert_eq!(memory.credential_versions[&key], current_version);
            assert!(memory.candidates.is_empty());
            assert!(memory.accounts.is_empty());
        }
        mark_failure(&state, &fresh, FailureClass::Authentication, None);
        let memory = state.route_memory.lock().unwrap();
        assert_eq!(memory.candidates["late-token-bind"].consecutive_failures, 1);
        assert_eq!(
            memory.accounts[&key].failure,
            Some(FailureClass::Authentication)
        );
    }

    #[test]
    fn reauthentication_only_clears_authentication_and_rejects_old_epoch_results() {
        let state = AppState::for_tests().unwrap();
        let mut auth = bound_health_candidate("reauthentication-auth");
        begin_attempt(&state, &mut auth);
        bind_attempt_credentials(&state, &auth, &json!({"accessToken":"T0"}));
        mark_failure(&state, &auth, FailureClass::Authentication, None);
        let billing = bound_health_candidate("reauthentication-billing");
        let limited = bound_health_candidate("reauthentication-limited");
        mark_failure_scoped(&state, &billing, FailureClass::Billing, None, true);
        mark_failure_scoped(
            &state,
            &limited,
            FailureClass::RateLimit,
            Some(Duration::from_secs(600)),
            true,
        );
        let key = account_health_key(&auth).unwrap();
        {
            let mut memory = state.route_memory.lock().unwrap();
            assert!(clear_account_authentication(
                &mut memory,
                &key,
                &HashSet::new()
            ));
            assert!(!memory.candidates.contains_key("reauthentication-auth"));
            assert!(!memory.accounts.contains_key(&key));
            assert_eq!(
                memory.candidates["reauthentication-billing"].failure,
                Some(FailureClass::Billing)
            );
            assert_eq!(
                memory.candidates["reauthentication-limited"].failure,
                Some(FailureClass::RateLimit)
            );
            let epoch = memory.account_epochs.entry(key.clone()).or_default();
            *epoch = epoch.saturating_add(1);
            memory.credential_versions.remove(&key);
            memory.revision = memory.revision.saturating_add(1);
        }
        mark_failure(&state, &auth, FailureClass::Authentication, None);
        mark_success(&state, &auth);
        {
            let memory = state.route_memory.lock().unwrap();
            assert!(!memory.candidates.contains_key("reauthentication-auth"));
            assert!(!memory.accounts.contains_key(&key));
            assert!(memory.candidates.contains_key("reauthentication-billing"));
            assert!(memory.candidates.contains_key("reauthentication-limited"));
        }
        let mut fresh = auth.clone();
        begin_attempt(&state, &mut fresh);
        bind_attempt_credentials(&state, &fresh, &json!({"accessToken":"T1"}));
        mark_failure(&state, &fresh, FailureClass::Authentication, None);
        assert!(state
            .route_memory
            .lock()
            .unwrap()
            .candidates
            .contains_key("reauthentication-auth"));
    }

    #[test]
    fn model_local_long_cooldown_is_not_promoted_to_account_authentication() {
        for (failure, retry_after, model_scope) in [
            (FailureClass::ModelUnavailable, None, false),
            (
                FailureClass::RateLimit,
                Some(Duration::from_secs(24 * 60 * 60)),
                true,
            ),
        ] {
            let state = AppState::for_tests().unwrap();
            let mut local = bound_health_candidate("local-long-cooldown");
            let mut authentication = local.clone();
            begin_attempt(&state, &mut local);
            begin_attempt(&state, &mut authentication);
            bind_attempt_credentials(&state, &local, &json!({"accessToken":"T0"}));
            bind_attempt_credentials(&state, &authentication, &json!({"accessToken":"T0"}));
            mark_failure_scoped(&state, &local, failure, retry_after, model_scope);
            let key = account_health_key(&local).unwrap();
            let long_deadline = state.route_memory.lock().unwrap().candidates
                ["local-long-cooldown"]
                .cooldown_until_ms;
            assert!(state.route_memory.lock().unwrap().accounts.is_empty());
            let started = now_ms();
            mark_failure(&state, &authentication, FailureClass::Authentication, None);
            let finished = now_ms();
            let mut memory = state.route_memory.lock().unwrap();
            assert_eq!(
                memory.candidates["local-long-cooldown"].failure,
                Some(failure)
            );
            assert_eq!(
                memory.candidates["local-long-cooldown"].cooldown_until_ms,
                long_deadline
            );
            let account = memory.accounts.get_mut(&key).unwrap();
            assert_eq!(account.failure, Some(FailureClass::Authentication));
            let auth_deadline = account.cooldown_until_ms.unwrap();
            assert!(auth_deadline >= started + AUTH_OR_BILLING_COOLDOWN.as_millis() as u64);
            assert!(auth_deadline <= finished + AUTH_OR_BILLING_COOLDOWN.as_millis() as u64);
            assert!(Some(auth_deadline) < long_deadline);
            account.cooldown_until_ms = Some(started.saturating_sub(1));
            drop(memory);
            assert!(!candidate_is_cooling_down(
                &state,
                &bound_health_candidate("local-sibling")
            ));
            assert!(candidate_is_cooling_down(&state, &local));
        }
    }

    fn typed_body_failure(error: crate::services::account_proxy::ProxyError) -> Response {
        Response::new(Body::from_stream(stream::once(async move {
            Err::<Bytes, std::io::Error>(std::io::Error::other(std::io::Error::other(error)))
        })))
    }

    #[tokio::test]
    async fn early_typed_request_rejection_does_not_cool_down_or_call_backup() {
        for streaming in [false, true] {
            for (status, message) in [
                (StatusCode::FORBIDDEN, "blocked by safety policy"),
                (StatusCode::BAD_REQUEST, "invalid input"),
            ] {
                let mut error = crate::services::account_proxy::ProxyError::new(status, message);
                error.error_code = Some("request_rejected".into());
                let responses = Arc::new(Mutex::new(HashMap::from([
                    ("typed-rejection".to_string(), typed_body_failure(error)),
                    (
                        "rejection-backup".to_string(),
                        Response::new(Body::from("unused")),
                    ),
                ])));
                let mut state = AppState::for_tests().unwrap();
                state.fixture_responses = Some(responses.clone());
                let response = route_chat_with_candidates(
                    &state,
                    &inbound_headers(),
                    json!({"model":SMART_ROUTER_MODEL_ID,"messages":[],"stream":streaming}),
                    vec![
                        bound_health_candidate("typed-rejection"),
                        bound_health_candidate("rejection-backup"),
                    ],
                )
                .await;
                assert_eq!(response.status(), status);
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let error: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(error["error"]["message"], message);
                assert_eq!(error["error"]["code"], "request_rejected");
                let memory = state.route_memory.lock().unwrap();
                assert!(memory.candidates.is_empty());
                assert!(memory.accounts.is_empty());
                let responses = responses.lock().unwrap();
                assert!(!responses.contains_key("typed-rejection"));
                assert!(responses.contains_key("rejection-backup"));
            }
        }
    }

    #[tokio::test]
    async fn first_typed_body_rate_limit_preserves_retry_after_scope_and_calls_backup() {
        for model_scope in [false, true] {
            let mut error = crate::services::account_proxy::ProxyError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit",
            );
            error.error_code = Some("rate_limit_exceeded".into());
            error.retry_after = Some(HeaderValue::from_static("3600"));
            error.model_scope = model_scope;
            let responses = Arc::new(Mutex::new(HashMap::from([
                ("typed-rate-limit".to_string(), typed_body_failure(error)),
                ("rate-backup".to_string(), Response::new(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"backup\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))),
            ])));
            let mut state = AppState::for_tests().unwrap();
            state.fixture_responses = Some(responses.clone());
            let limited = bound_health_candidate("typed-rate-limit");
            let key = account_health_key(&limited).unwrap();
            let backup = candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "rate-backup");
            let started = now_ms();
            let response = route_chat_with_candidates(
                &state,
                &inbound_headers(),
                json!({"model":SMART_ROUTER_MODEL_ID,"messages":[],"stream":true}),
                vec![limited, backup],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "rate-backup/rate-backup-model"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert!(String::from_utf8_lossy(&bytes).contains("[DONE]"));
            assert!(responses.lock().unwrap().is_empty());
            let memory = state.route_memory.lock().unwrap();
            let limited = &memory.candidates["typed-rate-limit"];
            assert_eq!(limited.failure, Some(FailureClass::RateLimit));
            assert!(limited.cooldown_until_ms.unwrap() >= started + 3_600_000);
            if model_scope {
                assert!(memory.accounts.is_empty());
            } else {
                assert_eq!(memory.accounts[&key].failure, Some(FailureClass::RateLimit));
                assert_eq!(
                    memory.accounts[&key].cooldown_until_ms,
                    limited.cooldown_until_ms
                );
            }
        }
    }

    fn authentication_fixture(initial: Response, retried: Response) -> AppState {
        let mut initial = initial;
        initial
            .extensions_mut()
            .insert(RejectedAccountCredentials(json!({"accessToken":"T0"})));
        let mut state = AppState::for_tests().unwrap();
        state.fixture_responses = Some(Arc::new(Mutex::new(HashMap::from([
            ("refresh-account".into(), initial),
            ("refresh-backup".into(), Response::new(Body::from("backup"))),
        ]))));
        state.fixture_authentication = Some(Arc::new(Mutex::new(AuthenticationFixture {
            result: Ok(json!({"accessToken":"T1"})),
            delay: Duration::ZERO,
            rejections: Vec::new(),
            retry_responses: std::collections::VecDeque::from([retried]),
            sent_credentials: Vec::new(),
        })));
        state
    }

    fn unauthorized_body() -> Response {
        typed_body_failure(crate::services::account_proxy::ProxyError::new(
            StatusCode::UNAUTHORIZED,
            "Expired account credential",
        ))
    }

    fn successful_chat_fixture(streaming: bool) -> Response {
        if streaming {
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"backup\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                .unwrap()
        } else {
            crate::services::account_proxy::json_response(
                &json!({"choices":[{"message":{"role":"assistant","content":"backup"},"finish_reason":"stop"}]}),
            )
        }
    }

    #[tokio::test]
    async fn early_authentication_failure_refreshes_once_before_any_output() {
        for protocol in [RouteProtocol::Chat, RouteProtocol::Responses] {
            for streaming in [false, true] {
                for http_unauthorized in [false, true] {
                    let initial = if http_unauthorized {
                        crate::services::account_proxy::ProxyError::new(
                            StatusCode::UNAUTHORIZED,
                            "Expired account credential",
                        )
                        .into_response()
                    } else {
                        unauthorized_body()
                    };
                    let successful = if protocol == RouteProtocol::Responses {
                        if streaming {
                            Response::new(Body::from("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-fresh\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"fresh output\"}]}]}}\n\n"))
                        } else {
                            Response::new(Body::from("{\"id\":\"resp-fresh\",\"object\":\"response\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"fresh output\"}]}]}"))
                        }
                    } else if streaming {
                        Response::new(Body::from("data: {\"choices\":[{\"delta\":{\"content\":\"fresh output\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
                    } else {
                        Response::new(Body::from("fresh output"))
                    };
                    let state = authentication_fixture(initial, successful);
                    let account = bound_health_candidate("refresh-account");
                    let source = account.account_source.clone().unwrap();
                    let response = route_with_policy(
                        &state,
                        &HeaderMap::new(),
                        json!({"stream":streaming,"input":"hello","messages":[]}),
                        vec![
                            account,
                            candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "refresh-backup"),
                        ],
                        RoutePolicy::default(),
                        protocol,
                    )
                    .await;
                    assert_eq!(response.status(), StatusCode::OK);
                    assert_eq!(
                        response.headers()["X-EchoBird-Routed-Via"],
                        "refresh-account/refresh-account-model"
                    );
                    response.into_body().collect().await.unwrap();
                    let fixture = state
                        .fixture_authentication
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap();
                    assert_eq!(
                        fixture.rejections,
                        vec![(source, json!({"accessToken":"T0"}))]
                    );
                    assert_eq!(fixture.sent_credentials, vec![json!({"accessToken":"T1"})]);
                    assert!(fixture.retry_responses.is_empty());
                    assert!(state
                        .fixture_responses
                        .as_ref()
                        .unwrap()
                        .lock()
                        .unwrap()
                        .contains_key("refresh-backup"));
                    let memory = state.route_memory.lock().unwrap();
                    assert!(memory.candidates.is_empty());
                    assert!(memory.accounts.is_empty());
                }
            }
        }
    }

    #[tokio::test]
    async fn repeated_early_unauthorized_does_not_refresh_again() {
        for streaming in [false, true] {
            let state = authentication_fixture(unauthorized_body(), unauthorized_body());
            state
                .fixture_responses
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .insert("refresh-backup".into(), successful_chat_fixture(streaming));
            let mut backup = candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "refresh-backup");
            // The backup has independent credentials and must remain eligible.
            backup.account_source = None;
            let response = route_chat_with_candidates(
                &state,
                &HeaderMap::new(),
                json!({"stream":streaming,"messages":[]}),
                vec![bound_health_candidate("refresh-account"), backup],
            )
            .await;
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "refresh-backup/refresh-backup-model"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert!(String::from_utf8_lossy(&bytes).contains("backup"));
            assert_eq!(
                state
                    .fixture_authentication
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .rejections
                    .len(),
                1
            );
            let memory = state.route_memory.lock().unwrap();
            assert_eq!(
                memory.candidates["refresh-account"].failure,
                Some(FailureClass::Authentication)
            );
            assert_eq!(memory.accounts.len(), 1);
        }
    }

    #[tokio::test]
    async fn authentication_failure_after_delivered_output_is_never_replayed() {
        let partial = Response::new(Body::from_stream(stream::iter([
            Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            )),
            Err(std::io::Error::other(
                crate::services::account_proxy::ProxyError::new(
                    StatusCode::UNAUTHORIZED,
                    "Expired account credential",
                ),
            )),
        ])));
        let state = authentication_fixture(partial, Response::new(Body::from("unused")));
        let response = route_chat_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"stream":true,"messages":[]}),
            vec![
                bound_health_candidate("refresh-account"),
                candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "refresh-backup"),
            ],
        )
        .await;
        let mut output = response.into_body().into_data_stream();
        assert!(
            String::from_utf8_lossy(&output.next().await.unwrap().unwrap()).contains("partial")
        );
        assert!(output.next().await.unwrap().is_err());
        assert!(output.next().await.is_none());
        let fixture = state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert!(fixture.rejections.is_empty());
        assert_eq!(fixture.retry_responses.len(), 1);
        assert!(state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .contains_key("refresh-backup"));
    }

    #[tokio::test]
    async fn refresh_preserves_original_budget_and_can_be_cancelled() {
        let state =
            authentication_fixture(unauthorized_body(), Response::new(Body::from("unused")));
        state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .delay = Duration::from_secs(1);
        let account = bound_health_candidate("refresh-account");
        let policy = RoutePolicy {
            time_budget: Duration::from_millis(20),
            ..RoutePolicy::default()
        };
        let response = route_chat_with_policy(
            &state,
            &HeaderMap::new(),
            json!({"stream":true,"messages":[]}),
            vec![account],
            policy,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        {
            let fixture = state
                .fixture_authentication
                .as_ref()
                .unwrap()
                .lock()
                .unwrap();
            assert_eq!(fixture.rejections.len(), 1);
            assert!(fixture.sent_credentials.is_empty());
            assert_eq!(fixture.retry_responses.len(), 1);
        }
        assert_eq!(
            state.route_memory.lock().unwrap().candidates["refresh-account"].failure,
            Some(FailureClass::Transient)
        );

        let state =
            authentication_fixture(unauthorized_body(), Response::new(Body::from("unused")));
        state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .delay = Duration::from_secs(1);
        let headers = HeaderMap::new();
        let request = route_chat_with_candidates(
            &state,
            &headers,
            json!({"stream":true,"messages":[]}),
            vec![bound_health_candidate("refresh-account")],
        );
        assert!(tokio::time::timeout(Duration::from_millis(20), request)
            .await
            .is_err());
        let fixture = state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert_eq!(fixture.rejections.len(), 1);
        assert!(fixture.sent_credentials.is_empty());
        assert_eq!(fixture.retry_responses.len(), 1);
        let memory = state.route_memory.lock().unwrap();
        assert!(memory.candidates.is_empty());
        assert!(memory.accounts.is_empty());
    }

    #[tokio::test]
    async fn bound_continuation_refreshes_same_account_without_switching() {
        for streaming in [false, true] {
            let state = authentication_fixture(unauthorized_body(), unauthorized_body());
            let account = bound_health_candidate("refresh-account");
            remember_response(&state, &account, Some("resp-bound"));
            let response = route_responses_with_candidates(
                &state,
                &HeaderMap::new(),
                json!({"stream":streaming,"input":"continue","previous_response_id":"resp-bound"}),
                vec![
                    account,
                    candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "refresh-backup"),
                ],
            )
            .await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                state
                    .fixture_authentication
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .rejections
                    .len(),
                1
            );
            assert!(state
                .fixture_responses
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .contains_key("refresh-backup"));
        }
    }

    #[tokio::test]
    async fn refresh_failure_keeps_its_rate_limit_classification() {
        let state =
            authentication_fixture(unauthorized_body(), Response::new(Body::from("unused")));
        state
            .fixture_responses
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .insert("refresh-backup".into(), successful_chat_fixture(true));
        state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .result = Err("accountError.rateLimited|HTTP 429".into());
        let response = route_chat_with_candidates(
            &state,
            &HeaderMap::new(),
            json!({"stream":true,"messages":[]}),
            vec![
                bound_health_candidate("refresh-account"),
                candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "refresh-backup"),
            ],
        )
        .await;
        assert_eq!(
            response.headers()["X-EchoBird-Routed-Via"],
            "refresh-backup/refresh-backup-model"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&bytes).contains("backup"));
        let fixture = state
            .fixture_authentication
            .as_ref()
            .unwrap()
            .lock()
            .unwrap();
        assert_eq!(fixture.rejections.len(), 1);
        assert!(fixture.sent_credentials.is_empty());
        assert_eq!(fixture.retry_responses.len(), 1);
        let memory = state.route_memory.lock().unwrap();
        assert_eq!(
            memory.candidates["refresh-account"].failure,
            Some(FailureClass::RateLimit)
        );
        assert_eq!(
            memory.accounts.values().next().unwrap().failure,
            Some(FailureClass::RateLimit)
        );
    }

    #[test]
    fn changing_model_clears_candidate_health_without_clearing_account_cooldown() {
        for failure in [
            FailureClass::RateLimit,
            FailureClass::Billing,
            FailureClass::Authentication,
        ] {
            let state = AppState::for_tests().unwrap();
            let account = bound_health_candidate("edited-account-model");
            mark_failure(&state, &account, failure, Some(Duration::from_secs(600)));
            let key = account_health_key(&account).unwrap();
            let deadline = state.route_memory.lock().unwrap().accounts[&key].cooldown_until_ms;
            assert!(clear_candidate_health(
                &mut state.route_memory.lock().unwrap(),
                &account.internal_id
            ));
            assert!(candidate_is_cooling_down(
                &state,
                &bound_health_candidate("sibling-account-model")
            ));
            let memory = state.route_memory.lock().unwrap();
            assert!(!memory.candidates.contains_key(&account.internal_id));
            assert_eq!(memory.accounts[&key].failure, Some(failure));
            assert_eq!(memory.accounts[&key].cooldown_until_ms, deadline);
        }
        for failure in [FailureClass::Transient, FailureClass::ModelUnavailable] {
            let state = AppState::for_tests().unwrap();
            let account = bound_health_candidate("edited-model-local");
            mark_failure(&state, &account, failure, None);
            clear_candidate_health(
                &mut state.route_memory.lock().unwrap(),
                &account.internal_id,
            );
            assert!(!candidate_is_cooling_down(&state, &account));
        }
    }

    #[tokio::test]
    async fn delivered_stream_request_rejection_ends_without_health_or_backup() {
        let error = crate::services::account_proxy::ProxyError::new(
            StatusCode::FORBIDDEN,
            "blocked by safety policy",
        );
        let body = Body::from_stream(stream::iter([
            Ok(Bytes::from_static(
                b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            )),
            Err(std::io::Error::other(error)),
        ]));
        let responses = Arc::new(Mutex::new(HashMap::from([
            ("delivered-rejection".to_string(), Response::new(body)),
            (
                "delivered-backup".to_string(),
                Response::new(Body::from("unused")),
            ),
        ])));
        let mut state = AppState::for_tests().unwrap();
        state.fixture_responses = Some(responses.clone());
        let response = route_chat_with_candidates(
            &state,
            &inbound_headers(),
            json!({"model":SMART_ROUTER_MODEL_ID,"messages":[],"stream":true}),
            vec![
                bound_health_candidate("delivered-rejection"),
                bound_health_candidate("delivered-backup"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let mut output = response.into_body().into_data_stream();
        let first = output.next().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&first).contains("partial"));
        assert!(output.next().await.unwrap().is_err());
        assert!(output.next().await.is_none());
        assert!(responses.lock().unwrap().contains_key("delivered-backup"));
        let memory = state.route_memory.lock().unwrap();
        assert!(memory.candidates.is_empty());
        assert!(memory.accounts.is_empty());
    }

    #[tokio::test]
    async fn converted_responses_try_backup_before_creation_and_preserve_account_limit() {
        for streaming in [false, true] {
            let request = json!({"model":SMART_ROUTER_MODEL_ID,"input":"hello","stream":streaming});
            let chat = responses_chat_request(&request).unwrap();
            let mut error = crate::services::account_proxy::ProxyError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Trae account is rate limited",
            );
            error.error_code = Some("trae_4011".into());
            error.retry_after = Some(HeaderValue::from_static("120"));
            let first = responses_from_chat(typed_body_failure(error), &chat)
                .await
                .unwrap();
            let backup_chat = Response::builder().header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"backup output\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")).unwrap();
            let backup = responses_from_chat(backup_chat, &chat).await.unwrap();
            let fixtures = Arc::new(Mutex::new(HashMap::from([
                ("responses-early".into(), first),
                ("responses-backup".into(), backup),
            ])));
            let mut state = AppState::for_tests().unwrap();
            state.fixture_responses = Some(fixtures.clone());
            let mut limited = bound_health_candidate("responses-early");
            limited.account_source.as_mut().unwrap().provider =
                crate::models::model::AccountProvider::Traecn;
            let key = account_health_key(&limited).unwrap();
            let started = now_ms();
            let response = route_responses_with_candidates(
                &state,
                &inbound_headers(),
                request,
                vec![
                    limited,
                    candidate(SocketAddr::from(([127, 0, 0, 1], 1)), "responses-backup"),
                ],
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()["X-EchoBird-Routed-Via"],
                "responses-backup/responses-backup-model"
            );
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let text = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(text.contains("backup output"));
            assert!(!text.contains("response.failed"));
            if streaming {
                assert_eq!(text.matches("event: response.created").count(), 1);
                assert!(text.contains("response.completed"));
            } else {
                let value: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["status"], "completed");
            }
            assert!(fixtures.lock().unwrap().is_empty());
            let memory = state.route_memory.lock().unwrap();
            assert_eq!(memory.accounts[&key].failure, Some(FailureClass::RateLimit));
            assert!(memory.accounts[&key].cooldown_until_ms.unwrap() >= started + 120_000);
        }
    }
}
