mod eventstream;
mod request;
mod stream;
#[cfg(test)]
mod tests;

use super::{providers, ProxyError};
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
const JSON_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug)]
struct Model {
    id: String,
    name: String,
    images: bool,
}

struct Catalog {
    at: Instant,
    models: Vec<Model>,
}

fn catalogs() -> &'static Mutex<BTreeMap<String, Catalog>> {
    static CACHE: OnceLock<Mutex<BTreeMap<String, Catalog>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

type Labels = BTreeMap<String, BTreeMap<String, String>>;
fn labels() -> &'static Mutex<Labels> {
    static LABELS: OnceLock<Mutex<Labels>> = OnceLock::new();
    LABELS.get_or_init(|| {
        #[cfg(not(test))]
        let saved = crate::services::cursor_auth::read(&labels_path()).unwrap_or_default();
        #[cfg(test)]
        let saved = Labels::default();
        Mutex::new(saved)
    })
}
#[cfg(not(test))]
fn labels_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".echobird/cache/kiro-model-labels.json")
}

fn client() -> Result<&'static reqwest::Client, ProxyError> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| "Could not create Kiro client".into())
        })
        .as_ref()
        .map_err(ProxyError::upstream)
}

fn required<'a>(credentials: &'a Value, key: &str) -> Result<&'a str, ProxyError> {
    credentials[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ProxyError::new(
                StatusCode::UNAUTHORIZED,
                "Kiro account credentials are incomplete",
            )
        })
}

fn region(credentials: &Value) -> Result<&str, ProxyError> {
    let region = required(credentials, "region")?;
    if region.len() > 32
        || region.split('-').count() < 3
        || !region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ProxyError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid Kiro account region",
        ));
    }
    if let Some(profile) = credentials["profileArn"]
        .as_str()
        .filter(|profile| !profile.is_empty())
    {
        let parts: Vec<_> = profile.split(':').collect();
        if parts.len() != 6
            || parts[0] != "arn"
            || parts[2] != "codewhisperer"
            || parts[3] != region
            || !parts[5].starts_with("profile/")
            || parts[5] == "profile/"
        {
            return Err(ProxyError::new(
                StatusCode::UNAUTHORIZED,
                "Kiro profile does not match its account region",
            ));
        }
    }
    Ok(region)
}

fn base(credentials: &Value, management: bool) -> Result<String, ProxyError> {
    let region = region(credentials)?;
    #[cfg(test)]
    if let Some(base) = credentials["baseUrl"].as_str() {
        let url = url::Url::parse(base).map_err(ProxyError::upstream)?;
        if url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
        {
            return Ok(base.trim_end_matches('/').into());
        }
        return Err(ProxyError::upstream("Fixture endpoint must be loopback"));
    }
    Ok(format!(
        "https://{}.{region}.kiro.dev",
        if management { "management" } else { "runtime" }
    ))
}

fn signed(
    request: reqwest::RequestBuilder,
    credentials: &Value,
) -> Result<reqwest::RequestBuilder, ProxyError> {
    let mut request = request.bearer_auth(required(credentials, "accessToken")?);
    if let Some(kind) = credentials["tokenType"]
        .as_str()
        .filter(|kind| !kind.is_empty())
    {
        if kind != "EXTERNAL_IDP" {
            return Err(ProxyError::new(
                StatusCode::UNAUTHORIZED,
                "Unsupported Kiro account token type",
            ));
        }
        request = request.header("tokentype", kind);
    }
    Ok(request)
}

fn cache_key(credentials: &Value) -> Result<String, ProxyError> {
    Ok(json!([
        base(credentials, true)?,
        required(credentials, "accountId")?,
        required(credentials, "credentialGeneration")?,
        region(credentials)?,
        credentials["profileArn"]
    ])
    .to_string())
}

pub(super) fn cached_model_labels(account_id: &str) -> BTreeMap<String, String> {
    let Ok(labels) = labels().lock() else {
        return BTreeMap::new();
    };
    labels.get(account_id).cloned().unwrap_or_default()
}

async fn read_json(response: reqwest::Response) -> Result<Value, ProxyError> {
    let mut data = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ProxyError::upstream("Kiro response transport failed"))?;
        if data.len().saturating_add(chunk.len()) > JSON_LIMIT {
            return Err(ProxyError::upstream("Kiro JSON response is too large"));
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| ProxyError::upstream("Invalid Kiro JSON response"))
}

fn failure(status: StatusCode, headers: &HeaderMap, value: &Value, kind: &str) -> ProxyError {
    let reason = value["reason"].as_str().unwrap_or_default();
    let code = value["__type"]
        .as_str()
        .or_else(|| value["code"].as_str())
        .unwrap_or(kind);
    let message = value["message"]
        .as_str()
        .or_else(|| value["Message"].as_str())
        .unwrap_or("Kiro upstream refused the request");
    let detail = format!("{code} {reason} {message}").to_ascii_lowercase();
    let (status, model_scope) = if detail.contains("content_length_exceeds_threshold")
        || detail.contains("input is too long")
        || detail.contains("invalid_model_id")
    {
        (StatusCode::BAD_REQUEST, true)
    } else if detail.contains("insufficient_model_capacity")
        || detail.contains("serviceunavailable")
        || detail.contains("service_unavailable")
    {
        (StatusCode::SERVICE_UNAVAILABLE, true)
    } else if detail.contains("monthly_request_count")
        || detail.contains("usage_limit")
        || detail.contains("overage_request_limit")
        || detail.contains("servicequotaexceeded")
        || detail.contains("throttl")
        || detail.contains("ratelimit")
    {
        (StatusCode::TOO_MANY_REQUESTS, false)
    } else if detail.contains("accessdenied")
        || detail.contains("unauthorized")
        || detail.contains("expiredtoken")
        || detail.contains("invalidtoken")
        || detail.contains("invalid bearer")
        || detail.contains("bearer token included")
    {
        (StatusCode::UNAUTHORIZED, false)
    } else if detail.contains("validation") {
        (StatusCode::BAD_REQUEST, true)
    } else {
        (status, false)
    };
    let mut error =
        providers::upstream_error(status, headers, &json!({"message":message,"code":code}));
    error.model_scope = model_scope;
    if !reason.is_empty() {
        error.message = format!(
            "{} ({})",
            error.message,
            reason.chars().take(160).collect::<String>()
        );
    }
    error
}

pub(super) async fn list_models(
    credentials: Value,
    _client: &reqwest::Client,
) -> Result<Vec<String>, ProxyError> {
    tokio::time::timeout(Duration::from_secs(30), fetch_models(credentials))
        .await
        .map_err(|_| ProxyError::upstream("Kiro model directory timed out"))?
}

async fn fetch_models(credentials: Value) -> Result<Vec<String>, ProxyError> {
    let key = cache_key(&credentials)?;
    let mut models = Vec::new();
    let mut ids = BTreeSet::new();
    let mut tokens = BTreeSet::new();
    let mut next: Option<String> = None;
    let mut default = None;
    loop {
        let mut request = signed(
            client()?
                .get(format!(
                    "{}/List-Available-Models",
                    base(&credentials, true)?
                ))
                .timeout(Duration::from_secs(20))
                .query(&[("origin", "KIRO_CLI")]),
            &credentials,
        )?;
        if let Some(profile) = credentials["profileArn"]
            .as_str()
            .filter(|profile| !profile.is_empty())
        {
            request = request.query(&[("profileArn", profile)]);
        }
        if let Some(token) = &next {
            request = request.query(&[("nextToken", token)]);
        }
        let response = request
            .send()
            .await
            .map_err(|_| ProxyError::upstream("Kiro model directory transport failed"))?;
        let status = response.status();
        let headers = response.headers().clone();
        let value = read_json(response).await;
        if !status.is_success() {
            return Err(failure(status, &headers, &value.unwrap_or(Value::Null), ""));
        }
        let value = value?;
        if default.is_none() {
            default = value["defaultModel"]["modelId"].as_str().map(str::to_owned);
        }
        let rows = value["models"]
            .as_array()
            .ok_or_else(|| ProxyError::upstream("Invalid Kiro model directory"))?;
        for row in rows {
            let id = row["modelId"]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| ProxyError::upstream("Kiro listed a model without an ID"))?;
            if ids.insert(id.to_owned()) {
                models.push(Model {
                    id: id.into(),
                    name: row["modelName"]
                        .as_str()
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or(id)
                        .into(),
                    images: row["supportedInputTypes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|kind| {
                            kind.as_str()
                                .is_some_and(|kind| kind.eq_ignore_ascii_case("IMAGE"))
                        }),
                });
            }
        }
        next = value["nextToken"]
            .as_str()
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
        let Some(token) = &next else {
            break;
        };
        if tokens.len() >= 100 || !tokens.insert(token.clone()) {
            return Err(ProxyError::upstream(
                "Kiro model directory pagination did not finish",
            ));
        }
    }
    if models.is_empty() {
        return Err(ProxyError::upstream("Kiro listed no available models"));
    }
    if let Some(index) = models
        .iter()
        .position(|model| Some(model.id.as_str()) == default.as_deref())
    {
        let model = models.remove(index);
        models.insert(0, model);
    }
    let names = models.iter().map(|model| model.id.clone()).collect();
    if let Ok(mut labels) = labels().lock() {
        labels.insert(
            required(&credentials, "accountId")?.into(),
            models
                .iter()
                .map(|model| (model.id.clone(), model.name.clone()))
                .collect(),
        );
        #[cfg(not(test))]
        let _ = crate::services::cursor_auth::write(&labels_path(), &*labels);
    }
    let mut cache = catalogs()
        .lock()
        .map_err(|_| ProxyError::upstream("Kiro model cache unavailable"))?;
    cache.retain(|_, catalog| catalog.at.elapsed() < CACHE_LIFETIME);
    cache.insert(
        key,
        Catalog {
            at: Instant::now(),
            models,
        },
    );
    Ok(names)
}

async fn resolve_model(
    credentials: &Value,
    id: &str,
    client: &reqwest::Client,
) -> Result<Model, ProxyError> {
    let key = cache_key(credentials)?;
    let cached = catalogs().lock().ok().and_then(|cache| {
        cache
            .get(&key)
            .filter(|catalog| catalog.at.elapsed() < CACHE_LIFETIME)
            .map(|catalog| catalog.models.clone())
    });
    let models = if let Some(models) = cached {
        models
    } else {
        list_models(credentials.clone(), client).await?;
        catalogs()
            .lock()
            .map_err(|_| ProxyError::upstream("Kiro model cache unavailable"))?
            .get(&key)
            .map(|catalog| catalog.models.clone())
            .unwrap_or_default()
    };
    models
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| {
            request::unsupported("This Kiro model is not available in the account directory")
        })
}

pub(super) async fn route(
    credentials: Value,
    model: &str,
    body: Value,
    client: &reqwest::Client,
) -> Result<Response, ProxyError> {
    request::validate(&body)?;
    let selected = resolve_model(&credentials, model, client).await?;
    let built = request::build(&body, &selected, credentials["profileArn"].as_str())?;
    let user_agent = concat!(
        "EchoBird/",
        env!("CARGO_PKG_VERSION"),
        " api/codewhispererstreaming"
    );
    let response = signed(
        self::client()?.post(format!(
            "{}/generateAssistantResponse",
            base(&credentials, false)?
        )),
        &credentials,
    )?
    .header(header::CONTENT_TYPE, "application/json")
    .header(header::ACCEPT, "application/vnd.amazon.eventstream")
    .header("x-amzn-codewhisperer-optout", "true")
    .header("x-amzn-kiro-agent-mode", "vibe")
    .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
    .header("amz-sdk-request", "attempt=1; max=1")
    .header("x-amz-user-agent", user_agent)
    .header(header::USER_AGENT, user_agent)
    .json(&built.body)
    .send()
    .await
    .map_err(|_| ProxyError::upstream("Kiro inference transport failed"))?;
    if !response.status().is_success() {
        let status = response.status();
        let headers = response.headers().clone();
        let value = read_json(response).await.unwrap_or(Value::Null);
        return Err(failure(status, &headers, &value, ""));
    }
    if !response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim() == "application/vnd.amazon.eventstream")
        })
    {
        return Err(ProxyError::upstream("Kiro did not return an event stream"));
    }
    stream::convert(
        response,
        model,
        body["stream"] == true,
        built.names,
        built.allowed,
        built.thinking,
    )
    .await
}
