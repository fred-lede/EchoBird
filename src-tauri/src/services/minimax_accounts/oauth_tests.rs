use super::*;
use crate::services::minimax_accounts::test_support::{json, jwt, MockServer};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn device_pkce_login_has_a_positive_control_and_is_cancellable() {
    let _guard = TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(String::new()));
    let expected = seen.clone();
    let count = calls.clone();
    let server = MockServer::start(move |request| {
        let expected = expected.clone(); let count = count.clone();
        async move {
            let fields: std::collections::HashMap<_, _> = url::form_urlencoded::parse(request.body.as_bytes()).into_owned().collect();
            if request.path.ends_with("/device/code") {
                assert_eq!(fields["client_id"], "mcode-public");
                assert_eq!(fields["code_challenge_method"], "S256");
                *expected.lock().unwrap() = fields["code_challenge"].clone();
                json(200, json!({"device_code":"device","user_code":"CODE","verification_uri":"https://account.minimax.cn/verify","expires_in":600,"interval":1}))
            } else {
                count.fetch_add(1, Ordering::SeqCst);
                assert_eq!(fields["device_code"], "device");
                assert_eq!(URL_SAFE_NO_PAD.encode(Sha256::digest(fields["code_verifier"].as_bytes())), *expected.lock().unwrap());
                json(200, json!({"access_token":jwt("user", "new"),"refresh_token":"refresh-new","expires_in":3600}))
            }
        }
    }).await;
    let login = start_at(Region::Cn, &server.url).await.unwrap();
    assert!(login.expires_at <= chrono::Utc::now().timestamp() + 60);
    let credential = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(credential) = poll(&login.login_id).unwrap() {
                break credential;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(credential.user_id, "user");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    complete(&login.login_id, || Ok(())).unwrap();
    let next = start_at(Region::Cn, &server.url).await.unwrap();
    cancel(&next.login_id).unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(poll(&next.login_id).unwrap_err(), "accountError.cancelled");
}

#[tokio::test]
async fn replacement_and_cancelled_late_exchange_cannot_complete_or_save() {
    let _guard = TEST_LOCK.lock().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let notify = started.clone();
    let release = released.clone();
    let server = MockServer::start(move |request| {
        let notify = notify.clone(); let release = release.clone();
        async move {
            if request.path.ends_with("/device/code") { json(200,json!({"device_code":"device","user_code":"CODE","verification_uri":"https://account.minimax.io/verify","expires_in":600,"interval":1})) }
            else { notify.notify_one(); release.notified().await; json(200,json!({"access_token":jwt("user","new"),"refresh_token":"new","expires_in":3600})) }
        }
    }).await;
    let old = start_at(Region::Global, &server.url).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), started.notified())
        .await
        .unwrap();
    let new = start_at(Region::Global, &server.url).await.unwrap();
    released.notify_one();
    let writes = AtomicUsize::new(0);
    assert!(complete(&old.login_id, || {
        writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .is_err());
    cancel(&new.login_id).unwrap();
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn native_user_code_variant_and_denied_and_expired_states_are_handled() {
    let _guard = TEST_LOCK.lock().await;
    let server = MockServer::start(|request| async move {
        if request.path.ends_with("/device/code") { json(200,json!({"user_code":"CODE","verification_url":"https://account.minimax.cn/verify","expired_in":chrono::Utc::now().timestamp_millis()+600_000,"interval":1})) }
        else {
            assert!(request.body.contains("user_code=CODE"));
            json(200,json!({"status":"denied"}))
        }
    }).await;
    let login = start_at(Region::Cn, &server.url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(poll(&login.login_id).unwrap_err(), "accountError.auth");
    cancel(&login.login_id).unwrap();
    let attempt = Arc::new(Attempt {
        id: "expired".into(),
        region: Region::Cn,
        expires: chrono::Utc::now().timestamp() - 1,
        cancel: CancellationToken::new(),
        outcome: Mutex::new(None),
    });
    *PENDING.lock().unwrap() = Some(attempt);
    assert_eq!(poll("expired").unwrap_err(), "accountError.expired");
    cancel("expired").unwrap();
}

#[tokio::test]
async fn refresh_checks_identity_and_preserves_sign_in_on_transient_failure() {
    let old = Credentials {
        region: Region::Cn,
        access_token: jwt("user", "old"),
        refresh_token: "old".into(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        user_id: "profile-id".into(),
        email: "a@fixture.invalid".into(),
        name: None,
    };
    let server = MockServer::start(|request| async move {
        assert!(request.body.contains("grant_type=refresh_token"));
        json(
            200,
            json!({"access_token":jwt("other-user","new"),"refresh_token":"new","expires_in":3600}),
        )
    })
    .await;
    assert_eq!(
        refresh_at(&client().unwrap(), &server.url, &old)
            .await
            .unwrap_err(),
        "accountError.invalidAccount"
    );
    let failure =
        MockServer::start(|_| async { json(503, json!({"error":"temporarily_unavailable"})) })
            .await;
    assert_eq!(
        refresh_at(&client().unwrap(), &failure.url, &old)
            .await
            .unwrap_err(),
        "accountError.network"
    );
    assert_eq!(old.refresh_token, "old");
    let invalid =
        MockServer::start(|_| async { json(400, json!({"error":"invalid_grant"})) }).await;
    assert_eq!(
        refresh_at(&client().unwrap(), &invalid.url, &old)
            .await
            .unwrap_err(),
        "accountError.loginRequired"
    );
}
