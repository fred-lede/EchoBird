use super::*;
use axum::{extract::State, routing::post, Json, Router};
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::Semaphore;

pub(crate) fn failed_writes() -> &'static Mutex<BTreeSet<PathBuf>> {
    static FAILED: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    FAILED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

struct Fixture {
    dir: PathBuf,
    path: PathBuf,
    id: String,
}

impl Fixture {
    fn new(expired: bool) -> Self {
        let dir =
            std::env::temp_dir().join(format!("echobird-kiro-proxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("accounts.json");
        let mut credentials = super::super::tests::credentials();
        if expired {
            credentials.token["expires_at"] = json!("2020-01-01T00:00:00Z");
        }
        let mut store = Store::new();
        let account = capture(Edition::Cli, &credentials, &mut store).unwrap();
        save_at(&path, &store).unwrap();
        Self {
            dir,
            path,
            id: account.id,
        }
    }
    fn credentials(&self) -> Credentials {
        unprotect(&load_at(&self.path).unwrap()[&self.id]).unwrap()
    }
    fn builder() -> Self {
        let mut fixture = Self::new(true);
        let mut credentials = fixture.credentials();
        credentials.token["provider"] = json!("BuilderId");
        credentials.token["profile_arn"] = Value::Null;
        let mut store = Store::new();
        fixture.id = capture(Edition::Cli, &credentials, &mut store).unwrap().id;
        save_at(&fixture.path, &store).unwrap();
        fixture
    }
    fn native(&self, edition: Edition) -> PathBuf {
        let path = if edition == Edition::Cli {
            self.dir.join("data.sqlite3")
        } else {
            self.dir.join("kas")
        };
        if edition == Edition::Cli {
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT); CREATE TABLE state (key TEXT PRIMARY KEY,value TEXT);").unwrap();
        }
        native::fixture_write(edition, &path, &self.credentials()).unwrap();
        path
    }
    fn bind(&self, edition: Edition, path: &Path) {
        let mut store = load_at(&self.path).unwrap();
        let generation = pair(&unprotect(&store[&self.id]).unwrap());
        store
            .get_mut(&self.id)
            .unwrap()
            .native_pairs
            .insert(native::binding_key(edition, path), generation);
        save_at(&self.path, &store).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        failed_writes().lock().unwrap().remove(&self.path);
        forget_pending(&self.path, &self.id);
        std::fs::remove_dir_all(&self.dir).unwrap();
    }
}

struct Server {
    calls: AtomicUsize,
    entered: Semaphore,
    release: Semaphore,
}

async fn refresh(State(state): State<Arc<Server>>, Json(request): Json<Value>) -> Json<Value> {
    assert_eq!(request["refreshToken"], "fixture-refresh");
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.entered.add_permits(1);
    state.release.acquire().await.unwrap().forget();
    Json(
        json!({"accessToken":"rotated-access", "refreshToken":"rotated-refresh", "expiresIn":3600}),
    )
}

async fn server(blocked: bool) -> (String, Arc<Server>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(Server {
        calls: AtomicUsize::new(0),
        entered: Semaphore::new(0),
        release: Semaphore::new(if blocked { 0 } else { 20 }),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("http://{}/refresh", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/refresh", post(refresh))
        .with_state(state.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, state, task)
}

async fn request(
    path: PathBuf,
    id: String,
    rejected: Option<Value>,
    targets: Vec<(Edition, PathBuf)>,
    running: bool,
    endpoint: String,
) -> Result<Value, String> {
    complete(async move {
        let _request = REQUEST_LOCK.lock().await;
        let credentials = obtain_locked(
            &path,
            &id,
            rejected.is_some(),
            rejected.as_ref(),
            &targets,
            running,
            Some(&endpoint),
        )
        .await?;
        value(&id, &credentials)
    })
    .await
}

#[tokio::test]
async fn saved_only_list_keeps_cached_summaries_without_decrypting_or_creating_anything() {
    let fixture = Fixture::new(false);
    let mut store = load_at(&fixture.path).unwrap();
    store.get_mut(&fixture.id).unwrap().credential = "invalid-encryption".into();
    save_at(&fixture.path, &store).unwrap();
    let before = std::fs::read(&fixture.path).unwrap();
    let rows = list_at(&fixture.path).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, fixture.id);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    let missing = fixture.dir.join("never-created.json");
    assert!(list_at(&missing).unwrap().is_empty());
    assert!(!missing.exists());
    assert_eq!(std::fs::read_dir(&fixture.dir).unwrap().count(), 1);
}

#[tokio::test]
async fn concurrent_expiry_and_stale_rejection_rotate_once_and_keep_secrets_backend_only() {
    let fixture = Fixture::new(true);
    let rejected = value(&fixture.id, &fixture.credentials()).unwrap();
    let (url, state, task) = server(false).await;
    let (first, second) = tokio::join!(
        request(
            fixture.path.clone(),
            fixture.id.clone(),
            None,
            vec![],
            false,
            url.clone()
        ),
        request(
            fixture.path.clone(),
            fixture.id.clone(),
            Some(rejected.clone()),
            vec![],
            false,
            url.clone()
        )
    );
    for value in [first.unwrap(), second.unwrap()] {
        assert_eq!(value["accessToken"], "rotated-access");
        assert_ne!(
            value["credentialGeneration"],
            rejected["credentialGeneration"]
        );
        assert!(value.get("refreshToken").is_none());
        assert!(value.get("registration").is_none());
    }
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture.credentials().token["refresh_token"],
        "rotated-refresh"
    );
    assert!(!std::fs::read_to_string(&fixture.path)
        .unwrap()
        .contains("rotated-access"));
    task.abort();
}

#[tokio::test]
async fn fresh_credentials_and_profileless_builder_ids_need_no_network_or_fabricated_profile() {
    let fixture = Fixture::new(false);
    let (url, state, task) = server(false).await;
    let before = std::fs::read(&fixture.path).unwrap();
    request(
        fixture.path.clone(),
        fixture.id.clone(),
        None,
        vec![],
        false,
        url,
    )
    .await
    .unwrap();
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
    let mut builder = fixture.credentials();
    builder.token["provider"] = json!("BuilderId");
    builder.token["profile_arn"] = json!(LEGACY_BUILDER_PROFILE);
    builder.token["region"] = json!("eu-central-1");
    let result = value(&builder.identity().unwrap(), &builder).unwrap();
    assert!(result.get("profileArn").is_none());
    assert_eq!(result["region"], "eu-central-1");
    builder.token["provider"] = json!("ExternalIdp");
    assert_eq!(
        value(&builder.identity().unwrap(), &builder).unwrap()["tokenType"],
        "EXTERNAL_IDP"
    );
    task.abort();
}

#[tokio::test]
async fn cancelling_the_request_keeps_a_received_rotation_until_saved() {
    let fixture = Fixture::new(true);
    let (url, state, server) = server(true).await;
    let path = fixture.path.clone();
    let id = fixture.id.clone();
    let caller = tokio::spawn(request(path, id, None, vec![], false, url));
    tokio::time::timeout(Duration::from_secs(2), state.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    caller.abort();
    state.release.add_permits(1);
    let _request = tokio::time::timeout(Duration::from_secs(2), REQUEST_LOCK.lock())
        .await
        .unwrap();
    assert_eq!(fixture.credentials().access().unwrap(), "rotated-access");
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn failed_save_retains_new_pair_and_retry_heals_native_without_another_refresh() {
    for (edition, builder) in [
        (Edition::Cli, false),
        (Edition::Crew, false),
        (Edition::Cli, true),
    ] {
        let fixture = if builder {
            Fixture::builder()
        } else {
            Fixture::new(true)
        };
        let native = fixture.native(edition);
        fixture.bind(edition, &native);
        let targets = vec![(edition, native.clone())];
        let (url, state, task) = server(true).await;
        let path = fixture.path.clone();
        let id = fixture.id.clone();
        let caller = tokio::spawn(request(path, id, None, targets.clone(), false, url.clone()));
        state.entered.acquire().await.unwrap().forget();
        failed_writes().lock().unwrap().insert(fixture.path.clone());
        state.release.add_permits(1);
        assert_eq!(caller.await.unwrap().unwrap_err(), "accountError.write");
        assert_eq!(fixture.credentials().access().unwrap(), "fixture-access");
        {
            let session = native::Session::open(edition, &native).unwrap().unwrap();
            assert_eq!(
                session.read().unwrap().unwrap().access().unwrap(),
                "fixture-access"
            );
        }
        failed_writes().lock().unwrap().remove(&fixture.path);
        let result = request(
            fixture.path.clone(),
            fixture.id.clone(),
            None,
            targets,
            false,
            url,
        )
        .await
        .unwrap();
        assert_eq!(result["accessToken"], "rotated-access");
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        {
            let session = native::Session::open(edition, &native).unwrap().unwrap();
            assert_eq!(
                session.read().unwrap().unwrap().access().unwrap(),
                "rotated-access"
            );
        }
        task.abort();
    }
}

#[tokio::test]
async fn a_known_builder_native_baseline_recovers_after_a_native_write_failure_or_account_switch() {
    for apply_other in [false, true] {
        let fixture = Fixture::builder();
        let path = fixture.native(Edition::Cli);
        fixture.bind(Edition::Cli, &path);
        rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE TRIGGER reject_native_refresh BEFORE INSERT ON auth_kv WHEN NEW.key = 'kirocli:odic:token' BEGIN SELECT RAISE(FAIL, 'fixture native write failure'); END;").unwrap();
        let targets = vec![(Edition::Cli, path.clone())];
        let (url, state, task) = server(false).await;
        let error = request(
            fixture.path.clone(),
            fixture.id.clone(),
            None,
            targets.clone(),
            false,
            url.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error, "accountError.write");
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.credentials().access().unwrap(), "rotated-access");
        {
            let session = native::Session::open(Edition::Cli, &path).unwrap().unwrap();
            let native = session.read().unwrap().unwrap();
            assert!(native.identity().is_none());
            assert_eq!(native.access().unwrap(), "fixture-access");
            assert_eq!(
                load_at(&fixture.path).unwrap()[&fixture.id].native_pairs[&session.key()],
                pair(&native)
            );
        }
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch("DROP TRIGGER reject_native_refresh;")
            .unwrap();
        if apply_other {
            let mut selected = super::super::tests::credentials();
            selected.token["access_token"] = json!("selected-access");
            selected.token["refresh_token"] = json!("selected-refresh");
            let mut store = load_at(&fixture.path).unwrap();
            let selected_id = capture(Edition::Cli, &selected, &mut store).unwrap().id;
            save_at(&fixture.path, &store).unwrap();
            let _request = REQUEST_LOCK.lock().await;
            let mut session = native::Session::open_for_apply(Edition::Cli, &path).unwrap();
            apply_at(&fixture.path, &selected_id, &selected, &mut session)
                .await
                .unwrap();
            assert_eq!(
                session.read().unwrap().unwrap().access().unwrap(),
                selected.access().unwrap()
            );
            assert_eq!(fixture.credentials().access().unwrap(), "rotated-access");
            assert!(!load_at(&fixture.path).unwrap()[&fixture.id]
                .native_pairs
                .contains_key(&session.key()));
        } else {
            let credentials = request(
                fixture.path.clone(),
                fixture.id.clone(),
                None,
                targets,
                false,
                url,
            )
            .await
            .unwrap();
            assert_eq!(credentials["accessToken"], "rotated-access");
            let session = native::Session::open(Edition::Cli, &path).unwrap().unwrap();
            assert_eq!(
                session.read().unwrap().unwrap().access().unwrap(),
                "rotated-access"
            );
            assert_eq!(
                load_at(&fixture.path).unwrap()[&fixture.id].native_pairs[&session.key()],
                pair(&fixture.credentials())
            );
        }
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn deletion_or_independent_login_during_refresh_cannot_be_overwritten() {
    for changed in [0, 1, 2] {
        let fixture = Fixture::new(true);
        let (url, state, task) = server(true).await;
        let caller = tokio::spawn(request(
            fixture.path.clone(),
            fixture.id.clone(),
            None,
            vec![],
            false,
            url,
        ));
        state.entered.acquire().await.unwrap().forget();
        let mut store = load_at(&fixture.path).unwrap();
        if changed == 0 {
            store.remove(&fixture.id);
        } else {
            let mut login = fixture.credentials();
            if changed == 1 {
                login.token["access_token"] = json!("independent-access");
                login.token["refresh_token"] = json!("independent-refresh");
                login.token["expires_at"] = json!("2099-01-01T00:00:00Z");
            } else {
                login.registration = Some(json!({"client_id":"independent-client"}));
            }
            capture(Edition::Crew, &login, &mut store).unwrap();
        }
        save_at(&fixture.path, &store).unwrap();
        state.release.add_permits(1);
        assert_eq!(caller.await.unwrap().unwrap_err(), "accountError.cancelled");
        if changed == 0 {
            assert!(load_at(&fixture.path).unwrap().is_empty());
        } else if changed == 1 {
            assert_eq!(
                fixture.credentials().access().unwrap(),
                "independent-access"
            );
        } else {
            assert_eq!(
                fixture.credentials().registration.unwrap()["client_id"],
                "independent-client"
            );
        }
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

#[tokio::test]
async fn unknown_rotated_cli_grants_cannot_cross_accounts_or_be_discarded_during_apply() {
    let fixture = Fixture::builder();
    let path = fixture.native(Edition::Cli);
    fixture.bind(Edition::Cli, &path);
    let mut rotated = fixture.credentials();
    rotated.token["access_token"] = json!("unknown-native-access");
    rotated.token["refresh_token"] = json!("unknown-native-refresh");
    native::fixture_write(Edition::Cli, &path, &rotated).unwrap();
    let (url, state, task) = server(false).await;
    let result = request(
        fixture.path.clone(),
        fixture.id.clone(),
        None,
        vec![(Edition::Cli, path.clone())],
        false,
        url,
    )
    .await
    .unwrap_err();
    assert_eq!(result, "accountError.loginRequired");
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    let _request = REQUEST_LOCK.lock().await;
    let mut session = native::Session::open(Edition::Cli, &path).unwrap().unwrap();
    assert_eq!(
        capture_outgoing(&fixture.path, &mut session)
            .await
            .unwrap_err(),
        "accountError.invalidAccount"
    );
    assert_eq!(
        session.read().unwrap().unwrap().access().unwrap(),
        "unknown-native-access"
    );
    assert_eq!(fixture.credentials().access().unwrap(), "fixture-access");
    drop(session);
    task.abort();
}

#[tokio::test]
async fn an_independent_same_user_login_never_adopts_or_updates_the_native_grant() {
    let fixture = Fixture::new(true);
    let native = fixture.native(Edition::Cli);
    let mut independent = fixture.credentials();
    independent.token["access_token"] = json!("independent-native-access");
    independent.token["refresh_token"] = json!("independent-native-refresh");
    independent.token["expires_at"] = json!("2099-01-01T00:00:00Z");
    native::fixture_write(Edition::Cli, &native, &independent).unwrap();
    let mut store = load_at(&fixture.path).unwrap();
    store.get_mut(&fixture.id).unwrap().account.active = true;
    save_at(&fixture.path, &store).unwrap();
    let (url, state, task) = server(false).await;
    let result = request(
        fixture.path.clone(),
        fixture.id.clone(),
        None,
        vec![(Edition::Cli, native.clone())],
        true,
        url,
    )
    .await
    .unwrap();
    assert_eq!(result["accessToken"], "rotated-access");
    assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    let session = native::Session::open(Edition::Cli, &native)
        .unwrap()
        .unwrap();
    assert_eq!(
        session.read().unwrap().unwrap().access().unwrap(),
        "independent-native-access"
    );
    drop(session);
    task.abort();
}

#[tokio::test]
async fn running_native_owners_route_fresh_grants_but_never_refresh_a_rejected_or_expired_grant() {
    let fixture = Fixture::new(false);
    let native = fixture.native(Edition::Cli);
    fixture.bind(Edition::Cli, &native);
    let targets = vec![(Edition::Cli, native.clone())];
    let (url, state, task) = server(false).await;
    let result = request(
        fixture.path.clone(),
        fixture.id.clone(),
        None,
        targets.clone(),
        true,
        url.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result["accessToken"], "fixture-access");
    let rejected = request(
        fixture.path.clone(),
        fixture.id.clone(),
        Some(result),
        targets,
        true,
        url,
    )
    .await
    .unwrap_err();
    assert_eq!(rejected, "accountError.closeClient");
    assert_eq!(state.calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn applying_another_account_preserves_an_outgoing_bound_native_rotation() {
    for edition in [Edition::Cli, Edition::Crew] {
        let fixture = Fixture::new(false);
        let native = fixture.native(edition);
        fixture.bind(edition, &native);
        let mut rotated = fixture.credentials();
        rotated.token["access_token"] = json!("native-rotated-access");
        rotated.token["refresh_token"] = json!("native-rotated-refresh");
        native::fixture_write(edition, &native, &rotated).unwrap();
        let _request = REQUEST_LOCK.lock().await;
        let mut session = native::Session::open(edition, &native).unwrap().unwrap();
        capture_outgoing(&fixture.path, &mut session).await.unwrap();
        let mut other = rotated.clone();
        other.token["profile_arn"] = json!("arn:aws:codewhisperer:us-east-1:123:profile/other");
        other.token["access_token"] = json!("other-access");
        session.write(&other, true).unwrap();
        assert_eq!(
            fixture.credentials().access().unwrap(),
            "native-rotated-access"
        );
        drop(session);
    }
}

#[tokio::test]
async fn failed_binding_save_rolls_back_native_apply_and_keeps_captured_outgoing_credentials() {
    for edition in [Edition::Cli, Edition::Crew] {
        let fixture = Fixture::new(false);
        let path = fixture.native(edition);
        fixture.bind(edition, &path);
        let mut outgoing = fixture.credentials();
        outgoing.token["access_token"] = json!("outgoing-rotated-access");
        outgoing.token["refresh_token"] = json!("outgoing-rotated-refresh");
        native::fixture_write(edition, &path, &outgoing).unwrap();
        let mut selected = outgoing.clone();
        selected.token["profile_arn"] =
            json!("arn:aws:codewhisperer:us-east-1:123:profile/selected");
        selected.token["access_token"] = json!("selected-access");
        selected.token["refresh_token"] = json!("selected-refresh");
        let mut store = load_at(&fixture.path).unwrap();
        let selected_id = capture(Edition::Cli, &selected, &mut store).unwrap().id;
        save_at(&fixture.path, &store).unwrap();
        let _request = REQUEST_LOCK.lock().await;
        let mut session = native::Session::open_for_apply(edition, &path).unwrap();
        capture_outgoing(&fixture.path, &mut session).await.unwrap();
        let before = std::fs::read(&fixture.path).unwrap();
        failed_writes().lock().unwrap().insert(fixture.path.clone());
        assert_eq!(
            apply_at(&fixture.path, &selected_id, &selected, &mut session)
                .await
                .unwrap_err(),
            "accountError.write"
        );
        assert_eq!(std::fs::read(&fixture.path).unwrap(), before);
        assert_eq!(
            fixture.credentials().access().unwrap(),
            "outgoing-rotated-access"
        );
        assert_eq!(
            session.read().unwrap().unwrap().access().unwrap(),
            "outgoing-rotated-access"
        );
        failed_writes().lock().unwrap().remove(&fixture.path);
        apply_at(&fixture.path, &selected_id, &selected, &mut session)
            .await
            .unwrap();
        assert_eq!(
            session.read().unwrap().unwrap().access().unwrap(),
            "selected-access"
        );
        let store = load_at(&fixture.path).unwrap();
        assert!(!store[&fixture.id].native_pairs.contains_key(&session.key()));
        assert_eq!(
            store[&selected_id].native_pairs[&session.key()],
            pair(&selected)
        );
        drop(session);
    }
}

#[tokio::test]
async fn failed_first_crew_apply_leaves_no_native_credentials() {
    let fixture = Fixture::new(false);
    let path = fixture.dir.join("new-kas");
    let credentials = fixture.credentials();
    let _request = REQUEST_LOCK.lock().await;
    let mut session = native::Session::open_for_apply(Edition::Crew, &path).unwrap();
    failed_writes().lock().unwrap().insert(fixture.path.clone());
    assert_eq!(
        apply_at(&fixture.path, &fixture.id, &credentials, &mut session)
            .await
            .unwrap_err(),
        "accountError.write"
    );
    assert!(session.read().unwrap().is_none());
    assert!(!path.join(".vault/secrets.enc").exists());
    assert!(!path.join(".vault/.vault_key").exists());
    drop(session);
}
