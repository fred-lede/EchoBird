use super::*;

fn attempt() -> Arc<Attempt> {
    Arc::new(Attempt {
        id: "attempt".into(),
        edition: Edition::Cli,
        state: "expected-state".into(),
        verifier: random(),
        port: 3128,
        expires: chrono::Utc::now().timestamp() + 60,
        cancel: CancellationToken::new(),
        outcome: Mutex::new(Outcome::Waiting),
        aws: Mutex::new(None),
    })
}

#[test]
fn portal_uses_the_native_callback_contract_and_pkce_vector() {
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    assert_eq!(
        challenge(verifier),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let parsed = url::Url::parse(&url(3128, "state", verifier).unwrap()).unwrap();
    let pairs = parsed
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(parsed.host_str(), Some("app.kiro.dev"));
    assert_eq!(pairs["redirect_uri"], "http://localhost:3128");
    assert_eq!(pairs["redirect_from"], "kirocli");
    assert_eq!(pairs["code_challenge_method"], "S256");
    assert_eq!(pairs["state"], "state");
}

#[tokio::test]
async fn foreign_duplicate_and_expired_callbacks_do_not_exchange_or_complete() {
    let attempt = attempt();
    let uri = "/oauth/callback".parse().unwrap();
    let invalid = callback(
        State(attempt.clone()),
        OriginalUri(uri),
        Query(vec![("state".into(), "foreign".into())]),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(matches!(*attempt.outcome.lock().unwrap(), Outcome::Waiting));
    assert!(one(
        &[
            ("code".into(), "first".into()),
            ("code".into(), "second".into())
        ],
        "code"
    )
    .is_err());
    let duplicate = callback(
        State(attempt.clone()),
        OriginalUri("/oauth/callback".parse().unwrap()),
        Query(vec![
            ("state".into(), "expected-state".into()),
            ("state".into(), "expected-state".into()),
        ]),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);
    attempt.cancel.cancel();
    let stopped = callback(
        State(attempt.clone()),
        OriginalUri("/oauth/callback".parse().unwrap()),
        Query(vec![("state".into(), "expected-state".into())]),
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::GONE);
    assert!(matches!(*attempt.outcome.lock().unwrap(), Outcome::Waiting));
}

#[tokio::test]
async fn cancelled_and_superseded_attempts_cannot_save_or_cancel_a_new_login() {
    let _guard = START_LOCK.lock().await;
    let old = attempt();
    *PENDING.lock().unwrap() = Some(old.clone());
    cancel("attempt").unwrap();
    assert!(old.cancel.is_cancelled());
    assert!(poll("attempt").unwrap().is_none());
    let mut wrote = false;
    assert!(complete("attempt", || {
        wrote = true;
        Ok(())
    })
    .is_err());
    assert!(!wrote);
    let new = Arc::new(Attempt {
        id: "new".into(),
        edition: Edition::Crew,
        state: random(),
        verifier: random(),
        port: 4649,
        expires: chrono::Utc::now().timestamp() + 60,
        cancel: CancellationToken::new(),
        outcome: Mutex::new(Outcome::Done(Ok(super::super::tests::credentials()))),
        aws: Mutex::new(None),
    });
    *PENDING.lock().unwrap() = Some(new.clone());
    cancel("attempt").unwrap();
    assert!(!new.cancel.is_cancelled());
    assert!(poll("attempt").unwrap().is_none());
    assert_eq!(poll("new").unwrap().unwrap().0, Edition::Crew);
    assert_eq!(complete("new", || Ok(7)).unwrap(), 7);
    assert!(new.cancel.is_cancelled());
    assert!(PENDING.lock().unwrap().is_none());
}
