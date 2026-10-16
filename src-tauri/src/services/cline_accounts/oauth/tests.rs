use super::*;
use axum::{
    extract::{Form, State},
    http::{HeaderMap, StatusCode},
    routing::{get as route_get, post},
    Json, Router,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};

static LOGIN_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct Api {
    authorizations: AtomicUsize,
    authentications: AtomicUsize,
    registrations: AtomicUsize,
    refreshes: AtomicUsize,
    requests: Mutex<Vec<(String, Value, Instant)>>,
    pending_once: bool,
    slow_down_once: bool,
    always_pending: bool,
    block_authorize: bool,
    block_register: bool,
    wrapped: bool,
    fail_refresh: bool,
    malformed_refresh: bool,
    expires: i64,
    uri: Mutex<String>,
    started: tokio::sync::Notify,
    released: tokio::sync::Notify,
}
fn token_body() -> Value {
    json!({"accessToken":"workos:cline-access","refreshToken":"cline-refresh","expiresAt":"2030-01-01T00:00:00Z",
        "userInfo":{"clineUserId":"cline-user","subject":"workos-subject","email":"user@example.test","name":"Fixture user"}})
}
fn credential() -> Credentials {
    token(&token_body(), None).unwrap()
}
async fn authorize(
    State(api): State<Arc<Api>>,
    Form(body): Form<BTreeMap<String, String>>,
) -> Json<Value> {
    api.authorizations.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("authorize".into(), json!(body), Instant::now()));
    if api.block_authorize {
        api.started.notify_one();
        api.released.notified().await;
    }
    Json(
        json!({"device_code":"fixture-device","user_code":"ABC-123","verification_uri_complete":*api.uri.lock().unwrap(),
        "interval":1,"expires_in":if api.expires > 0 { api.expires } else { 300 }}),
    )
}
async fn authenticate(
    State(api): State<Arc<Api>>,
    Form(body): Form<BTreeMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let count = api.authentications.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("authenticate".into(), json!(body), Instant::now()));
    if api.always_pending || (count == 0 && api.pending_once) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"authorization_pending"})),
        );
    }
    if count == 0 && api.slow_down_once {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"slow_down"})));
    }
    (
        StatusCode::OK,
        Json(json!({"access_token":"workos-access","refresh_token":"workos-refresh"})),
    )
}
async fn register(State(api): State<Arc<Api>>, Json(body): Json<Value>) -> Json<Value> {
    api.registrations.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("register".into(), body, Instant::now()));
    if api.block_register {
        api.started.notify_one();
        api.released.notified().await;
    }
    Json(if api.wrapped {
        json!({"success":true,"data":token_body()})
    } else {
        token_body()
    })
}
async fn refresh_endpoint(
    State(api): State<Arc<Api>>,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    api.refreshes.fetch_add(1, Ordering::SeqCst);
    api.requests
        .lock()
        .unwrap()
        .push(("refresh".into(), body, Instant::now()));
    if api.fail_refresh {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":{"code":"invalid_grant"}})),
        );
    }
    let mut result = token_body();
    result["accessToken"] = json!("workos:rotated-access");
    result["refreshToken"] = json!("rotated-refresh");
    if api.malformed_refresh {
        result["userInfo"]["clineUserId"] = json!("other-user");
    }
    (StatusCode::OK, Json(json!({"success":true,"data":result})))
}
async fn probe(headers: HeaderMap) -> (StatusCode, &'static str) {
    if headers["authorization"] != "Bearer workos:cline-access" {
        return (StatusCode::FORBIDDEN, "wrong bearer");
    }
    (StatusCode::UNAUTHORIZED, "plain text unauthorized")
}

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn new(api: Arc<Api>) -> Self {
        let app = Router::new()
            .route("/user_management/authorize/device", post(authorize))
            .route("/user_management/authenticate", post(authenticate))
            .route("/api/v1/auth/register", post(register))
            .route("/api/v1/auth/refresh", post(refresh_endpoint))
            .route("/probe", route_get(probe))
            .with_state(api.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        if api.uri.lock().unwrap().is_empty() {
            *api.uri.lock().unwrap() = format!("{base}/activate");
        }
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
async fn wait_for_result(id: &str) -> Result<Credentials, String> {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(c) = poll(id)? {
                return Ok(c);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("fixture OAuth attempt did not finish")
}

#[test]
fn token_parsing_strips_native_prefix_and_binds_millisecond_expiry_to_cline_identity() {
    let c = credential();
    assert_eq!(c.access_token, "cline-access");
    assert_eq!(c.refresh_token, "cline-refresh");
    assert_eq!(c.expires_at, 1_893_456_000_000);
    assert_eq!(c.user_id, "cline-user");
    assert_eq!(c.user_info["subject"], "workos-subject");
    assert_eq!(super::super::bearer(&c), "workos:cline-access");
    let mut body = token_body();
    body["userInfo"]["clineUserId"] = json!("other");
    assert_eq!(
        token(&body, Some(&c)).err().unwrap(),
        "accountError.invalidAccount"
    );
    body = token_body();
    body["userInfo"]["subject"] = json!("other-subject");
    assert_eq!(
        token(&body, Some(&c)).err().unwrap(),
        "accountError.invalidAccount"
    );
    for malformed in [
        Value::Null,
        json!({"accessToken":"x"}),
        json!({"success":true,"data":token_body()}),
    ] {
        assert!(token(&malformed, None).is_err());
    }
}

#[test]
fn response_envelopes_allow_real_null_plan_and_reject_false_or_missing_data() {
    assert_eq!(data(StatusCode::OK, Value::Null).unwrap(), Value::Null);
    assert_eq!(
        data(StatusCode::OK, json!({"success":true,"data":null})).unwrap(),
        Value::Null
    );
    assert_eq!(
        data(StatusCode::OK, json!({"success":true,"data":token_body()})).unwrap(),
        token_body()
    );
    assert!(data(StatusCode::OK, json!({"success":false,"data":token_body()})).is_err());
    assert!(data(StatusCode::OK, json!({"success":true})).is_err());
    assert_eq!(
        data(StatusCode::UNAUTHORIZED, Value::Null).err().unwrap(),
        "accountError.loginRequired"
    );
}

#[tokio::test]
async fn device_login_registers_workos_pair_before_exposing_the_cline_pair_and_enforces_poll_interval(
) {
    let _serial = LOGIN_TEST_LOCK.lock().await;
    let api = Arc::new(Api {
        pending_once: true,
        wrapped: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let before = chrono::Utc::now().timestamp();
    let started = start_at(&server.base, &server.base).await.unwrap();
    assert_eq!(
        started.verification_uri,
        format!("{}/activate", server.base)
    );
    assert_eq!(started.user_code, "ABC-123");
    assert!(started.expires_at <= before + 61 && started.expires_at > before);
    let c = wait_for_result(&started.login_id).await.unwrap();
    assert_eq!(c.access_token, "cline-access");
    assert_eq!(c.refresh_token, "cline-refresh");
    assert_eq!(api.authorizations.load(Ordering::SeqCst), 1);
    assert_eq!(api.authentications.load(Ordering::SeqCst), 2);
    assert_eq!(api.registrations.load(Ordering::SeqCst), 1);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 0);
    let requests = api.requests.lock().unwrap();
    assert_eq!(requests[0].1, json!({"client_id":CLIENT_ID}));
    let polls: Vec<_> = requests
        .iter()
        .filter(|(kind, _, _)| kind == "authenticate")
        .collect();
    assert_eq!(
        polls[0].1,
        json!({"grant_type":"urn:ietf:params:oauth:grant-type:device_code","device_code":"fixture-device","client_id":CLIENT_ID})
    );
    assert!(polls[1].2.duration_since(polls[0].2) >= Duration::from_millis(900));
    assert_eq!(
        requests
            .iter()
            .find(|(kind, _, _)| kind == "register")
            .unwrap()
            .1,
        json!({"accessToken":"workos-access","refreshToken":"workos-refresh"})
    );
    drop(requests);
    let saved = AtomicUsize::new(0);
    complete(&started.login_id, || {
        saved.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .unwrap();
    assert_eq!(saved.load(Ordering::SeqCst), 1);
    assert!(matches!(poll(&started.login_id),Err(error) if error == "accountError.cancelled"));
}

#[tokio::test]
async fn slow_down_increases_the_actual_poll_delay_instead_of_repeating_immediately() {
    let _serial = LOGIN_TEST_LOCK.lock().await;
    let api = Arc::new(Api {
        slow_down_once: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let started = start_at(&server.base, &server.base).await.unwrap();
    wait_for_result(&started.login_id).await.unwrap();
    let requests = api.requests.lock().unwrap();
    let polls: Vec<_> = requests
        .iter()
        .filter(|(kind, _, _)| kind == "authenticate")
        .collect();
    assert_eq!(polls.len(), 2);
    assert!(polls[1].2.duration_since(polls[0].2) >= Duration::from_millis(1900));
    drop(requests);
    cancel(&started.login_id).unwrap();
}

#[tokio::test]
async fn cancellation_during_start_and_late_register_cannot_capture_an_account() {
    let _serial = LOGIN_TEST_LOCK.lock().await;
    let api = Arc::new(Api {
        block_authorize: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let starter = start_at(&server.base, &server.base);
    let cancel_start = async {
        api.started.notified().await;
        let id = PENDING.lock().unwrap().as_ref().unwrap().id.clone();
        cancel(&id).unwrap();
        api.released.notify_one();
    };
    let (result, ()) = tokio::join!(starter, cancel_start);
    assert!(matches!(result,Err(error) if error == "accountError.cancelled"));
    assert_eq!(api.authentications.load(Ordering::SeqCst), 0);
    assert_eq!(api.registrations.load(Ordering::SeqCst), 0);

    let api = Arc::new(Api {
        block_register: true,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let started = start_at(&server.base, &server.base).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), api.started.notified())
        .await
        .unwrap();
    cancel(&started.login_id).unwrap();
    api.released.notify_one();
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(matches!(poll(&started.login_id),Err(error) if error == "accountError.cancelled"));
    let wrote = AtomicBool::new(false);
    assert!(complete(&started.login_id, || {
        wrote.store(true, Ordering::SeqCst);
        Ok(())
    })
    .is_err());
    assert!(!wrote.load(Ordering::SeqCst));
    assert_eq!(api.registrations.load(Ordering::SeqCst), 1);
    assert_eq!(api.refreshes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn replacing_a_login_attempt_and_an_upstream_deadline_leave_no_late_save_path() {
    let _serial = LOGIN_TEST_LOCK.lock().await;
    let api = Arc::new(Api {
        always_pending: true,
        expires: 1,
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    let first = start_at(&server.base, &server.base).await.unwrap();
    let second = start_at(&server.base, &server.base).await.unwrap();
    assert_ne!(first.login_id, second.login_id);
    assert!(matches!(poll(&first.login_id),Err(error) if error == "accountError.cancelled"));
    assert!(
        matches!(wait_for_result(&second.login_id).await,Err(error) if error == "accountError.expired")
    );
    assert_eq!(api.registrations.load(Ordering::SeqCst), 0);
    cancel(&second.login_id).unwrap();
}

#[tokio::test]
async fn unsafe_browser_verification_url_is_rejected_before_polling() {
    let _serial = LOGIN_TEST_LOCK.lock().await;
    let api = Arc::new(Api {
        uri: Mutex::new("file:///credentials".into()),
        ..Api::default()
    });
    let server = Server::new(api.clone()).await;
    assert!(
        matches!(start_at(&server.base,&server.base).await,Err(error) if error == "accountError.authResponse")
    );
    assert_eq!(api.authentications.load(Ordering::SeqCst), 0);
    assert_eq!(api.registrations.load(Ordering::SeqCst), 0);
    assert!(PENDING.lock().unwrap().is_none());
}

#[tokio::test]
async fn refresh_rotates_both_tokens_uses_exact_body_and_rejects_wrong_identity_or_revoked_grant() {
    let c = credential();
    let api = Arc::new(Api::default());
    let server = Server::new(api.clone()).await;
    let rotated = refresh(&c, &server.base).await.unwrap();
    assert_eq!(rotated.access_token, "rotated-access");
    assert_eq!(rotated.refresh_token, "rotated-refresh");
    assert_eq!(rotated.user_id, c.user_id);
    assert_eq!(
        api.requests.lock().unwrap()[0].1,
        json!({"refreshToken":"cline-refresh","grantType":"refresh_token"})
    );
    let server = Server::new(Arc::new(Api {
        malformed_refresh: true,
        ..Api::default()
    }))
    .await;
    assert_eq!(
        refresh(&c, &server.base).await.err().unwrap(),
        "accountError.invalidAccount"
    );
    let server = Server::new(Arc::new(Api {
        fail_refresh: true,
        ..Api::default()
    }))
    .await;
    assert_eq!(
        refresh(&c, &server.base).await.err().unwrap(),
        "accountError.loginRequired"
    );
}

#[tokio::test]
async fn plain_401_is_classified_as_login_required_and_bearer_is_not_double_prefixed() {
    let server = Server::new(Arc::new(Api::default())).await;
    let result = get(&client().unwrap(), &server.base, "/probe", &credential()).await;
    assert_eq!(result.err().unwrap(), "accountError.loginRequired");
}
