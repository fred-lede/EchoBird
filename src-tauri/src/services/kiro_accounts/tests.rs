use super::*;

pub(super) fn credentials() -> Credentials {
    Credentials {
        token: serde_json::json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","expires_at":"2099-01-01T00:00:00Z","provider":"google","profile_arn":"arn:aws:codewhisperer:us-east-1:123:profile/fixture","telemetry_generation":"fixture-generation"}),
        registration: None,
        subject: Some("fixture-user".into()),
        name: Some("fixture@example.test".into()),
        usage: None,
        source: None,
        source_path: None,
    }
}

#[test]
fn clients_share_accounts_and_native_sync_preserves_the_verified_identity() {
    let verified = credentials();
    let mut store = Store::new();
    let account = capture(Edition::Crew, &verified, &mut store).unwrap();
    let mut native = verified.clone();
    native.name = None;
    native.subject = None;
    let synchronized = capture(Edition::Cli, &native, &mut store).unwrap();
    assert_eq!(store.len(), 1);
    assert_eq!(synchronized.id, account.id);
    assert_eq!(synchronized.name, "fixture@example.test");
    assert_eq!(
        unprotect(&store[&account.id]).unwrap().name.as_deref(),
        Some("fixture@example.test")
    );
    assert!(store_path()
        .unwrap()
        .ends_with(".echobird/kiro-accounts.json"));
}

#[test]
fn native_stale_tokens_do_not_replace_a_new_browser_login() {
    let latest = credentials();
    let mut stale = latest.clone();
    stale.token["expires_at"] = serde_json::json!("2020-01-01T00:00:00Z");
    assert!(!credentials_newer(&stale, &latest));
    assert!(credentials_newer(&latest, &stale));
    assert!(credentials_newer(&latest, &latest));
    stale.token["expires_at"] = Value::Null;
    assert!(!credentials_newer(&stale, &latest));
}

#[test]
fn login_region_and_account_runtime_region_are_independent() {
    let mut credential = credentials();
    credential.token["region"] = serde_json::json!("us-west-2");
    credential.token["profile_arn"] =
        serde_json::json!("arn:aws:codewhisperer:eu-central-1:123:profile/fixture");
    assert_eq!(credential.auth_region().unwrap(), "us-west-2");
    assert_eq!(credential.region().unwrap(), "eu-central-1");
}

#[test]
fn native_identity_uses_account_data_and_does_not_share_the_builder_service_profile() {
    let mut first = credentials();
    let mut second = first.clone();
    second.token["access_token"] = serde_json::json!("rotated");
    assert_eq!(first.identity(), second.identity());
    second.token["profile_arn"] =
        serde_json::json!("arn:aws:codewhisperer:us-east-1:123:profile/other");
    assert_ne!(first.identity(), second.identity());
    first.token["provider"] = serde_json::json!("BuilderId");
    first.token["profile_arn"] = serde_json::json!(LEGACY_BUILDER_PROFILE);
    first.subject = None;
    assert_eq!(first.identity(), None);
    first.subject = Some("first-user".into());
    second = first.clone();
    second.subject = Some("second-user".into());
    assert_ne!(first.identity(), second.identity());
}

#[test]
fn invalid_saved_credentials_stop_before_decryption() {
    for credential in ["plaintext", "enc:v1:00", "enc:v1:zzzzzz"] {
        let saved = Saved {
            account: Account {
                id: "fixture".into(),
                edition: Edition::Cli,
                name: "Fixture".into(),
                plan: None,
                active: false,
                quota: vec![],
            },
            credential: credential.into(),
            native_pairs: BTreeMap::new(),
        };
        assert_eq!(
            unprotect(&saved).err().as_deref(),
            Some("accountError.keychain")
        );
        assert!(!native_is_newer(&credentials(), &saved));
    }
    for region in [
        "localhost",
        "us-east-1/evil",
        "us-east-1:443",
        "UPPER-region-1",
    ] {
        assert!(!valid_region(region));
    }
    for region in ["us-east-1", "eu-central-1", "us-gov-west-1"] {
        assert!(valid_region(region));
    }
}
