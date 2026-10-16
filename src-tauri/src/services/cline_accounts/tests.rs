use super::*;
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use axum::{
    extract::State,
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use rand::RngCore;
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

pub(super) fn encrypt(raw: &str) -> String {
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&[43u8; 32]).unwrap();
    let encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), raw.as_bytes())
        .unwrap();
    format!("enc:v1:{}{}", hex::encode(nonce), hex::encode(encrypted))
}

pub(super) fn decrypt(encoded: &str) -> String {
    let Some(bytes) = encoded
        .strip_prefix("enc:v1:")
        .and_then(|value| hex::decode(value).ok())
    else {
        return String::new();
    };
    if bytes.len() < 28 {
        return String::new();
    }
    let cipher = Aes256Gcm::new_from_slice(&[43u8; 32]).unwrap();
    cipher
        .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok())
        .unwrap_or_default()
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("echobird-cline-account-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("accounts.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn credential(user: &str, suffix: &str, expires_at: i64) -> Credentials {
    Credentials {
        access_token: format!("{suffix}-{user}"),
        refresh_token: format!("{user}:{suffix}"),
        expires_at,
        user_id: user.into(),
        email: format!("{user}@example.test"),
        name: Some(format!("User {user}")),
        user_info: json!({"clineUserId":user,"subject":format!("subject-{user}"),"email":format!("{user}@example.test")}),
    }
}
fn fresh(user: &str) -> Credentials {
    credential(
        user,
        "old",
        chrono::Utc::now().timestamp_millis() + 3_600_000,
    )
}

fn native_state(c: &Credentials) -> Value {
    json!({"version":1,"modes":{},"lastUsedProvider":"cline","providers":{"cline":{"settings":{"provider":"cline",
        "auth":{"accessToken":bearer(c),"refreshToken":c.refresh_token,"accountId":c.user_id,"expiresAt":c.expires_at,
        "metadata":{"userInfo":c.user_info}}}}}})
}

async fn apply_fixture(path: &Path, native_path: &Path, c: &Credentials) {
    let mut native = super::super::cline_native::RefreshSession::open_at(native_path)
        .await
        .unwrap();
    let apply = super::super::cline_native::begin_apply_at(path, &mut native, c, None)
        .await
        .unwrap();
    mark_active_at(path, &account_id(c).unwrap(), c).unwrap();
    apply.commit();
}

#[tokio::test]
async fn switching_away_preserves_the_outgoing_accounts_native_rotation() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let first = fresh("one");
    let second = fresh("two");
    let first_account = capture(&path, &first).unwrap();
    capture(&path, &second).unwrap();
    let native_path = fixture.0.join("providers.json");
    fs::write(&native_path, native_state(&first).to_string()).unwrap();
    apply_fixture(&path, &native_path, &first).await;
    assert!(capture(&path, &first).unwrap().active);
    assert_eq!(
        load(&path).unwrap()[&first_account.id]
            .native_pair
            .as_deref(),
        Some(pair(&first).as_str())
    );
    let renewed = credential("one", "native-new", first.expires_at - 120_000);
    fs::write(&native_path, native_state(&renewed).to_string()).unwrap();

    apply_fixture(&path, &native_path, &second).await;

    let stored = selected(&load(&path).unwrap(), &first_account.id).unwrap();
    assert_eq!(stored.access_token, renewed.access_token);
    assert_eq!(stored.refresh_token, renewed.refresh_token);
    assert!(!load(&path).unwrap()[&first_account.id].account.active);
    apply_fixture(&path, &native_path, &stored).await;
    let native: Value = serde_json::from_slice(&fs::read(&native_path).unwrap()).unwrap();
    assert_eq!(
        native["providers"]["cline"]["settings"]["auth"]["refreshToken"],
        renewed.refresh_token
    );
}

#[tokio::test]
async fn applying_another_account_flushes_a_pending_pair_without_readopting_its_native_baseline() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let first = fresh("one");
    let second = fresh("two");
    let account = capture(&path, &first).unwrap();
    capture(&path, &second).unwrap();
    mark_active_at(&path, &account.id, &first).unwrap();
    let baseline = load(&path).unwrap()[&account.id].native_pair.clone();
    let renewed = credential("one", "own-new", first.expires_at);
    rotations()
        .lock()
        .unwrap()
        .insert(key(&path, &account.id), (first.clone(), renewed.clone()));
    let native_path = fixture.0.join("providers.json");
    fs::write(&native_path, native_state(&first).to_string()).unwrap();

    let mut native = super::super::cline_native::RefreshSession::open_at(&native_path)
        .await
        .unwrap();
    let apply = super::super::cline_native::begin_apply_at(&path, &mut native, &second, None)
        .await
        .unwrap();
    assert!(selected(&load(&path).unwrap(), &account.id).unwrap() == renewed);
    assert_eq!(load(&path).unwrap()[&account.id].native_pair, baseline);
    assert!(!rotations()
        .lock()
        .unwrap()
        .contains_key(&key(&path, &account.id)));
    // A failed selection restores native state while the captured valid pair remains saved.
    drop(apply);
    let restored: Value = serde_json::from_slice(&fs::read(&native_path).unwrap()).unwrap();
    assert_eq!(restored, native_state(&first));
    assert!(load(&path).unwrap()[&account.id].account.active);
    drop(native);
    apply_fixture(&path, &native_path, &second).await;
    assert!(selected(&load(&path).unwrap(), &account.id).unwrap() == renewed);
    apply_fixture(&path, &native_path, &renewed).await;
    let updated: Value = serde_json::from_slice(&fs::read(&native_path).unwrap()).unwrap();
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"]["refreshToken"],
        renewed.refresh_token
    );
}

#[tokio::test]
async fn applying_another_account_preserves_an_independent_unbound_login() {
    for ambiguous_active in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let original = fresh("one");
        let independent = credential("one", "independent-login", original.expires_at);
        let second = fresh("two");
        let account = capture(&path, &original).unwrap();
        mark_active_at(&path, &account.id, &original).unwrap();
        let replacement = capture(&path, &independent).unwrap();
        assert!(!replacement.active);
        if ambiguous_active {
            let mut store = load(&path).unwrap();
            store.get_mut(&account.id).unwrap().account.active = true;
            write(&path, &store).unwrap();
        }
        capture(&path, &second).unwrap();
        let native_pair = credential("one", "native-new", independent.expires_at);
        let native_path = fixture.0.join("providers.json");
        fs::write(&native_path, native_state(&native_pair).to_string()).unwrap();

        apply_fixture(&path, &native_path, &second).await;

        assert!(selected(&load(&path).unwrap(), &account.id).unwrap() == independent);
        assert!(load(&path).unwrap()[&account.id].native_pair.is_none());
    }
}

#[tokio::test]
async fn invalid_outgoing_native_credentials_do_not_change_either_account_store() {
    for field in ["accessToken", "subject"] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let first = fresh("one");
        let second = fresh("two");
        let account = capture(&path, &first).unwrap();
        capture(&path, &second).unwrap();
        mark_active_at(&path, &account.id, &first).unwrap();
        let mut invalid = native_state(&first);
        let auth = &mut invalid["providers"]["cline"]["settings"]["auth"];
        if field == "accessToken" {
            auth[field] = json!("workos:");
        } else {
            auth["metadata"]["userInfo"][field] = json!("another-subject");
        }
        let native_path = fixture.0.join("providers.json");
        fs::write(&native_path, invalid.to_string()).unwrap();
        let own_before = fs::read(&path).unwrap();
        let native_before = fs::read(&native_path).unwrap();
        let mut native = super::super::cline_native::RefreshSession::open_at(&native_path)
            .await
            .unwrap();

        assert!(
            super::super::cline_native::begin_apply_at(&path, &mut native, &second, None)
                .await
                .is_err()
        );

        assert_eq!(fs::read(&path).unwrap(), own_before);
        assert_eq!(fs::read(&native_path).unwrap(), native_before);
    }
}

#[cfg(windows)]
#[tokio::test]
async fn failed_outgoing_capture_preserves_native_state_and_retries_its_pending_pair() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    let path = fixture.path();
    let first = fresh("one");
    let second = fresh("two");
    let account = capture(&path, &first).unwrap();
    capture(&path, &second).unwrap();
    mark_active_at(&path, &account.id, &first).unwrap();
    let renewed = credential("one", "native-new", first.expires_at);
    let native_path = fixture.0.join("providers.json");
    fs::write(&native_path, native_state(&renewed).to_string()).unwrap();
    let own_before = fs::read(&path).unwrap();
    let native_before = fs::read(&native_path).unwrap();
    let holder = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let mut native = super::super::cline_native::RefreshSession::open_at(&native_path)
        .await
        .unwrap();
    assert!(
        super::super::cline_native::begin_apply_at(&path, &mut native, &second, None)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), own_before);
    assert_eq!(fs::read(&native_path).unwrap(), native_before);
    assert!(rotations()
        .lock()
        .unwrap()
        .contains_key(&key(&path, &account.id)));
    drop(holder);

    let apply = super::super::cline_native::begin_apply_at(&path, &mut native, &second, None)
        .await
        .unwrap();
    mark_active_at(&path, &account_id(&second).unwrap(), &second).unwrap();
    apply.commit();

    assert!(selected(&load(&path).unwrap(), &account.id).unwrap() == renewed);
    assert!(!rotations()
        .lock()
        .unwrap()
        .contains_key(&key(&path, &account.id)));
}

#[derive(Default)]
struct Api {
    refreshes: AtomicUsize,
    profiles: AtomicUsize,
    balances: AtomicUsize,
    plans: AtomicUsize,
    active: AtomicUsize,
    peak: AtomicUsize,
    requests: Mutex<Vec<(String, String, Value)>>,
    fail_old_profile: bool,
    wrong_profile: bool,
    null_plan: bool,
    plan_status: u16,
    plan_error: Option<String>,
    delay_refresh: bool,
    block_refresh: bool,
    started: tokio::sync::Notify,
    released: tokio::sync::Notify,
    block_store: Mutex<Option<PathBuf>>,
    replace_login: Mutex<Option<(PathBuf, Credentials)>>,
}

fn authorization(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .into()
}
async fn refresh_endpoint(State(api): State<Arc<Api>>, Json(body): Json<Value>) -> Json<Value> {
    api.refreshes.fetch_add(1, Ordering::SeqCst);
    let active = api.active.fetch_add(1, Ordering::SeqCst) + 1;
    api.peak.fetch_max(active, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("refresh".into(), String::new(), body.clone()));
    if api.delay_refresh {
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    if api.block_refresh {
        api.started.notify_one();
        api.released.notified().await;
    }
    if let Some(path) = api.block_store.lock().unwrap().take() {
        fs::rename(&path, path.with_extension("backup")).unwrap();
        fs::create_dir(path).unwrap();
    }
    if let Some((path, c)) = api.replace_login.lock().unwrap().take() {
        capture(&path, &c).unwrap();
    }
    api.active.fetch_sub(1, Ordering::SeqCst);
    let user = body["refreshToken"]
        .as_str()
        .unwrap()
        .split(':')
        .next()
        .unwrap();
    Json(
        json!({"accessToken":format!("workos:new-{user}"),"refreshToken":format!("{user}:new"),
        "expiresAt":(chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
        "userInfo":{"clineUserId":user,"subject":format!("subject-{user}"),"email":format!("{user}@example.test")}}),
    )
}
async fn profile_endpoint(
    State(api): State<Arc<Api>>,
    headers: HeaderMap,
) -> (axum::http::StatusCode, Json<Value>) {
    api.profiles.fetch_add(1, Ordering::SeqCst);
    let authorization = authorization(&headers);
    api.requests
        .lock()
        .unwrap()
        .push(("profile".into(), authorization.clone(), Value::Null));
    if api.fail_old_profile && authorization.contains("old-") {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(
            json!({"id":if api.wrong_profile { "other" } else { "user" },
        "email":"updated@example.test","displayName":"Updated user"}),
        ),
    )
}
async fn balance_endpoint(State(api): State<Arc<Api>>, headers: HeaderMap) -> Json<Value> {
    api.balances.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("balance".into(), authorization(&headers), Value::Null));
    Json(json!({"success":true,"data":{"userId":"user","balance":1_250_000}}))
}
async fn plan_endpoint(
    State(api): State<Arc<Api>>,
    headers: HeaderMap,
) -> (axum::http::StatusCode, Json<Value>) {
    api.plans.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("plan".into(), authorization(&headers), Value::Null));
    if api.plan_status != 0 {
        return (
            axum::http::StatusCode::from_u16(api.plan_status).unwrap(),
            Json(json!({"success":false,"error":api.plan_error,"data":null})),
        );
    }
    (
        axum::http::StatusCode::OK,
        Json(if api.null_plan {
            json!({"success":true,"data":null})
        } else {
            json!({"plan":{"name":"cline-pass","displayName":"Cline Pass"},"currentPeriodEnd":"2030-01-02T03:04:05Z"})
        }),
    )
}

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(api: Arc<Api>) -> Self {
        let app = Router::new()
            .route("/api/v1/auth/refresh", post(refresh_endpoint))
            .route("/api/v1/users/me", get(profile_endpoint))
            .route("/api/v1/users/user/balance", get(balance_endpoint))
            .route("/api/v1/users/me/plan", get(plan_endpoint))
            .with_state(api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { base, task }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[test]
fn passive_list_returns_metadata_without_decoding_credentials_or_touching_other_files() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = fresh("user");
    let account = summary(&c).unwrap();
    let store = BTreeMap::from([(
        account.id.clone(),
        Saved {
            account: account.clone(),
            credential: "corrupt-private-credential".into(),
            native_pair: None,
        },
    )]);
    write(&path, &store).unwrap();
    let before = fs::read(&path).unwrap();
    let native = fixture.0.join("providers.json");
    fs::write(&native, b"native config deliberately invalid").unwrap();
    let rows = list_at(&path).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, account.id);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(&native).unwrap(),
        b"native config deliberately invalid"
    );
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
    assert!(list_at(&fixture.0.join("missing.json")).unwrap().is_empty());
    assert!(!fixture.0.join("missing.json").exists());
}

#[test]
fn credential_store_is_encrypted_identity_bound_and_dto_exposes_only_summary() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = fresh("user");
    let account = capture(&path, &c).unwrap();
    let encoded = fs::read_to_string(&path).unwrap();
    assert!(encoded.contains("enc:v1:"));
    for secret in [&c.access_token, &c.refresh_token, "subject-user"] {
        assert!(!encoded.contains(secret));
    }
    let store = load(&path).unwrap();
    assert!(selected(&store, &account.id).unwrap() == c);
    let mut next = c.clone();
    next.access_token = "workos:rotated-user".into();
    next.refresh_token = "rotated-refresh".into();
    assert_eq!(account_id(&next).unwrap(), account.id);
    assert_ne!(pair(&next), pair(&c));
    let frontend = serde_json::to_string(&account).unwrap();
    for private in [
        "accessToken",
        "refreshToken",
        "userInfo",
        "credential",
        "nativePair",
    ] {
        assert!(!frontend.contains(private));
    }
    assert_eq!(
        proxy_value(&next),
        json!({"accessToken":"workos:rotated-user","userId":"user","credentialGeneration":pair(&next)})
    );
    assert_eq!(bearer(&next), "workos:rotated-user");
    let mut wrong = c.clone();
    wrong.user_info["clineUserId"] = json!("other");
    assert!(account_id(&wrong).is_err());
    let mut store = store;
    store.get_mut(&account.id).unwrap().credential = protect(&fresh("other")).unwrap();
    assert_eq!(
        selected(&store, &account.id).err().unwrap(),
        "accountError.invalidAccount"
    );
}

#[test]
fn invalid_or_tampered_ciphertext_cannot_be_used_as_an_account() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let account = capture(&path, &fresh("user")).unwrap();
    for encoded in [
        "plaintext-token".into(),
        "enc:v1:00".into(),
        format!("enc:v1:{}", "00".repeat(80)),
    ] {
        let mut store = load(&path).unwrap();
        store.get_mut(&account.id).unwrap().credential = encoded;
        assert_eq!(
            selected(&store, &account.id).err().unwrap(),
            "accountError.keychain"
        );
    }
}

#[tokio::test]
async fn explicit_refresh_retries_401_once_and_retains_real_microdollar_and_subscription_values() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = fresh("user");
    let account = capture(&path, &c).unwrap();
    let api = Arc::new(Api {
        fail_old_profile: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let mut session = session_at(&path, &account.id, false, None, &server.base, None)
        .await
        .unwrap();
    assert_eq!(api.profiles.load(Ordering::SeqCst), 0);
    let refreshed = refresh_at(&path, &account.id, &mut session, &server.base)
        .await
        .unwrap();
    assert_eq!(refreshed.balance_micros, Some(1_250_000));
    assert_eq!(refreshed.balance_micros.unwrap() as f64 / 1_000_000.0, 1.25);
    assert_eq!(refreshed.plan.as_deref(), Some("Cline Pass"));
    assert_eq!(refreshed.subscription_end_at, Some(1_893_553_445));
    assert_eq!(refreshed.email, "updated@example.test");
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(api.profiles.load(Ordering::SeqCst), 2);
    assert_eq!(api.balances.load(Ordering::SeqCst), 1);
    assert_eq!(api.plans.load(Ordering::SeqCst), 1);
    let requests = api.requests.lock().unwrap();
    assert!(requests
        .iter()
        .any(|(kind, auth, _)| kind == "profile" && auth == "Bearer workos:old-user"));
    for (kind, auth, _) in requests
        .iter()
        .filter(|(kind, _, _)| kind == "balance" || kind == "plan")
    {
        assert_eq!(auth, "Bearer workos:new-user", "{kind}");
    }
    assert_eq!(
        requests
            .iter()
            .find(|(kind, _, _)| kind == "refresh")
            .unwrap()
            .2,
        json!({"refreshToken":"user:old","grantType":"refresh_token"})
    );
}

#[tokio::test]
async fn confirmed_absent_subscription_keeps_balance_but_other_plan_failures_remain_visible() {
    for (status, error, absent) in [
        (404, "No plan history found for user", true),
        (404, "Route not found", false),
        (401, "No plan history found for user", false),
        (500, "No plan history found for user", false),
    ] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let account = capture(&path, &fresh("user")).unwrap();
        let mut store = load(&path).unwrap();
        let saved = store.get_mut(&account.id).unwrap();
        saved.account.balance_micros = Some(500_000);
        saved.account.plan = Some("Previous plan".into());
        saved.account.subscription_end_at = Some(1_893_553_445);
        write(&path, &store).unwrap();
        let before = fs::read(&path).unwrap();
        let api = Arc::new(Api {
            plan_status: status,
            plan_error: Some(error.into()),
            ..Api::default()
        });
        let server = Server::new(api.clone()).await;
        let mut session = session_at(&path, &account.id, false, None, &server.base, None)
            .await
            .unwrap();
        let result = refresh_at(&path, &account.id, &mut session, &server.base).await;
        if absent {
            let updated = result.unwrap();
            assert_eq!(updated.balance_micros, Some(1_250_000));
            assert!(updated.plan.is_none());
            assert!(updated.subscription_end_at.is_none());
            assert_eq!(api.refreshes.load(Ordering::SeqCst), 0);
        } else {
            assert!(result.is_err(), "unexpectedly hid HTTP {status}");
            let store = load(&path).unwrap();
            assert_eq!(store[&account.id].account.balance_micros, Some(500_000));
            assert_eq!(
                store[&account.id].account.plan.as_deref(),
                Some("Previous plan")
            );
            if status != 401 {
                assert_eq!(fs::read(&path).unwrap(), before);
            }
        }
    }
}

#[tokio::test]
async fn null_plan_is_unknown_and_wrong_profile_identity_preserves_cached_metadata() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let account = capture(&path, &fresh("user")).unwrap();
    let server = Server::new(Arc::new(Api {
        null_plan: true,
        ..Api::default()
    }))
    .await;
    {
        let mut session = session_at(&path, &account.id, false, None, &server.base, None)
            .await
            .unwrap();
        let updated = refresh_at(&path, &account.id, &mut session, &server.base)
            .await
            .unwrap();
        assert!(updated.plan.is_none());
        assert!(updated.subscription_end_at.is_none());
        assert_eq!(updated.balance_micros, Some(1_250_000));
    }
    let before = fs::read(&path).unwrap();
    let api = Arc::new(Api {
        wrong_profile: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let mut session = session_at(&path, &account.id, false, None, &server.base, None)
        .await
        .unwrap();
    assert_eq!(
        refresh_at(&path, &account.id, &mut session, &server.base)
            .await
            .err()
            .unwrap(),
        "accountError.invalidAccount"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(api.balances.load(Ordering::SeqCst), 0);
    assert_eq!(api.plans.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn same_account_rejected_requests_refresh_once_while_different_accounts_can_rotate_together()
{
    let fixture = Fixture::new();
    let path = fixture.path();
    let account = capture(&path, &fresh("user")).unwrap();
    let api = Arc::new(Api {
        delay_refresh: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let renew = || async {
        let session = session_at(
            &path,
            &account.id,
            true,
            Some("workos:old-user"),
            &server.base,
            None,
        )
        .await
        .unwrap();
        session.credentials.access_token.clone()
    };
    let (a, b) = tokio::join!(renew(), renew());
    assert_eq!(a, "new-user");
    assert_eq!(a, b);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    let one = capture(&path, &fresh("one")).unwrap();
    let two = capture(&path, &fresh("two")).unwrap();
    let renew = |id: String, rejected: &'static str| {
        let path = path.clone();
        let base = server.base.clone();
        async move {
            session_at(&path, &id, true, Some(rejected), &base, None)
                .await
                .unwrap()
                .credentials
                .access_token
                .clone()
        }
    };
    let (a, b) = tokio::join!(renew(one.id, "old-one"), renew(two.id, "old-two"));
    assert_eq!(a, "new-one");
    assert_eq!(b, "new-two");
    assert_eq!(api.peak.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_rotated_pair_survives_failed_storage_and_retry_does_not_reuse_the_old_grant() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let account = capture(&path, &fresh("user")).unwrap();
    let api = Arc::new(Api {
        block_store: Mutex::new(Some(path.clone())),
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    assert!(session_at(
        &path,
        &account.id,
        true,
        Some("old-user"),
        &server.base,
        None
    )
    .await
    .is_err());
    assert!(path.is_dir());
    fs::remove_dir(&path).unwrap();
    fs::rename(path.with_extension("backup"), &path).unwrap();
    let session = session_at(
        &path,
        &account.id,
        true,
        Some("old-user"),
        &server.base,
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.credentials.access_token, "new-user");
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    assert_eq!(
        selected(&load(&path).unwrap(), &account.id)
            .unwrap()
            .refresh_token,
        "user:new"
    );
}

#[tokio::test]
async fn a_login_replacing_the_same_identity_during_rotation_cannot_be_overwritten() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = fresh("user");
    let account = capture(&path, &c).unwrap();
    let newer = credential("user", "login", c.expires_at - 120_000);
    let api = Arc::new(Api {
        replace_login: Mutex::new(Some((path.clone(), newer.clone()))),
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    assert!(session_at(
        &path,
        &account.id,
        true,
        Some("old-user"),
        &server.base,
        None
    )
    .await
    .is_err());
    assert!(selected(&load(&path).unwrap(), &account.id).unwrap() == newer);
    let session = session_at(
        &path,
        &account.id,
        true,
        Some("old-user"),
        &server.base,
        None,
    )
    .await
    .unwrap();
    assert!(session.credentials == newer);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_existing_native_sqlite_lock_blocks_rotation_until_released() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = credential("user", "old", chrono::Utc::now().timestamp_millis() - 1000);
    let account = capture(&path, &c).unwrap();
    mark_active_at(&path, &account.id, &c).unwrap();
    let native = fixture.0.join("providers.json");
    fs::write(&native, serde_json::to_vec(&json!({"version":1,"modes":{},"providers":{"cline":{"settings":{"provider":"cline",
        "auth":{"accessToken":"workos:old-user","refreshToken":"user:old","accountId":"user","expiresAt":c.expires_at,
        "metadata":{"userInfo":c.user_info}}}}}})).unwrap()).unwrap();
    let native_lock = PathBuf::from(format!(
        "{}.oauth-{:x}.lock",
        native.display(),
        Sha256::digest(b"cline")
    ));
    let holder = rusqlite::Connection::open(&native_lock).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();
    let api = Arc::new(Api::default());
    let server = Server::new(api.clone()).await;
    let mut waiting = Box::pin(session_at(
        &path,
        &account.id,
        false,
        None,
        &server.base,
        Some(&native),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 0);
    holder.execute_batch("ROLLBACK;").unwrap();
    let session = waiting.await.unwrap();
    assert_eq!(session.credentials.access_token, "new-user");
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    assert!(native_lock.exists());
    let native: Value = serde_json::from_slice(&fs::read(&native).unwrap()).unwrap();
    assert_eq!(
        native["providers"]["cline"]["settings"]["auth"]["refreshToken"],
        "user:new"
    );
    assert_eq!(
        load(&path).unwrap()[&account.id].native_pair.as_deref(),
        Some(pair(&session.credentials).as_str())
    );
}

#[test]
fn active_selection_is_written_only_for_a_matching_saved_pair() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let a = fresh("one");
    let b = fresh("two");
    let first = capture(&path, &a).unwrap();
    let second = capture(&path, &b).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(mark_active_at(&path, &first.id, &b).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    mark_active_at(&path, &second.id, &b).unwrap();
    let store = load(&path).unwrap();
    assert!(!store[&first.id].account.active);
    assert!(store[&second.id].account.active);
    assert!(store[&first.id].native_pair.is_none());
    assert_eq!(
        store[&second.id].native_pair.as_deref(),
        Some(pair(&b).as_str())
    );
    let recaptured = capture(&path, &b).unwrap();
    assert!(recaptured.active);
    assert_eq!(
        load(&path).unwrap()[&second.id].native_pair.as_deref(),
        Some(pair(&b).as_str())
    );
    let independent = credential("two", "another-login", b.expires_at);
    assert!(!capture(&path, &independent).unwrap().active);
    assert!(load(&path).unwrap()[&second.id].native_pair.is_none());
}

#[tokio::test]
async fn an_aborted_caller_does_not_lose_a_single_use_rotation_or_leave_native_locks_held() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = credential("user", "old", chrono::Utc::now().timestamp_millis() - 1000);
    let account = capture(&path, &c).unwrap();
    mark_active_at(&path, &account.id, &c).unwrap();
    let native = fixture.0.join("providers.json");
    fs::write(&native, serde_json::to_vec(&json!({"version":1,"modes":{},"providers":{"cline":{"settings":{"provider":"cline",
        "auth":{"accessToken":"workos:old-user","refreshToken":"user:old","accountId":"user","expiresAt":c.expires_at,
        "metadata":{"userInfo":c.user_info}}}}}})).unwrap()).unwrap();
    let api = Arc::new(Api {
        block_refresh: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let mut request = Box::pin(session_at(
        &path,
        &account.id,
        false,
        None,
        &server.base,
        Some(&native),
    ));
    tokio::select! {
        _ = api.started.notified() => {},
        _ = &mut request => panic!("refresh must wait for the fixture response"),
        _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("refresh did not reach the controlled upstream"),
    }
    drop(request);
    api.released.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if selected(&load(&path).unwrap(), &account.id)
                .unwrap()
                .access_token
                == "new-user"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let next = tokio::time::timeout(
        Duration::from_secs(2),
        session_at(&path, &account.id, false, None, &server.base, Some(&native)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next.credentials.refresh_token, "user:new");
    drop(next);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    let lock = PathBuf::from(format!(
        "{}.oauth-{:x}.lock",
        native.display(),
        Sha256::digest(b"cline")
    ));
    let probe = rusqlite::Connection::open(&lock).unwrap();
    probe
        .execute_batch("PRAGMA busy_timeout = 0; BEGIN EXCLUSIVE; ROLLBACK;")
        .unwrap();
}

#[tokio::test]
async fn a_stale_pair_marker_does_not_refresh_a_new_grant_with_the_same_bearer() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = fresh("user");
    let account = capture(&path, &c).unwrap();
    let rejected = format!("pair:{}", pair(&c));
    let mut newer = c.clone();
    newer.refresh_token = "user:new-login".into();
    capture(&path, &newer).unwrap();
    let api = Arc::new(Api::default());
    let server = Server::new(api.clone()).await;
    let session = session_at(
        &path,
        &account.id,
        true,
        Some(&rejected),
        &server.base,
        None,
    )
    .await
    .unwrap();
    assert_eq!(session.credentials.access_token, c.access_token);
    assert_eq!(session.credentials.refresh_token, newer.refresh_token);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_unbound_account_releases_a_foreign_native_lock_before_its_own_refresh() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let c = credential("user", "old", chrono::Utc::now().timestamp_millis() - 1000);
    let account = capture(&path, &c).unwrap();
    let foreign = fresh("foreign");
    let native = fixture.0.join("providers.json");
    fs::write(
        &native,
        serde_json::to_vec(&json!({"version":1,"modes":{},"providers":{"cline":{"settings":{"provider":"cline",
            "auth":{"accessToken":"workos:old-foreign","refreshToken":"foreign:old","accountId":"foreign",
                "expiresAt":foreign.expires_at,"metadata":{"userInfo":foreign.user_info}}}}}}))
        .unwrap(),
    )
    .unwrap();
    let before = fs::read(&native).unwrap();
    let api = Arc::new(Api {
        block_refresh: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let mut request = Box::pin(session_at(
        &path,
        &account.id,
        false,
        None,
        &server.base,
        Some(&native),
    ));
    tokio::select! {
        _ = api.started.notified() => {},
        _ = &mut request => panic!("refresh must wait for the fixture response"),
        _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("refresh did not reach the controlled upstream"),
    }
    let lock = PathBuf::from(format!(
        "{}.oauth-{:x}.lock",
        native.display(),
        Sha256::digest(b"cline")
    ));
    assert!(
        lock.is_file(),
        "probe must target the native lock opened by the request"
    );
    let probe = rusqlite::Connection::open(&lock).unwrap();
    probe
        .execute_batch("PRAGMA busy_timeout = 0; BEGIN EXCLUSIVE;")
        .unwrap();
    assert_eq!(fs::read(&native).unwrap(), before);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    api.released.notify_one();
    let session = tokio::time::timeout(Duration::from_secs(2), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.credentials.refresh_token, "user:new");
    assert!(session.native.is_none());
    assert_eq!(fs::read(&native).unwrap(), before);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 1);
    probe.execute_batch("ROLLBACK;").unwrap();
}
