use super::*;
use axum::{
    body::Bytes, extract::Request, http::StatusCode, response::IntoResponse, routing::any, Router,
};
use serde_json::json;
use std::{collections::VecDeque, sync::Arc};

struct Fixture {
    root: PathBuf,
    store: PathBuf,
    native: PathBuf,
    first: Account,
    second: Account,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("echobird-grok-grade-{}", uuid::Uuid::new_v4()));
        let store = root.join("accounts");
        let native = root.join("native/auth.json");
        let auth = |user: &str| {
            json!({"https://auth.x.ai":{
                "user_id":user, "email":format!("{user}@example.test"), "key":format!("token-{user}"),
                "oidc_issuer":"https://auth.x.ai", "oidc_client_id":"fixture-client",
                "refresh_token":format!("refresh-{user}"), "principal_type":"personal", "principal_id":user,
                "expires_at":"2026-01-01T00:00:00Z"
            }})
        };
        let first = save_at(&store, "https://auth.x.ai", &auth("first")).unwrap();
        let second = save_at(&store, "https://auth.x.ai", &auth("second")).unwrap();
        super::super::cursor_auth::write(&native, &auth("second")).unwrap();
        Self {
            root,
            store,
            native,
            first,
            second,
        }
    }
    fn path(&self, account: &Account) -> PathBuf {
        self.store.join(format!("{}.json", account.id))
    }
    fn saved(&self, account: &Account) -> Saved {
        super::super::cursor_auth::read(&self.path(account)).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

type Calls = Arc<Mutex<Vec<(String, axum::http::HeaderMap, String)>>>;
async fn server(
    responses: Vec<(StatusCode, &'static str)>,
) -> (String, Calls, tokio::task::JoinHandle<()>) {
    let replies = Arc::new(Mutex::new(VecDeque::from(responses)));
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let app = Router::new().fallback(any(move |request: Request| {
        let replies = replies.clone();
        let captured = captured.clone();
        async move {
            let (parts, body) = request.into_parts();
            let body: Bytes = axum::body::to_bytes(body, 4096).await.unwrap();
            captured.lock().unwrap().push((
                format!("{} {}", parts.method, parts.uri),
                parts.headers,
                String::from_utf8(body.to_vec()).unwrap(),
            ));
            let (status, body) = replies.lock().unwrap().pop_front().unwrap();
            (status, [("content-type", "application/json")], body).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, calls, handle)
}

#[tokio::test]
async fn explicit_refresh_queries_the_requested_account_and_keeps_grades_after_switching() {
    let fixture = Fixture::new();
    let (url, calls, handle) = server(vec![
        (
            StatusCode::OK,
            r#"{"subscription_tier_display":" SuperGrok Plus "}"#,
        ),
        (StatusCode::OK, r#"{"subscription_tier_display":"Free"}"#),
    ])
    .await;
    let first = refresh_at(
        &fixture.path(&fixture.first),
        &fixture.native,
        &format!("{url}/settings"),
        &url,
    )
    .await
    .unwrap();
    assert_eq!(first.plan.as_deref(), Some("SuperGrok Plus"));
    assert!(!first.active);
    assert!(fixture.saved(&fixture.second).summary.plan.is_none());
    let second = refresh_at(
        &fixture.path(&fixture.second),
        &fixture.native,
        &format!("{url}/settings"),
        &url,
    )
    .await
    .unwrap();
    assert_eq!(second.plan.as_deref(), Some("Free"));
    assert!(second.active);
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    for (index, user) in ["first", "second"].iter().enumerate() {
        assert_eq!(recorded[index].0, "GET /settings");
        assert_eq!(
            recorded[index].1["authorization"],
            format!("Bearer token-{user}")
        );
        assert_eq!(recorded[index].1["x-userid"], *user);
        assert_eq!(recorded[index].1["x-email"], format!("{user}@example.test"));
        assert_eq!(recorded[index].1["x-xai-token-auth"], "xai-grok-cli");
    }
    drop(recorded);
    handle.abort();
    // Passive listing never contacts the now-stopped server and never adopts a foreign native tier.
    fs::write(fixture.native.parent().unwrap().join("settings_cache.json"),
        json!({"payload":json!({"settings":{"subscription_tier_display":"Foreign plan"}}).to_string()}).to_string()).unwrap();
    let first_saved = fixture.saved(&fixture.first);
    storage::apply_at(
        &fixture.store,
        &fixture.native,
        &fixture.native.parent().unwrap().join("settings_cache.json"),
        &first_saved,
    )
    .unwrap();
    let rows = list_at(&fixture.store, &fixture.native).unwrap();
    assert!(rows
        .iter()
        .any(|a| a.id == first.id && a.active && a.plan == first.plan));
    assert!(rows
        .iter()
        .any(|a| a.id == second.id && !a.active && a.plan == second.plan));
    // Re-adding the same login preserves its previously fetched tier.
    let auth = json!({first_saved.key.clone():first_saved.auth});
    assert_eq!(
        save_at(&fixture.store, &first_saved.key, &auth)
            .unwrap()
            .plan,
        first.plan
    );
}

#[tokio::test]
async fn refresh_errors_preserve_cached_grade_and_missing_data_is_never_free() {
    let fixture = Fixture::new();
    let mut saved = fixture.saved(&fixture.first);
    saved.summary.plan = Some("SuperGrok".into());
    super::super::cursor_auth::write(&fixture.path(&fixture.first), &saved).unwrap();
    for (status, body, expected) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "not json",
            "accountError.network",
        ),
        (StatusCode::TOO_MANY_REQUESTS, "{}", "accountError.network"),
        (StatusCode::FORBIDDEN, "{}", "accountError.auth"),
        (StatusCode::OK, "not json", "accountError.format"),
        (StatusCode::OK, "{}", "accountError.format"),
        (
            StatusCode::OK,
            r#"{"subscription_tier_display":" "}"#,
            "accountError.format",
        ),
    ] {
        let before = fs::read(fixture.path(&fixture.first)).unwrap();
        let native_before = fs::read(&fixture.native).unwrap();
        let (url, _, handle) = server(vec![(status, body)]).await;
        assert_eq!(
            refresh_at(&fixture.path(&fixture.first), &fixture.native, &url, &url)
                .await
                .unwrap_err(),
            expected
        );
        handle.abort();
        assert_eq!(fs::read(fixture.path(&fixture.first)).unwrap(), before);
        assert_eq!(fs::read(&fixture.native).unwrap(), native_before);
        assert!(fixture.saved(&fixture.second).summary.plan.is_none());
    }
}

#[tokio::test]
async fn expired_login_renews_once_and_preserves_rotation_when_settings_fail() {
    let fixture = Fixture::new();
    let native_before = fs::read(&fixture.native).unwrap();
    let (url, calls, handle) = server(vec![
        (StatusCode::UNAUTHORIZED, "{}"),
        (
            StatusCode::OK,
            r#"{"access_token":"new-token","refresh_token":"new-refresh","expires_in":3600}"#,
        ),
        (StatusCode::SERVICE_UNAVAILABLE, "{}"),
    ])
    .await;
    assert_eq!(
        refresh_at(
            &fixture.path(&fixture.first),
            &fixture.native,
            &format!("{url}/settings"),
            &format!("{url}/token")
        )
        .await
        .unwrap_err(),
        "accountError.network"
    );
    handle.abort();
    let saved = fixture.saved(&fixture.first);
    assert_eq!(saved.auth["key"], "new-token");
    assert_eq!(saved.auth["refresh_token"], "new-refresh");
    assert!(saved.summary.plan.is_none());
    assert_eq!(fs::read(&fixture.native).unwrap(), native_before);
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[1].0, "POST /token");
    for field in [
        "grant_type=refresh_token",
        "refresh_token=refresh-first",
        "client_id=fixture-client",
        "principal_type=personal",
        "principal_id=first",
    ] {
        assert!(recorded[1].2.contains(field));
    }
    assert_eq!(recorded[2].1["authorization"], "Bearer new-token");
    // An older native login for this user must not roll back the newly rotated credentials.
    super::super::cursor_auth::write(
        &fixture.native,
        &json!({saved.key.clone():{
            "user_id":"first", "email":"first@example.test", "key":"old-native",
            "expires_at":"2026-01-01T00:00:00Z"
        }}),
    )
    .unwrap();
    list_at(&fixture.store, &fixture.native).unwrap();
    assert_eq!(fixture.saved(&fixture.first).auth["key"], "new-token");
}

#[tokio::test]
async fn active_token_renewal_updates_only_its_native_entry_and_returns_real_tier() {
    let fixture = Fixture::new();
    let (url, _, handle) = server(vec![
        (StatusCode::UNAUTHORIZED, "{}"),
        (
            StatusCode::OK,
            r#"{"access_token":"new-token","expires_in":3600}"#,
        ),
        (StatusCode::OK, r#"{"subscription_tier_display":"Free"}"#),
    ])
    .await;
    let result = refresh_at(&fixture.path(&fixture.second), &fixture.native, &url, &url)
        .await
        .unwrap();
    handle.abort();
    assert!(result.active);
    assert_eq!(result.plan.as_deref(), Some("Free"));
    let native: Value = super::super::cursor_auth::read(&fixture.native).unwrap();
    assert_eq!(native["https://auth.x.ai"]["key"], "new-token");
    assert_eq!(
        native["https://auth.x.ai"]["refresh_token"],
        "refresh-second"
    );
    assert_eq!(fixture.saved(&fixture.first).auth["key"], "token-first");
}

#[tokio::test]
async fn deleting_an_account_during_refresh_does_not_recreate_it() {
    let fixture = Fixture::new();
    let path = fixture.path(&fixture.first);
    let removed = path.clone();
    let app = Router::new().fallback(any(move || {
        let removed = removed.clone();
        async move {
            fs::remove_file(removed).unwrap();
            axum::Json(json!({"subscription_tier_display":"Free"}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    assert!(refresh_at(&path, &fixture.native, &url, &url)
        .await
        .is_err());
    handle.abort();
    assert!(!path.exists());
    assert!(fixture.path(&fixture.second).exists());
}

#[tokio::test]
async fn renewal_failures_keep_both_the_cached_tier_and_native_credentials() {
    let fixture = Fixture::new();
    let mut saved = fixture.saved(&fixture.second);
    saved.summary.plan = Some("SuperGrok".into());
    super::super::cursor_auth::write(&fixture.path(&fixture.second), &saved).unwrap();
    for (status, body, expected) in [
        (
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant"}"#,
            "accountError.auth",
        ),
        (
            StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_client"}"#,
            "accountError.auth",
        ),
        (
            StatusCode::BAD_REQUEST,
            r#"{"error":"temporarily_unavailable"}"#,
            "accountError.network",
        ),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "not json",
            "accountError.network",
        ),
        (StatusCode::OK, "{}", "accountError.format"),
    ] {
        let native_before = fs::read(&fixture.native).unwrap();
        let (url, calls, handle) =
            server(vec![(StatusCode::UNAUTHORIZED, "{}"), (status, body)]).await;
        assert_eq!(
            refresh_at(&fixture.path(&fixture.second), &fixture.native, &url, &url)
                .await
                .unwrap_err(),
            expected
        );
        handle.abort();
        assert_eq!(calls.lock().unwrap().len(), 2);
        assert_eq!(fs::read(&fixture.native).unwrap(), native_before);
        let unchanged = fixture.saved(&fixture.second);
        assert_eq!(unchanged.auth, saved.auth);
        assert_eq!(unchanged.summary.plan, saved.summary.plan);
    }
}
