use super::{text, Credentials, LoginStart, Region};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const CLIENT_ID: &str = "mcode-public";
static PENDING: Mutex<Option<Arc<Attempt>>> = Mutex::new(None);

struct Attempt {
    id: String,
    region: Region,
    expires: i64,
    cancel: CancellationToken,
    outcome: Mutex<Option<Result<Credentials, String>>>,
}

struct Device {
    verifier: String,
    code: String,
    by_user: bool,
    interval: Duration,
}

pub(super) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "accountError.network".into())
}

async fn form(
    client: &reqwest::Client,
    url: &str,
    fields: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, Value), String> {
    let response = client
        .post(url)
        .form(fields)
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| "accountError.network")?;
    let status = response.status();
    let body = response
        .json()
        .await
        .map_err(|_| "accountError.authResponse")?;
    Ok((status, body))
}

fn token(
    region: Region,
    body: &Value,
    previous: Option<&Credentials>,
) -> Result<Credentials, String> {
    let access = text(body, "access_token").ok_or("accountError.authResponse")?;
    let ttl = body["expires_in"]
        .as_i64()
        .filter(|ttl| *ttl > 0)
        .ok_or("accountError.authResponse")?;
    let claims: Value = access
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    if let Some(previous) = previous {
        let previous_claims: Value = previous
            .access_token
            .split('.')
            .nth(1)
            .and_then(|part| URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        if previous.region != region
            || ["sub", "account_id"].into_iter().any(|key| {
                text(&previous_claims, key)
                    .zip(text(&claims, key))
                    .is_some_and(|(before, after)| before != after)
            })
        {
            return Err("accountError.invalidAccount".into());
        }
    }
    let user = previous
        .map(|previous| previous.user_id.as_str())
        .or_else(|| text(&claims, "account_id"))
        .or_else(|| text(&claims, "sub"))
        .or_else(|| text(body, "account_id"))
        .or_else(|| text(body, "subject"))
        .unwrap_or("");
    let refresh = text(body, "refresh_token")
        .or_else(|| previous.map(|previous| previous.refresh_token.as_str()))
        .filter(|refresh| !refresh.is_empty())
        .ok_or("accountError.authResponse")?;
    Ok(Credentials {
        region,
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: chrono::Utc::now().timestamp().saturating_add(ttl),
        user_id: user.into(),
        email: previous
            .map(|previous| previous.email.clone())
            .or_else(|| text(&claims, "email").map(str::to_string))
            .unwrap_or_else(|| user.into()),
        name: previous.and_then(|previous| previous.name.clone()),
    })
}

fn validate(attempt: &Attempt) -> Result<(), String> {
    if chrono::Utc::now().timestamp() >= attempt.expires {
        return Err("accountError.expired".into());
    }
    if attempt.cancel.is_cancelled() {
        return Err("accountError.cancelled".into());
    }
    Ok(())
}

pub(super) async fn start(region: Region) -> Result<LoginStart, String> {
    start_at(region, region.account_host()).await
}

async fn start_at(region: Region, host: &str) -> Result<LoginStart, String> {
    let attempt = Arc::new(Attempt {
        id: uuid::Uuid::new_v4().to_string(),
        region,
        expires: chrono::Utc::now().timestamp() + 60,
        cancel: CancellationToken::new(),
        outcome: Mutex::new(None),
    });
    {
        let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
        if let Some(previous) = pending.replace(attempt.clone()) {
            previous.cancel.cancel();
        }
    }
    let result = start_device(&attempt, host).await;
    if result.is_err() {
        cancel(&attempt.id)?;
    }
    result
}

async fn start_device(attempt: &Arc<Attempt>, host: &str) -> Result<LoginStart, String> {
    let client = client()?;
    let mut random = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let verifier = URL_SAFE_NO_PAD.encode(random);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let device_url = format!("{host}/oauth2/device/code");
    let device_fields = [
        ("client_id", CLIENT_ID),
        ("scope", "agent.default"),
        ("audience", "agent-backend"),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
    ];
    let (status, body) = tokio::select! {
        _ = attempt.cancel.cancelled() => return Err("accountError.cancelled".into()),
        result = form(&client, &device_url, &device_fields) => result?,
    };
    validate(attempt)?;
    if !status.is_success() || body.get("error").is_some() {
        return Err("accountError.auth".into());
    }
    let user_code = text(&body, "user_code").ok_or("accountError.authResponse")?;
    let by_user = text(&body, "device_code").is_none() && body["expired_in"].is_number();
    let code = text(&body, if by_user { "user_code" } else { "device_code" })
        .ok_or("accountError.authResponse")?;
    let uri = text(&body, "verification_uri_complete")
        .or_else(|| text(&body, "verification_uri"))
        .or_else(|| text(&body, "verification_url"))
        .ok_or("accountError.authResponse")?;
    let parsed = url::Url::parse(uri).map_err(|_| "accountError.authResponse")?;
    if !verification_host(attempt.region, &parsed) {
        return Err("accountError.authResponse".into());
    }
    let interval = body["interval"]
        .as_u64()
        .filter(|interval| *interval > 0)
        .unwrap_or(if by_user { 5000 } else { 5 });
    let server_end = body["expires_in"]
        .as_i64()
        .filter(|ttl| *ttl > 0)
        .map(|ttl| chrono::Utc::now().timestamp().saturating_add(ttl))
        .or_else(|| {
            body["expired_in"]
                .as_i64()
                .filter(|time| *time > 0)
                .map(|time| {
                    if time >= 1_000_000_000_000 {
                        time / 1000
                    } else {
                        chrono::Utc::now().timestamp().saturating_add(time)
                    }
                })
        });
    let expires = server_end.unwrap_or(attempt.expires).min(attempt.expires);
    let device = Device {
        verifier,
        code: code.into(),
        by_user,
        interval: if by_user {
            Duration::from_millis(interval)
        } else {
            Duration::from_secs(interval)
        },
    };
    let result = LoginStart {
        login_id: attempt.id.clone(),
        verification_uri: uri.into(),
        user_code: user_code.into(),
        expires_at: expires,
    };
    let attempt = attempt.clone();
    let host = host.to_string();
    tokio::spawn(async move {
        let result = tokio::select! {
            _ = attempt.cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs((expires - chrono::Utc::now().timestamp()).max(0) as u64)) => Err("accountError.expired".into()),
            result = poll_device(&client, &attempt, &host, device) => result,
        };
        if !attempt.cancel.is_cancelled() {
            if let Ok(mut outcome) = attempt.outcome.lock() {
                *outcome = Some(result);
            }
        }
    });
    Ok(result)
}

fn verification_host(region: Region, url: &url::Url) -> bool {
    #[cfg(test)]
    if url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        return true;
    }
    let suffixes = match region {
        Region::Cn => &["minimax.cn", "minimaxi.com"][..],
        Region::Global => &["minimax.io"][..],
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| {
            suffixes
                .iter()
                .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
        })
}

async fn poll_device(
    client: &reqwest::Client,
    attempt: &Attempt,
    host: &str,
    mut device: Device,
) -> Result<Credentials, String> {
    loop {
        tokio::time::sleep(device.interval).await;
        validate(attempt)?;
        let (status, body) = form(
            client,
            &format!("{host}/oauth2/token"),
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                (
                    if device.by_user {
                        "user_code"
                    } else {
                        "device_code"
                    },
                    &device.code,
                ),
                ("client_id", CLIENT_ID),
                ("code_verifier", &device.verifier),
            ],
        )
        .await?;
        let state = text(&body, "error").or_else(|| text(&body, "status"));
        match state {
            Some("authorization_pending" | "pending") => continue,
            Some("slow_down") => {
                device.interval += Duration::from_secs(5);
                continue;
            }
            Some("expired" | "expired_token") => return Err("accountError.expired".into()),
            Some("denied" | "access_denied") => return Err("accountError.auth".into()),
            Some(_) if body.get("error").is_some() => return Err("accountError.auth".into()),
            _ => {}
        }
        if !status.is_success() {
            return Err("accountError.auth".into());
        }
        validate(attempt)?;
        return token(attempt.region, &body, None);
    }
}

pub(super) async fn refresh_at(
    client: &reqwest::Client,
    host: &str,
    previous: &Credentials,
) -> Result<Credentials, String> {
    let (status, body) = form(
        client,
        &format!("{host}/oauth2/token"),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &previous.refresh_token),
            ("client_id", CLIENT_ID),
            ("scope", "agent.default"),
            ("audience", "agent-backend"),
        ],
    )
    .await?;
    if status == reqwest::StatusCode::BAD_REQUEST && body["error"] == "invalid_grant" {
        return Err("accountError.loginRequired".into());
    }
    if !status.is_success() || body.get("error").is_some() {
        return Err("accountError.network".into());
    }
    token(previous.region, &body, Some(previous))
}

pub(super) fn poll(id: &str) -> Result<Option<Credentials>, String> {
    let pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let attempt = pending
        .as_ref()
        .filter(|attempt| attempt.id == id)
        .ok_or("accountError.cancelled")?;
    validate(attempt)?;
    let result = attempt
        .outcome
        .lock()
        .map_err(|_| "accountError.busy")?
        .clone()
        .transpose();
    result
}

pub(super) async fn cancellable<F, T>(id: &str, operation: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let attempt = PENDING
        .lock()
        .map_err(|_| "accountError.busy")?
        .as_ref()
        .filter(|attempt| attempt.id == id)
        .cloned()
        .ok_or("accountError.cancelled")?;
    validate(&attempt)?;
    tokio::select! {
        _ = attempt.cancel.cancelled() => Err("accountError.cancelled".into()),
        _ = tokio::time::sleep(Duration::from_secs((attempt.expires - chrono::Utc::now().timestamp()).max(0) as u64)) => Err("accountError.expired".into()),
        result = operation => result,
    }
}

pub(super) fn complete<T>(id: &str, save: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let attempt = pending
        .as_ref()
        .filter(|attempt| attempt.id == id)
        .ok_or("accountError.cancelled")?;
    validate(attempt)?;
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

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
