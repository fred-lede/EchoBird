use super::*;

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!("echobird-kiro-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&path).unwrap();
    path
}
fn database(path: &Path) {
    Connection::open(path).unwrap().execute_batch("CREATE TABLE auth_kv (key TEXT PRIMARY KEY,value TEXT); CREATE TABLE state (key TEXT PRIMARY KEY,value TEXT); CREATE TABLE history (id TEXT PRIMARY KEY,value TEXT);").unwrap();
}
fn credentials() -> Credentials {
    super::super::tests::credentials()
}

#[test]
fn cli_profile_label_cannot_replace_the_account_identity() {
    let dir = directory();
    let path = dir.join("data.sqlite3");
    database(&path);
    write_cli(&path, &credentials(), true).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute(
        "UPDATE state SET value = ?1 WHERE key = ?2",
        params![
            r#"{"profile_name":"Social_Default_Profile"}"#,
            "api.codewhisperer.profile"
        ],
    )
    .unwrap();
    assert!(read_cli(&path).unwrap().unwrap().name.is_none());
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cli_native_switch_preserves_history_settings_and_unknown_rows() {
    let dir = directory();
    let path = dir.join("data.sqlite3");
    database(&path);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        params!["kirocli:odic:token", r#"{"access_token":"old"}"#],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        params!["kirocli:odic:device-registration", "{}"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        params!["unrelated", "{\"keep\":true}"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO state VALUES (?1,?2)",
        params!["chat.defaultModel", "\"auto\""],
    )
    .unwrap();
    db.execute(
        "INSERT INTO state VALUES (?1,?2)",
        params![
            "api.codewhisperer.profile",
            r#"{"arn":"old","profile_name":"old-name","device_code_disabled":true,"unknown":7}"#
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO history VALUES (?1,?2)",
        params!["session", "keep-session"],
    )
    .unwrap();
    drop(db);
    let credentials = credentials();
    write_cli(&path, &credentials, true).unwrap();
    let read = read_cli(&path).unwrap().unwrap();
    assert_eq!(read.access().unwrap(), "fixture-access");
    assert_eq!(read.identity(), credentials.identity());
    let db = Connection::open(&path).unwrap();
    assert!(json_row(&db, "auth_kv", "kirocli:odic:token")
        .unwrap()
        .is_none());
    assert!(json_row(&db, "auth_kv", "kirocli:odic:device-registration")
        .unwrap()
        .is_none());
    assert_eq!(
        json_row(&db, "auth_kv", "unrelated").unwrap(),
        Some(json!({"keep":true}))
    );
    assert_eq!(
        json_row(&db, "state", "chat.defaultModel").unwrap(),
        Some(json!("auto"))
    );
    let profile = json_row(&db, "state", "api.codewhisperer.profile")
        .unwrap()
        .unwrap();
    assert_eq!(profile["arn"], credentials.token["profile_arn"]);
    assert_eq!(profile["device_code_disabled"], true);
    assert_eq!(profile["unknown"], 7);
    assert_eq!(
        db.query_row(
            "SELECT value FROM history WHERE id = 'session'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "keep-session"
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn quota_token_write_updates_only_the_current_native_token() {
    let dir = directory();
    let path = dir.join("data.sqlite3");
    database(&path);
    let db = Connection::open(&path).unwrap();
    db.execute(
        "INSERT INTO auth_kv VALUES (?1,?2)",
        params!["kirocli:odic:token", r#"{"access_token":"other"}"#],
    )
    .unwrap();
    db.execute(
        "INSERT INTO state VALUES (?1,?2)",
        params!["telemetryUserId", "\"keep\""],
    )
    .unwrap();
    drop(db);
    write_cli(&path, &credentials(), false).unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        json_row(&db, "auth_kv", "kirocli:odic:token").unwrap(),
        Some(json!({"access_token":"other"}))
    );
    assert_eq!(
        json_row(&db, "state", "telemetryUserId").unwrap(),
        Some(json!("keep"))
    );
    drop(db);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn crew_native_vault_is_encrypted_and_keeps_the_native_token_schema() {
    let dir = directory();
    let kas = dir.join("kas");
    let mut credentials = credentials();
    credentials.token["extra"] = json!({"keep":7});
    write_crew(&kas, &credentials, true).unwrap();
    let raw = fs::read_to_string(kas.join(".vault/secrets.enc")).unwrap();
    assert!(!raw.contains("fixture-access"));
    assert!(!raw.contains("fixture-refresh"));
    let read = read_crew(&kas).unwrap().unwrap();
    assert_eq!(read.access().unwrap(), credentials.access().unwrap());
    assert_eq!(read.name, credentials.name);
    assert_eq!(read.token["provider"], "Google");
    assert_eq!(read.token["extra"]["keep"], 7);
    if let Ok(python) = std::env::var("ECHOBIRD_KIRO_SDK_PYTHON") {
        let script = "import os; from pathlib import Path; assert Path.home() == Path(os.environ['ECHOBIRD_KIRO_FIXTURE']); from kiro_crew.auth.store import TokenStore; token=TokenStore(os.environ['ECHOBIRD_KIRO_FIXTURE']).resolve(); assert token is not None and token.access_token == 'fixture-access'; assert token.extra['keep'] == 7; print('native SDK accepted fixture')";
        let output = crate::utils::process::command(python)
            .args(["-c", script])
            .env("ECHOBIRD_KIRO_FIXTURE", &dir)
            .env("KIROCREW_HOME", &dir)
            .env("HOME", &dir)
            .env("USERPROFILE", &dir)
            .env("APPDATA", dir.join("AppData/Roaming"))
            .env("LOCALAPPDATA", dir.join("AppData/Local"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "native SDK accepted fixture"
        );
    }
    assert!(read.token.get("telemetry_generation").is_none());
    assert!(read.token.get("start_url").is_none());
    let key = fs::read(kas.join(".vault/.vault_key")).unwrap();
    assert_eq!(key.len(), 32);
    let value: Value = serde_json::from_str(&raw).unwrap();
    assert!(decode(&value["entries"]["social"], "builder_id", &key).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(kas.join(".vault/.vault_key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    fs::write(kas.join("refresh-rejected-social"), "old").unwrap();
    credentials.token["access_token"] = json!("next-access");
    write_crew(&kas, &credentials, false).unwrap();
    assert!(!kas.join("refresh-rejected-social").exists());
    assert_eq!(fs::read(kas.join(".vault/.vault_key")).unwrap(), key);
    credentials.token["expires_at"] = json!("2020-01-01T00:00:00Z");
    credentials.token["refresh_token"] = Value::Null;
    write_crew(&kas, &credentials, false).unwrap();
    assert!(read_crew(&kas).unwrap().is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn crew_corrupt_store_never_replaces_a_missing_key() {
    let dir = directory();
    let kas = dir.join("kas");
    fs::create_dir_all(kas.join(".vault")).unwrap();
    let store = kas.join(".vault/secrets.enc");
    fs::write(
        &store,
        r#"{"version":1,"backend":"file","entries":{"social":{"nonce":"aa","ct":"bb"}}}"#,
    )
    .unwrap();
    let previous = fs::read(&store).unwrap();
    assert!(write_crew(&kas, &credentials(), true).is_err());
    assert!(!kas.join(".vault/.vault_key").exists());
    assert_eq!(fs::read(&store).unwrap(), previous);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_paths_cover_three_platforms_and_missing_reads_create_nothing() {
    let home = Path::new("fixture-home");
    assert!(cli_paths(home, "windows")[0].ends_with("AppData/Local/Kiro-Cli/data.sqlite3"));
    assert!(cli_paths(home, "windows")[1].ends_with("AppData/Roaming/kiro-cli/data.sqlite3"));
    assert!(
        cli_paths(home, "macos")[0].ends_with("Library/Application Support/kiro-cli/data.sqlite3")
    );
    assert!(cli_paths(home, "linux")[0].ends_with(".local/share/kiro-cli/data.sqlite3"));
    let dir = directory();
    let path = dir.join("missing.sqlite3");
    assert!(read_cli(&path).unwrap().is_none());
    assert!(!path.exists());
    assert!(read_crew(&dir.join("missing-kas")).unwrap().is_none());
    assert!(!dir.join("missing-kas").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn windows_legacy_store_uses_wal_recency_and_prefers_local_on_equal_dates() {
    let dir = directory();
    let local = dir.join("local.sqlite3");
    let roaming = dir.join("roaming.sqlite3");
    for path in [&local, &roaming] {
        fs::write(path, []).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10))
            .unwrap();
    }
    let paths = vec![local.clone(), roaming.clone()];
    assert_eq!(select_cli_path(&paths), local);
    let wal = PathBuf::from(format!("{}-wal", roaming.display()));
    fs::write(&wal, []).unwrap();
    assert_eq!(select_cli_path(&paths), roaming);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn refresh_sessions_hold_the_native_coordination_until_the_operation_finishes() {
    let dir = directory();
    let path = dir.join("data.sqlite3");
    database(&path);
    write_cli(&path, &credentials(), true).unwrap();
    let contender = Connection::open(&path).unwrap();
    contender.busy_timeout(Duration::from_millis(1)).unwrap();
    contender
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
    let session = Session::open(Edition::Cli, &path).unwrap().unwrap();
    assert!(contender.execute_batch("BEGIN IMMEDIATE").is_err());
    drop(session);
    contender
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .unwrap();
    drop(contender);
    let kas = dir.join("kas");
    write_crew(&kas, &credentials(), true).unwrap();
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(kas.join("refresh-social.lock"))
        .unwrap();
    contender.try_lock_exclusive().unwrap();
    FileExt::unlock(&contender).unwrap();
    let session = Session::open(Edition::Crew, &kas).unwrap().unwrap();
    assert!(contender.try_lock_exclusive().is_err());
    drop(session);
    contender.try_lock_exclusive().unwrap();
    FileExt::unlock(&contender).unwrap();
    drop(contender);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn profileless_builder_switch_omits_legacy_placeholders_and_clears_the_previous_account_profile() {
    let dir = directory();
    let path = dir.join("data.sqlite3");
    database(&path);
    write_cli(&path, &credentials(), true).unwrap();
    let db = Connection::open(&path).unwrap();
    let mut profile = json_row(&db, "state", "api.codewhisperer.profile")
        .unwrap()
        .unwrap();
    profile["unknown"] = json!(7);
    db.execute(
        "UPDATE state SET value=?1 WHERE key=?2",
        params![profile.to_string(), "api.codewhisperer.profile"],
    )
    .unwrap();
    drop(db);
    let mut builder = credentials();
    builder.token["provider"] = json!("BuilderId");
    builder.token["profile_arn"] = json!(super::super::LEGACY_BUILDER_PROFILE);
    write_cli(&path, &builder, true).unwrap();
    let db = Connection::open(&path).unwrap();
    let token = json_row(&db, "auth_kv", "kirocli:odic:token")
        .unwrap()
        .unwrap();
    assert!(token.get("profile_arn").is_none());
    let profile = json_row(&db, "state", "api.codewhisperer.profile")
        .unwrap()
        .unwrap();
    assert!(profile.get("arn").is_none());
    assert!(profile.get("profile_name").is_none());
    assert_eq!(profile["unknown"], 7);
    drop(db);
    let kas = dir.join("kas");
    write_crew(&kas, &builder, true).unwrap();
    let token = read_crew(&kas).unwrap().unwrap();
    assert!(token.token.get("profile_arn").is_none());
    assert_eq!(token.subject, builder.subject);
    fs::remove_dir_all(dir).unwrap();
}
