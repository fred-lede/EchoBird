use super::{client, response, text, valid_region, Credentials, Edition, LoginStart};
use axum::{
    extract::{OriginalUri, Query, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;

const PORTAL: &str = "https://app.kiro.dev/signin";
const SOCIAL: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
const PORTS: [u16; 10] = [
    3128, 4649, 6588, 8008, 9091, 49153, 50153, 51153, 52153, 53153,
];
const SCOPES: [&str; 3] = [
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
];
static PENDING: Mutex<Option<Arc<Attempt>>> = Mutex::new(None);
static START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

enum Outcome {
    Waiting,
    Exchanging,
    Done(Result<Credentials, String>),
}
struct Aws {
    state: String,
    verifier: String,
    region: String,
    issuer: String,
    provider: String,
    registration: Value,
}
struct Attempt {
    id: String,
    edition: Edition,
    state: String,
    verifier: String,
    port: u16,
    expires: i64,
    cancel: CancellationToken,
    outcome: Mutex<Outcome>,
    aws: Mutex<Option<Aws>>,
}

fn random() -> String {
    let mut value = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier))
}
fn alive(attempt: &Attempt) -> bool {
    !attempt.cancel.is_cancelled() && attempt.expires > chrono::Utc::now().timestamp()
}
fn url(port: u16, state: &str, verifier: &str) -> Result<String, String> {
    let mut url = url::Url::parse(PORTAL).map_err(|_| "accountError.auth")?;
    url.query_pairs_mut().extend_pairs([
        ("state", state),
        ("code_challenge", &challenge(verifier)),
        ("code_challenge_method", "S256"),
        ("redirect_uri", &format!("http://localhost:{port}")),
        ("redirect_from", "kirocli"),
    ]);
    Ok(url.into())
}

pub(super) async fn start(edition: Edition) -> Result<LoginStart, String> {
    let _guard = START_LOCK.lock().await;
    if let Some(previous) = PENDING.lock().map_err(|_| "accountError.busy")?.take() {
        previous.cancel.cancel();
    }
    let mut listener = None;
    for port in PORTS {
        if let Ok(value) =
            tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await
        {
            listener = Some(value);
            break;
        }
    }
    let listener = listener.ok_or("accountError.busy")?;
    let port = listener
        .local_addr()
        .map_err(|_| "accountError.auth")?
        .port();
    let attempt = Arc::new(Attempt {
        id: uuid::Uuid::new_v4().to_string(),
        edition,
        state: random(),
        verifier: random(),
        port,
        expires: chrono::Utc::now().timestamp() + 60,
        cancel: CancellationToken::new(),
        outcome: Mutex::new(Outcome::Waiting),
        aws: Mutex::new(None),
    });
    let result = LoginStart {
        login_id: attempt.id.clone(),
        verification_uri: url(port, &attempt.state, &attempt.verifier)?,
        expires_at: attempt.expires,
    };
    *PENDING.lock().map_err(|_| "accountError.busy")? = Some(attempt.clone());
    let router = Router::new()
        .route("/oauth/callback", get(callback))
        .route("/signin/callback", get(callback))
        .with_state(attempt.clone());
    let stop = attempt.cancel.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await;
    });
    tokio::spawn(async move {
        tokio::select! { _ = attempt.cancel.cancelled() => {}, _ = tokio::time::sleep(Duration::from_secs(60)) => attempt.cancel.cancel() }
    });
    Ok(result)
}

fn one<'a>(pairs: &'a [(String, String)], key: &str) -> Result<Option<&'a str>, String> {
    let values = pairs
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    if values.len() > 1 {
        return Err("accountError.state".into());
    }
    Ok(values.first().copied())
}

async fn post(url: &str, value: &Value) -> Result<Value, String> {
    response(
        client()?
            .post(url)
            .json(value)
            .send()
            .await
            .map_err(|_| "accountError.network")?,
    )
    .await
}

fn expires(value: &Value) -> Result<String, String> {
    let seconds = value["expiresIn"]
        .as_i64()
        .filter(|n| *n > 0)
        .ok_or("accountError.authResponse")?;
    let expiry = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::seconds(seconds))
        .ok_or("accountError.authResponse")?;
    Ok(expiry.to_rfc3339())
}

async fn exchange(
    attempt: &Attempt,
    pairs: &[(String, String)],
    path: &str,
) -> Result<Option<String>, String> {
    let state = one(pairs, "state")?.ok_or("accountError.state")?;
    let aws = attempt.aws.lock().map_err(|_| "accountError.busy")?.take();
    let (value, provider, registration, region, issuer) = if let Some(aws) = aws {
        if state != aws.state {
            *attempt.aws.lock().map_err(|_| "accountError.busy")? = Some(aws);
            return Err("accountError.state".into());
        }
        let code = one(pairs, "code")?
            .filter(|v| !v.is_empty())
            .ok_or("accountError.authResponse")?;
        let result = post(&format!("https://oidc.{}.amazonaws.com/token", aws.region), &json!({
            "clientId":aws.registration["client_id"],"clientSecret":aws.registration["client_secret"],
            "grantType":"authorization_code","redirectUri":format!("http://127.0.0.1:{}/oauth/callback",attempt.port),"code":code,"codeVerifier":aws.verifier,
        })).await?;
        (
            result,
            aws.provider,
            Some(aws.registration),
            aws.region,
            Some(aws.issuer),
        )
    } else {
        if state != attempt.state {
            return Err("accountError.state".into());
        }
        if one(pairs, "error")?.is_some() {
            return Err("accountError.auth".into());
        }
        let option = one(pairs, "login_option")?.ok_or("accountError.authResponse")?;
        if matches!(option, "builderid" | "awsidc" | "internal") {
            let region = one(pairs, "idc_region")?
                .filter(|value| valid_region(value))
                .ok_or("accountError.authResponse")?
                .to_string();
            let issuer = one(pairs, "issuer_url")?
                .ok_or("accountError.authResponse")?
                .to_string();
            let parsed = url::Url::parse(&issuer).map_err(|_| "accountError.authResponse")?;
            if parsed.scheme() != "https"
                || !parsed
                    .host_str()
                    .is_some_and(|host| host.ends_with(".awsapps.com"))
                || parsed.port().is_some()
                || !parsed.username().is_empty()
            {
                return Err("accountError.authResponse".into());
            }
            let reg = post(&format!("https://oidc.{region}.amazonaws.com/client/register"), &json!({
                "clientName":"EchoBird","clientType":"public","scopes":SCOPES,"grantTypes":["authorization_code","refresh_token"],
                "redirectUris":["http://127.0.0.1/oauth/callback"],"issuerUrl":issuer,
            })).await?;
            let client_id = text(&reg, "clientId").ok_or("accountError.authResponse")?;
            let client_secret = text(&reg, "clientSecret").ok_or("accountError.authResponse")?;
            let aws_state = random();
            let verifier = random();
            let mut redirect =
                url::Url::parse(&format!("https://oidc.{region}.amazonaws.com/authorize"))
                    .map_err(|_| "accountError.auth")?;
            redirect.query_pairs_mut().extend_pairs([
                ("response_type", "code"),
                ("client_id", client_id),
                (
                    "redirect_uri",
                    &format!("http://127.0.0.1:{}/oauth/callback", attempt.port),
                ),
                ("scopes", &SCOPES.join(",")),
                ("state", &aws_state),
                ("code_challenge", &challenge(&verifier)),
                ("code_challenge_method", "S256"),
            ]);
            let expiry = reg["clientSecretExpiresAt"]
                .as_i64()
                .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0))
                .ok_or("accountError.authResponse")?
                .to_rfc3339();
            let registration = json!({"client_id":client_id,"client_secret":client_secret,"expires_at":expiry,"region":region});
            *attempt.aws.lock().map_err(|_| "accountError.busy")? = Some(Aws {
                state: aws_state,
                verifier,
                region,
                issuer,
                provider: if option == "builderid" {
                    "BuilderId"
                } else {
                    "Enterprise"
                }
                .into(),
                registration,
            });
            return Ok(Some(redirect.into()));
        }
        if !matches!(option, "google" | "github") {
            return Err("accountError.unavailable".into());
        }
        let code = one(pairs, "code")?
            .filter(|value| !value.is_empty())
            .ok_or("accountError.authResponse")?;
        let redirect = format!(
            "http://localhost:{}{path}?login_option={option}",
            attempt.port
        );
        let result = post(
            &format!("{SOCIAL}/oauth/token"),
            &json!({"code":code,"code_verifier":attempt.verifier,"redirect_uri":redirect}),
        )
        .await?;
        (result, option.into(), None, "us-east-1".into(), None)
    };
    let access = text(&value, "accessToken").ok_or("accountError.authResponse")?;
    let refresh = text(&value, "refreshToken").ok_or("accountError.authResponse")?;
    let mut credentials = Credentials {
        token: json!({"access_token":access,"refresh_token":refresh,"expires_at":expires(&value)?,"provider":provider,
            "profile_arn":value["profileArn"],"region":region,"start_url":issuer,"telemetry_generation":uuid::Uuid::new_v4().to_string()}),
        registration,
        subject: None,
        name: None,
        usage: None,
        source: None,
        source_path: None,
    };
    super::quota::fetch(&mut credentials).await?;
    if credentials.identity().is_none() {
        return Err("accountError.authResponse".into());
    }
    if !alive(attempt) {
        return Err("accountError.cancelled".into());
    }
    *attempt.outcome.lock().map_err(|_| "accountError.busy")? = Outcome::Done(Ok(credentials));
    Ok(None)
}

async fn callback(
    State(attempt): State<Arc<Attempt>>,
    OriginalUri(uri): OriginalUri,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    if !alive(&attempt) {
        return (StatusCode::GONE, "This sign-in has ended.").into_response();
    }
    let expected = attempt
        .aws
        .lock()
        .ok()
        .and_then(|aws| aws.as_ref().map(|a| a.state.clone()))
        .unwrap_or_else(|| attempt.state.clone());
    if one(&pairs, "state").ok().flatten() != Some(expected.as_str()) {
        return (StatusCode::BAD_REQUEST, "Invalid sign-in state.").into_response();
    }
    {
        let Ok(mut outcome) = attempt.outcome.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if !matches!(*outcome, Outcome::Waiting) {
            return StatusCode::CONFLICT.into_response();
        }
        *outcome = Outcome::Exchanging;
    }
    let result = tokio::select! { _ = attempt.cancel.cancelled() => Err("accountError.cancelled".into()), result = exchange(&attempt,&pairs,uri.path()) => result };
    if !alive(&attempt) {
        return (StatusCode::GONE, "This sign-in has ended.").into_response();
    }
    match result {
        Ok(Some(url)) => {
            if let Ok(mut outcome) = attempt.outcome.lock() { *outcome = Outcome::Waiting; }
            (StatusCode::FOUND,[(header::LOCATION,url)]).into_response()
        },
        Ok(None) => Html("<!doctype html><html><meta charset=utf-8><title>EchoBird</title><body><h1>Signed in</h1><p>You can close this window.</p></body></html>").into_response(),
        Err(error) => {
            if alive(&attempt) { if let Ok(mut outcome) = attempt.outcome.lock() { *outcome = Outcome::Done(Err(error)); } }
            (StatusCode::BAD_REQUEST,"Sign-in failed. Return to EchoBird and try again.").into_response()
        },
    }
}

pub(super) fn poll(id: &str) -> Result<Option<(Edition, Credentials)>, String> {
    let pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let Some(attempt) = pending.as_ref().filter(|attempt| attempt.id == id) else {
        return Ok(None);
    };
    if !alive(attempt) {
        return Err("accountError.expired".into());
    }
    let outcome = attempt.outcome.lock().map_err(|_| "accountError.busy")?;
    match &*outcome {
        Outcome::Done(result) => result
            .clone()
            .map(|credentials| Some((attempt.edition, credentials))),
        _ => Ok(None),
    }
}
pub(super) fn complete<T>(id: &str, save: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let attempt = pending
        .as_ref()
        .filter(|attempt| attempt.id == id && alive(attempt))
        .ok_or("accountError.cancelled")?;
    let result = save()?;
    attempt.cancel.cancel();
    *pending = None;
    Ok(result)
}
pub(super) fn cancel(id: &str) -> Result<(), String> {
    let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    if pending.as_ref().is_some_and(|attempt| attempt.id == id) {
        if let Some(attempt) = pending.take() {
            attempt.cancel.cancel();
        }
    }
    Ok(())
}

pub(super) async fn ensure_fresh(credentials: &mut Credentials) -> Result<(), String> {
    let expiry = text(&credentials.token, "expires_at")
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
    if expiry.is_some_and(|expiry| expiry.timestamp() > chrono::Utc::now().timestamp() + 120) {
        return Ok(());
    }
    refresh_at(credentials, None).await
}

pub(super) async fn refresh_at(
    credentials: &mut Credentials,
    endpoint: Option<&str>,
) -> Result<(), String> {
    let refresh = text(&credentials.token, "refresh_token").ok_or("accountError.expired")?;
    let value = if let Some(endpoint) = endpoint {
        post(endpoint, &json!({"refreshToken":refresh})).await?
    } else if credentials.is_social() {
        post(
            &format!(
                "https://prod.{}.auth.desktop.kiro.dev/refreshToken",
                credentials.auth_region()?
            ),
            &json!({"refreshToken":refresh}),
        )
        .await?
    } else if credentials.provider().eq_ignore_ascii_case("ExternalIdp") {
        let endpoint = text(&credentials.token, "token_endpoint").ok_or("accountError.expired")?;
        let parsed = url::Url::parse(endpoint).map_err(|_| "accountError.auth")?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
        {
            return Err("accountError.auth".into());
        }
        response(
            client()?
                .post(endpoint)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh),
                    (
                        "client_id",
                        text(&credentials.token, "client_id").unwrap_or(""),
                    ),
                ])
                .send()
                .await
                .map_err(|_| "accountError.network")?,
        )
        .await?
    } else {
        let registration = credentials
            .registration
            .as_ref()
            .ok_or("accountError.expired")?;
        post(&format!("https://oidc.{}.amazonaws.com/token",credentials.auth_region()?), &json!({"clientId":registration["client_id"],"clientSecret":registration["client_secret"],"refreshToken":refresh,"grantType":"refresh_token"})).await?
    };
    let access = text(&value, "accessToken")
        .or_else(|| text(&value, "access_token"))
        .ok_or("accountError.authResponse")?;
    let mut normalized = value.clone();
    if normalized["expiresIn"].is_null() {
        normalized["expiresIn"] = value["expires_in"].clone();
    }
    credentials.token["access_token"] = json!(access);
    credentials.token["expires_at"] = json!(expires(&normalized)?);
    if let Some(refresh) = text(&value, "refreshToken").or_else(|| text(&value, "refresh_token")) {
        credentials.token["refresh_token"] = json!(refresh);
    }
    Ok(())
}
