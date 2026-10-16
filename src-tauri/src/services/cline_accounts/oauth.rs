use super::{raw_token, text, Credentials, LoginStart, API};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const WORKOS: &str = "https://api.workos.com";
const CLIENT_ID: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
static PENDING: Mutex<Option<Arc<Attempt>>> = Mutex::new(None);

struct Attempt {
    id: String,
    expires: i64,
    cancel: CancellationToken,
    result: Mutex<Option<Result<Credentials, String>>>,
}
pub(super) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("EchoBird/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "accountError.network".into())
}
async fn read_response(
    response: reqwest::Response,
) -> Result<(reqwest::StatusCode, Value), String> {
    let status = response.status();
    let body = match response.json::<Value>().await {
        Ok(body) => body,
        Err(_) if !status.is_success() => Value::Null,
        Err(_) => return Err("accountError.authResponse".into()),
    };
    Ok((status, body))
}
fn data(status: reqwest::StatusCode, body: Value) -> Result<Value, String> {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err("accountError.loginRequired".into());
    }
    if !status.is_success() || body["success"] == false {
        return Err(format!("accountError.network|HTTP {}", status.as_u16()));
    }
    if body.get("success").is_some() {
        if body["success"] != true || body.get("data").is_none() {
            return Err("accountError.authResponse".into());
        }
        return Ok(body["data"].clone());
    }
    Ok(body)
}
pub(super) async fn get(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    c: &Credentials,
) -> Result<Value, String> {
    let response = client
        .get(format!("{base}{path}"))
        .bearer_auth(super::bearer(c))
        .header("accept", "application/json")
        .send()
        .await
        .map_err(|_| "accountError.network")?;
    let (status, body) = read_response(response).await?;
    if path == "/api/v1/users/me/plan"
        && status == reqwest::StatusCode::NOT_FOUND
        && text(&body, "error")
            .is_some_and(|error| error.eq_ignore_ascii_case("No plan history found for user"))
    {
        return Ok(Value::Null);
    }
    data(status, body)
}
pub(super) fn token(body: &Value, previous: Option<&Credentials>) -> Result<Credentials, String> {
    let access = raw_token(text(body, "accessToken").ok_or("accountError.authResponse")?);
    let refresh = text(body, "refreshToken")
        .or_else(|| previous.map(|c| c.refresh_token.as_str()))
        .ok_or("accountError.authResponse")?;
    let expires = text(body, "expiresAt")
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
        .map(|date| date.timestamp_millis())
        .filter(|time| *time > 0)
        .ok_or("accountError.authResponse")?;
    let mut info = body["userInfo"].clone();
    if !info.is_object() {
        return Err("accountError.authResponse".into());
    }
    let user = text(&info, "clineUserId")
        .or_else(|| previous.map(|c| c.user_id.as_str()))
        .ok_or("accountError.authResponse")?
        .to_owned();
    if previous.is_some_and(|c| {
        c.user_id != user
            || text(&info, "subject")
                .zip(text(&c.user_info, "subject"))
                .is_some_and(|(a, b)| a != b)
    }) {
        return Err("accountError.invalidAccount".into());
    }
    let email = text(&info, "email")
        .or_else(|| previous.map(|c| c.email.as_str()))
        .unwrap_or("")
        .to_owned();
    let name = text(&info, "name")
        .map(str::to_owned)
        .or_else(|| previous.and_then(|c| c.name.clone()));
    info["clineUserId"] = json!(user);
    Ok(Credentials {
        access_token: access.into(),
        refresh_token: refresh.into(),
        expires_at: expires,
        user_id: user,
        email,
        name,
        user_info: info,
    })
}
pub(super) async fn refresh(c: &Credentials, base: &str) -> Result<Credentials, String> {
    let response = client()?
        .post(format!("{base}/api/v1/auth/refresh"))
        .json(&json!({"refreshToken":c.refresh_token,"grantType":"refresh_token"}))
        .send()
        .await
        .map_err(|_| "accountError.network")?;
    let (status, body) = read_response(response).await?;
    let error = text(&body, "error").or_else(|| text(&body["error"], "code"));
    if matches!(status.as_u16(), 400 | 401 | 403)
        && error.is_some_and(|error| {
            matches!(error, "invalid_grant" | "invalid_token" | "unauthorized")
        })
    {
        return Err("accountError.loginRequired".into());
    }
    token(&data(status, body)?, Some(c))
}
fn validate(attempt: &Attempt) -> Result<(), String> {
    if attempt.cancel.is_cancelled() {
        return Err("accountError.cancelled".into());
    }
    if chrono::Utc::now().timestamp() >= attempt.expires {
        return Err("accountError.expired".into());
    }
    Ok(())
}
pub(super) async fn start() -> Result<LoginStart, String> {
    start_at(WORKOS, API).await
}
async fn start_at(workos: &str, api: &str) -> Result<LoginStart, String> {
    let attempt = Arc::new(Attempt {
        id: uuid::Uuid::new_v4().to_string(),
        expires: chrono::Utc::now().timestamp() + 60,
        cancel: CancellationToken::new(),
        result: Mutex::new(None),
    });
    if let Some(old) = PENDING
        .lock()
        .map_err(|_| "accountError.busy")?
        .replace(attempt.clone())
    {
        old.cancel.cancel();
    }
    let result = start_device(&attempt, workos, api).await;
    if result.is_err() {
        cancel(&attempt.id)?;
    }
    result
}
async fn start_device(
    attempt: &Arc<Attempt>,
    workos: &str,
    api: &str,
) -> Result<LoginStart, String> {
    let client = client()?;
    let result = async {
        let response = client
            .post(format!("{workos}/user_management/authorize/device"))
            .form(&[("client_id", CLIENT_ID)])
            .send()
            .await
            .map_err(|_| "accountError.network")?;
        read_response(response).await
    };
    let (status, body) = tokio::select! {
        _ = attempt.cancel.cancelled() => return Err("accountError.cancelled".into()),
        _ = tokio::time::sleep(Duration::from_secs((attempt.expires - chrono::Utc::now().timestamp()).max(0) as u64)) => return Err("accountError.expired".into()),
        result = result => result?,
    };
    validate(attempt)?;
    if !status.is_success() {
        return Err("accountError.auth".into());
    }
    let device = text(&body, "device_code")
        .ok_or("accountError.authResponse")?
        .to_owned();
    let user_code = text(&body, "user_code")
        .ok_or("accountError.authResponse")?
        .to_owned();
    let uri = text(&body, "verification_uri_complete")
        .or_else(|| text(&body, "verification_uri"))
        .ok_or("accountError.authResponse")?;
    let parsed = url::Url::parse(uri).map_err(|_| "accountError.authResponse")?;
    let secure = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.host_str().is_some();
    #[cfg(test)]
    let secure = secure || (parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1"));
    if !secure {
        return Err("accountError.authResponse".into());
    }
    let interval = body["interval"].as_u64().filter(|i| *i > 0).unwrap_or(5);
    let expires = attempt.expires.min(
        chrono::Utc::now().timestamp().saturating_add(
            body["expires_in"]
                .as_i64()
                .filter(|i| *i > 0)
                .unwrap_or(300),
        ),
    );
    let result = LoginStart {
        login_id: attempt.id.clone(),
        verification_uri: uri.into(),
        user_code,
        expires_at: expires,
    };
    let attempt = attempt.clone();
    let workos = workos.to_owned();
    let api = api.to_owned();
    tokio::spawn(async move {
        let result = tokio::select! {
            _ = attempt.cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs((expires - chrono::Utc::now().timestamp()).max(0) as u64)) => Err("accountError.expired".into()),
            result = poll_device(&client, &attempt, &workos, &api, &device, interval) => result,
        };
        if !attempt.cancel.is_cancelled() {
            if let Ok(mut slot) = attempt.result.lock() {
                *slot = Some(result);
            }
        }
    });
    Ok(result)
}
async fn poll_device(
    client: &reqwest::Client,
    attempt: &Attempt,
    workos: &str,
    api: &str,
    device: &str,
    mut interval: u64,
) -> Result<Credentials, String> {
    loop {
        validate(attempt)?;
        let response = client
            .post(format!("{workos}/user_management/authenticate"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device),
                ("client_id", CLIENT_ID),
            ])
            .send()
            .await
            .map_err(|_| "accountError.network")?;
        let (status, body) = read_response(response).await?;
        match text(&body, "error") {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = interval.saturating_add(1),
            Some("expired_token") => return Err("accountError.expired".into()),
            Some(_) => return Err("accountError.auth".into()),
            None if status.is_success() => {
                validate(attempt)?;
                let access = text(&body, "access_token").ok_or("accountError.authResponse")?;
                let refresh = text(&body, "refresh_token").ok_or("accountError.authResponse")?;
                let response = client
                    .post(format!("{api}/api/v1/auth/register"))
                    .json(&json!({"accessToken":access,"refreshToken":refresh}))
                    .send()
                    .await
                    .map_err(|_| "accountError.network")?;
                let (status, body) = read_response(response).await?;
                validate(attempt)?;
                return token(&data(status, body)?, None);
            }
            _ => return Err("accountError.auth".into()),
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    }
}
pub(super) fn poll(id: &str) -> Result<Option<Credentials>, String> {
    let pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let attempt = pending
        .as_ref()
        .filter(|a| a.id == id)
        .ok_or("accountError.cancelled")?;
    validate(attempt)?;
    let result = attempt
        .result
        .lock()
        .map_err(|_| "accountError.busy")?
        .clone()
        .transpose();
    result
}
pub(super) fn complete<T>(id: &str, save: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    let attempt = pending
        .as_ref()
        .filter(|a| a.id == id)
        .ok_or("accountError.cancelled")?;
    validate(attempt)?;
    let result = save()?;
    attempt.cancel.cancel();
    *pending = None;
    Ok(result)
}
pub(super) fn cancel(id: &str) -> Result<(), String> {
    let mut pending = PENDING.lock().map_err(|_| "accountError.busy")?;
    if pending.as_ref().is_some_and(|a| a.id == id) {
        if let Some(a) = pending.take() {
            a.cancel.cancel();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
