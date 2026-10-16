use super::*;
use std::fs;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("echobird-cline-native-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn providers(&self, value: &Value) -> PathBuf {
        let path = self.0.join("providers.json");
        fs::write(&path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
        path
    }
    fn storage(&self, selection: &Value) -> Storage {
        let path = self.0.join("localstorage.sqlite3");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB NOT NULL);")
            .unwrap();
        let value: Vec<u8> = selection
            .to_string()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        db.execute(
            "INSERT INTO ItemTable VALUES (?1, ?2)",
            rusqlite::params!["cline.code.model-selection.v1", value],
        )
        .unwrap();
        db.execute(
            "INSERT INTO ItemTable VALUES ('unrelated-session', ?1)",
            [b"leave alone".as_slice()],
        )
        .unwrap();
        drop(db);
        Storage::open_at(&path).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn credential(suffix: &str, expires_at: i64) -> Credentials {
    Credentials {
        access_token: format!("access-{suffix}"),
        refresh_token: format!("refresh-{suffix}"),
        expires_at,
        user_id: "user".into(),
        email: "new@example.test".into(),
        name: Some("New user".into()),
        user_info: json!({"clineUserId":"user","subject":"subject-user","name":"New user"}),
    }
}
fn state(c: &Credentials) -> Value {
    json!({"version":1,"modes":{"plan":{"model":"existing-mode-model","provider":"other"}},
        "lastUsedProvider":"other","providers":{
        "cline":{"settings":{"provider":"cline","model":"cline-free/test-model","baseUrl":"https://api.cline.bot/api/v1",
            "protocol":"openai-chat","client":"openai-compatible","auth":auth(c),"apiKey":"previous-api-key",
            "reasoningEffort":"high","headers":{"x-custom":"kept"}},"tokenSource":"oauth","updatedAt":"2029-01-01T00:00:00Z","extra":"preserve"},
        "cline-pass":{"settings":{"provider":"cline-pass","model":"cline-pass/another-model"}},
        "other":{"settings":{"provider":"other","model":"other-model","auth":{"accessToken":"other-token"}}}},
        "sessionIds":["existing-session"],"extra":{"setting":true}})
}
fn selection() -> Value {
    json!({"lastProvider":"other","lastModelByProvider":{"other":"other-model","cline":"cline-free/remembered-model",
        "cline-pass":"cline-pass/remembered-model"},"otherSelectionMetadata":{"kept":true}})
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn trusted_native_settings_reject_foreign_hosts_protocols_and_embedded_credentials() {
    for base in [
        "http://api.cline.bot",
        "https://foreign.example/api/v1",
        "https://api.cline.bot.evil.example",
        "https://user:pass@api.cline.bot",
        "https://api.cline.bot/api/v1?key=value",
        "https://api.cline.bot/other",
        "https://api.cline.bot:8443",
        "https://api.cline.bot/#fragment",
    ] {
        assert!(
            trusted(&json!({"provider":"cline","baseUrl":base})).is_err(),
            "{base}"
        );
    }
    for base in [
        "https://api.cline.bot",
        "https://api.cline.bot/",
        "https://api.cline.bot/api/v1/",
    ] {
        assert!(
            trusted(&json!({"provider":"cline","baseUrl":base})).is_ok(),
            "{base}"
        );
    }
    for (key, value) in [
        ("protocol", "openai-responses"),
        ("client", "foreign"),
        ("routingProviderId", "other"),
    ] {
        assert!(trusted(&json!({"provider":"cline",(key):value})).is_err());
    }
}

#[test]
fn native_tokens_use_unix_milliseconds_and_reject_a_different_account() {
    let previous = credential("old", 1_893_456_000_000);
    let mut native = auth(&previous);
    let loaded = native_credentials(&native, &previous).unwrap();
    assert_eq!(loaded.expires_at, 1_893_456_000_000);
    assert_eq!(loaded.access_token, "access-old");
    native["expiresAt"] = Value::Null;
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":1893456000}"#);
    native["accessToken"] = json!(format!("workos:fixture.{payload}.signature"));
    assert_eq!(
        native_credentials(&native, &previous).unwrap().expires_at,
        1_893_456_000_000
    );
    native["accountId"] = json!("other");
    assert!(native_credentials(&native, &previous).is_err());
    native["accountId"] = json!("user");
    native["metadata"]["userInfo"]["clineUserId"] = json!("other");
    assert!(native_credentials(&native, &previous).is_err());
}

#[tokio::test]
async fn native_selected_grant_is_authoritative_even_when_its_expiry_is_shorter() {
    let fixture = Fixture::new();
    let saved = credential("saved", chrono::Utc::now().timestamp_millis() + 7_200_000);
    let incoming = credential("native", saved.expires_at - 3_600_000);
    let path = fixture.providers(&state(&incoming));
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    let current = session
        .credentials(&saved, Some(&cline_accounts::pair(&saved)))
        .unwrap()
        .unwrap();
    assert_eq!(current.access_token, incoming.access_token);
    assert_eq!(current.refresh_token, incoming.refresh_token);
    assert_eq!(current.expires_at, incoming.expires_at);
    assert!(current.expires_at < saved.expires_at);

    let mut metadata = incoming.clone();
    metadata.expires_at -= 120_000;
    metadata.email = "updated@example.test".into();
    metadata.user_info["email"] = json!(metadata.email);
    fs::write(&path, state(&metadata).to_string()).unwrap();
    let current = session
        .credentials(&incoming, Some(&cline_accounts::pair(&incoming)))
        .unwrap()
        .unwrap();
    assert_eq!(current.expires_at, metadata.expires_at);
    assert_eq!(current.email, metadata.email);
}

#[tokio::test]
async fn the_selected_pass_grant_wins_without_overwriting_an_independent_base_grant() {
    let fixture = Fixture::new();
    let saved = credential("base", chrono::Utc::now().timestamp_millis() + 7_200_000);
    let pass = credential("pass", saved.expires_at - 3_600_000);
    let mut initial = state(&saved);
    initial["lastUsedProvider"] = json!("cline-pass");
    initial["providers"]["cline-pass"]["settings"]["auth"] = auth(&pass);
    let path = fixture.providers(&initial);
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    let current = session
        .credentials(&saved, Some(&cline_accounts::pair(&saved)))
        .unwrap()
        .unwrap();
    assert_eq!(current.access_token, pass.access_token);
    assert_eq!(current.expires_at, pass.expires_at);
    let new = credential("renewed-pass", pass.expires_at + 1000);
    session.synchronize(&new).unwrap();
    let updated = read(&path);
    assert_eq!(updated["providers"]["cline"], initial["providers"]["cline"]);
    assert_eq!(
        updated["providers"]["cline-pass"]["settings"]["auth"],
        auth(&new)
    );
    assert_eq!(updated["lastUsedProvider"], "cline-pass");
}

#[tokio::test]
async fn a_persisted_new_pair_heals_its_old_native_binding_without_adopting_old_tokens() {
    let fixture = Fixture::new();
    let old = credential("old", chrono::Utc::now().timestamp_millis() + 7_200_000);
    let new = credential("new", old.expires_at - 3_600_000);
    let initial = state(&old);
    let path = fixture.providers(&initial);
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    let current = session
        .credentials(&new, Some(&cline_accounts::pair(&old)))
        .unwrap()
        .unwrap();
    assert_eq!(current.access_token, new.access_token);
    assert!(session.synchronize(&current).unwrap());
    let updated = read(&path);
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"],
        auth(&new)
    );
    assert_eq!(
        updated["providers"]["cline"]["settings"]["model"],
        initial["providers"]["cline"]["settings"]["model"]
    );
    assert_eq!(updated["providers"]["other"], initial["providers"]["other"]);
    assert_eq!(updated["modes"], initial["modes"]);
    assert_eq!(updated["lastUsedProvider"], initial["lastUsedProvider"]);
}

#[tokio::test]
async fn an_independent_pass_grant_and_unrelated_auth_metadata_are_preserved_during_refresh() {
    let fixture = Fixture::new();
    let old = credential("old", chrono::Utc::now().timestamp_millis() + 7_200_000);
    let independent = credential("independent-pass", old.expires_at + 3_600_000);
    let new = credential("new", old.expires_at - 1000);
    let mut initial = state(&old);
    initial["providers"]["cline"]["settings"]["auth"]["metadata"]["organization"] =
        json!({"id":"kept-org"});
    initial["providers"]["cline-pass"]["settings"]["auth"] = auth(&independent);
    let path = fixture.providers(&initial);
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    assert!(session
        .credentials(&old, Some(&cline_accounts::pair(&old)))
        .unwrap()
        .is_some());
    assert!(session.synchronize(&new).unwrap());
    let updated = read(&path);
    assert_eq!(
        updated["providers"]["cline-pass"],
        initial["providers"]["cline-pass"]
    );
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"]["metadata"]["organization"],
        json!({"id":"kept-org"})
    );
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"]["metadata"]["userInfo"],
        new.user_info
    );
    assert_eq!(updated["providers"]["other"], initial["providers"]["other"]);
    assert_eq!(updated["lastUsedProvider"], "other");
}

#[tokio::test]
async fn an_unbound_saved_login_does_not_adopt_or_overwrite_another_native_grant() {
    let fixture = Fixture::new();
    let saved = credential("saved", chrono::Utc::now().timestamp_millis() + 3_600_000);
    let native = credential("native", saved.expires_at + 3_600_000);
    let path = fixture.providers(&state(&native));
    let before = fs::read(&path).unwrap();
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    assert!(session.credentials(&saved, None).unwrap().is_none());
    assert!(!session.synchronize(&saved).unwrap());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn changed_native_auth_after_read_cannot_be_clobbered_by_synchronization() {
    let fixture = Fixture::new();
    let old = credential("old", chrono::Utc::now().timestamp_millis() + 3_600_000);
    let new = credential("new", old.expires_at + 1000);
    let path = fixture.providers(&state(&old));
    let mut session = RefreshSession::open_at(&path).await.unwrap();
    session
        .credentials(&old, Some(&cline_accounts::pair(&old)))
        .unwrap();
    let mut changed = read(&path);
    changed["providers"]["cline"]["settings"]["auth"] =
        auth(&credential("external-login", old.expires_at));
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(
        session.synchronize(&new).err().unwrap(),
        "accountError.invalidAccount"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn failed_account_apply_restores_native_credentials_and_desktop_selection() {
    let fixture = Fixture::new();
    let old = credential("old", chrono::Utc::now().timestamp_millis() + 3_600_000);
    let new = credential("new", old.expires_at + 1000);
    let path = fixture.providers(&state(&old));
    let before = fs::read(&path).unwrap();
    let original_selection = selection();
    let storage = fixture.storage(&original_selection);
    let session = RefreshSession::open_at(&path).await.unwrap();
    let apply = begin_apply(&session, &new, Some(storage)).unwrap();
    assert_eq!(
        read(&path)["providers"]["cline"]["settings"]["auth"],
        auth(&new)
    );
    // The native transaction remains uncommitted until saving the selected EchoBird account succeeds.
    let save_result: Result<(), String> = Err("accountError.write".into());
    assert!(save_result.map(|()| apply.commit()).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let mut storage = Storage::open_at(&fixture.0.join("localstorage.sqlite3")).unwrap();
    assert_eq!(storage.read().unwrap(), original_selection);
    let db = rusqlite::Connection::open(fixture.0.join("localstorage.sqlite3")).unwrap();
    let untouched: Vec<u8> = db
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'unrelated-session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(untouched, b"leave alone");
}

#[tokio::test]
async fn successful_account_apply_preserves_models_modes_and_other_provider_selectors() {
    let fixture = Fixture::new();
    let old = credential("old", chrono::Utc::now().timestamp_millis() + 3_600_000);
    let new = credential("new", old.expires_at + 1000);
    let initial = state(&old);
    let path = fixture.providers(&initial);
    let original_selection = selection();
    let storage = fixture.storage(&original_selection);
    let session = RefreshSession::open_at(&path).await.unwrap();
    begin_apply(&session, &new, Some(storage)).unwrap().commit();
    let updated = read(&path);
    assert_eq!(updated["lastUsedProvider"], "cline");
    for field in [
        "model",
        "baseUrl",
        "apiKey",
        "protocol",
        "client",
        "reasoningEffort",
        "headers",
    ] {
        assert_eq!(
            updated["providers"]["cline"]["settings"][field],
            initial["providers"]["cline"]["settings"][field],
            "{field}"
        );
    }
    assert_eq!(
        updated["providers"]["cline-pass"],
        initial["providers"]["cline-pass"]
    );
    assert_eq!(updated["providers"]["other"], initial["providers"]["other"]);
    for field in ["modes", "sessionIds", "extra"] {
        assert_eq!(updated[field], initial[field]);
    }
    let mut storage = Storage::open_at(&fixture.0.join("localstorage.sqlite3")).unwrap();
    let selected = storage.read().unwrap();
    assert_eq!(selected["lastProvider"], "cline");
    assert_eq!(
        selected["lastModelByProvider"],
        original_selection["lastModelByProvider"]
    );
    assert_eq!(
        selected["otherSelectionMetadata"],
        original_selection["otherSelectionMetadata"]
    );
}

#[test]
fn replacing_an_account_does_not_retain_previous_user_identity_and_preserves_pass_selection() {
    let old = credential("old", 1_893_456_000_000);
    let mut new = credential("new", 1_893_456_001_000);
    new.user_id = "another-user".into();
    new.user_info = json!({"clineUserId":"another-user","subject":"another-subject"});
    let mut initial = state(&old);
    initial["lastUsedProvider"] = json!("cline-pass");
    let updated = account_state(initial, &new).unwrap();
    assert_eq!(updated["lastUsedProvider"], "cline-pass");
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"]["accountId"],
        "another-user"
    );
    assert_eq!(
        updated["providers"]["cline"]["settings"]["auth"]["metadata"]["userInfo"],
        new.user_info
    );
    assert!(!updated["providers"]["cline"]["settings"]["auth"]
        .to_string()
        .contains("subject-user"));
}

#[test]
fn native_lock_path_uses_lexical_resolution_without_changing_windows_runtime_identity() {
    let root = std::env::temp_dir();
    assert_eq!(
        resolved_path(&root.join("nested/../providers.json")).unwrap(),
        root.join("providers.json")
    );
    #[cfg(windows)]
    assert!(!resolved_path(&root.join("providers.json"))
        .unwrap()
        .to_string_lossy()
        .starts_with(r"\\?\"));
}
