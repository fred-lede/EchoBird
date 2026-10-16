use super::{text, Credentials, Edition};
#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
use aes_gcm::{
    aead::{Aead, Payload},
    Aes256Gcm, KeyInit, Nonce,
};
use fs2::FileExt;
use rand::RngCore;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

const IDENTITIES: [&str; 4] = ["external_idp", "builder_id", "identity_center", "social"];
const CLI_KEYS: [&str; 3] = [
    "kirocli:external-idp:token",
    "kirocli:odic:token",
    "kirocli:social:token",
];

pub(super) fn cli_paths(home: &Path, os: &str) -> Vec<PathBuf> {
    match os {
        "windows" => vec![
            home.join("AppData/Local/Kiro-Cli/data.sqlite3"),
            home.join("AppData/Roaming/kiro-cli/data.sqlite3"),
        ],
        "macos" => vec![home.join("Library/Application Support/kiro-cli/data.sqlite3")],
        _ => vec![home.join(".local/share/kiro-cli/data.sqlite3")],
    }
}

fn modified(path: &Path) -> SystemTime {
    let file = fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let wal = fs::metadata(format!("{}-wal", path.display()))
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    file.max(wal)
}

fn cli_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("accountError.home")?;
    let paths = cli_paths(&home, std::env::consts::OS);
    Ok(select_cli_path(&paths))
}

fn select_cli_path(paths: &[PathBuf]) -> PathBuf {
    paths
        .iter()
        .filter(|path| path.exists())
        .fold(None, |latest: Option<&PathBuf>, candidate| {
            Some(match latest {
                Some(path) if modified(path) >= modified(candidate) => path,
                _ => candidate,
            })
        })
        .cloned()
        .unwrap_or_else(|| paths[0].clone())
}

fn crew_home() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("accountError.home")?;
    if let Some(path) = std::env::var_os("KIROCREW_HOME").map(PathBuf::from) {
        if !path.is_absolute() {
            return Err("accountError.format".into());
        }
        return Ok(path);
    }
    Ok(home.join(".kiro/crew"))
}

pub(super) fn read(edition: Edition) -> Result<Option<Credentials>, String> {
    if edition == Edition::Crew {
        if let Some(credentials) = read_crew(&crew_home()?.join("kas"))? {
            return Ok(Some(credentials));
        }
    }
    read_cli(&cli_path()?)
}

fn json_row(db: &Connection, table: &str, key: &str) -> Result<Option<Value>, String> {
    let raw: Option<String> = db
        .query_row(
            &format!("SELECT value FROM {table} WHERE key = ?1"),
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| "accountError.read")?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(|_| "accountError.format".into()))
        .transpose()
}

fn read_cli(path: &Path) -> Result<Option<Credentials>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| "accountError.read")?;
    db.busy_timeout(Duration::from_secs(3))
        .map_err(|_| "accountError.read")?;
    let mut value = read_cli_db(&db)?;
    if let Some(credentials) = &mut value {
        credentials.source_path = Some(path.into());
    }
    Ok(value)
}

fn read_cli_db(db: &Connection) -> Result<Option<Credentials>, String> {
    for key in CLI_KEYS {
        let Some(mut token) = json_row(db, "auth_kv", key)? else {
            continue;
        };
        if text(&token, "access_token").is_none() {
            continue;
        }
        let registration = if key == "kirocli:odic:token" {
            json_row(db, "auth_kv", "kirocli:odic:device-registration")?
        } else {
            None
        };
        if key == "kirocli:odic:token" && text(&token, "provider").is_none() {
            token["provider"] = json!(if text(&token, "start_url")
                .is_some_and(|url| url != "https://view.awsapps.com/start")
            {
                "Enterprise"
            } else {
                "BuilderId"
            });
        }
        if key == "kirocli:external-idp:token" {
            token["provider"] = json!("ExternalIdp");
        }
        return Ok(Some(Credentials {
            name: text(&token["extra"], "email")
                .filter(|value| value.contains('@'))
                .map(str::to_owned),
            token,
            registration,
            subject: None,
            usage: None,
            source: Some(Edition::Cli),
            source_path: None,
        }));
    }
    Ok(None)
}

fn unlinked(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("accountError.read".into());
            }
        }
        if metadata.file_type().is_symlink() {
            return Err("accountError.read".into());
        }
    }
    Ok(())
}

fn envelope(dir: &Path) -> Result<Value, String> {
    unlinked(dir)?;
    unlinked(&dir.join(".vault"))?;
    let path = dir.join(".vault/secrets.enc");
    unlinked(&path)?;
    if !path.exists() {
        return Ok(json!({"version":1,"backend":"file","entries":{}}));
    }
    let value: Value = super::super::cursor_auth::read(&path)?;
    if value["version"] != 1 || value["backend"] != "file" || !value["entries"].is_object() {
        return Err("accountError.format".into());
    }
    Ok(value)
}

fn aad(identity: &str) -> Vec<u8> {
    format!("v1kiro_crew\0{identity}").into_bytes()
}

fn decode(entry: &Value, identity: &str, key: &[u8]) -> Result<Value, String> {
    let nonce = hex::decode(text(entry, "nonce").ok_or("accountError.format")?)
        .map_err(|_| "accountError.format")?;
    let ct = hex::decode(text(entry, "ct").ok_or("accountError.format")?)
        .map_err(|_| "accountError.format")?;
    if nonce.len() != 12 {
        return Err("accountError.format".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "accountError.keychain")?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ct,
                aad: &aad(identity),
            },
        )
        .map_err(|_| "accountError.keychain")?;
    serde_json::from_slice(&plaintext).map_err(|_| "accountError.format".into())
}

fn read_crew(dir: &Path) -> Result<Option<Credentials>, String> {
    let value = envelope(dir)?;
    for identity in IDENTITIES {
        let Some(entry) = value["entries"].get(identity) else {
            continue;
        };
        let path = dir.join(".vault/.vault_key");
        unlinked(&path)?;
        let key = fs::read(&path).map_err(|_| "accountError.keychain")?;
        let token = decode(entry, identity, &key)?;
        if text(&token, "access_token").is_none() {
            continue;
        }
        let registration = text(&token, "client_id")
            .map(|id| json!({"client_id":id,"client_secret":token["client_secret"]}));
        let credentials = Credentials {
            subject: text(&token["extra"], "user_id").map(str::to_owned),
            name: text(&token["extra"], "email")
                .filter(|value| value.contains('@'))
                .map(str::to_owned),
            token,
            registration,
            usage: None,
            source: Some(Edition::Crew),
            source_path: Some(dir.into()),
        };
        return Ok((credentials
            .expiry()
            .is_some_and(|expiry| expiry > chrono::Utc::now().timestamp() + 180)
            || text(&credentials.token, "refresh_token").is_some())
        .then_some(credentials));
    }
    Ok(None)
}

#[cfg(test)]
fn write_cli(path: &Path, credentials: &Credentials, switching: bool) -> Result<(), String> {
    if !path.exists() {
        return Err("accountError.initializeClient".into());
    }
    let mut db = Connection::open(path).map_err(|_| "accountError.write")?;
    db.busy_timeout(Duration::from_secs(3))
        .map_err(|_| "accountError.write")?;
    let tx = db.transaction().map_err(|_| "accountError.write")?;
    write_cli_db(&tx, credentials, switching)?;
    tx.commit().map_err(|_| "accountError.write".into())
}

fn write_cli_db(tx: &Connection, credentials: &Credentials, switching: bool) -> Result<(), String> {
    if switching {
        for key in CLI_KEYS {
            tx.execute("DELETE FROM auth_kv WHERE key = ?1", [key])
                .map_err(|_| "accountError.write")?;
        }
        tx.execute(
            "DELETE FROM auth_kv WHERE key = ?1",
            ["kirocli:odic:device-registration"],
        )
        .map_err(|_| "accountError.write")?;
    }
    let kind = if credentials.is_social() {
        "social"
    } else if credentials.provider().eq_ignore_ascii_case("ExternalIdp") {
        "external-idp"
    } else {
        "odic"
    };
    let key = format!("kirocli:{kind}:token");
    let mut token = credentials.token.clone();
    if credentials.profile().is_none() {
        token
            .as_object_mut()
            .ok_or("accountError.format")?
            .remove("profile_arn");
    }
    tx.execute(
        "INSERT OR REPLACE INTO auth_kv (key,value) VALUES (?1,?2)",
        params![key, token.to_string()],
    )
    .map_err(|_| "accountError.write")?;
    if !switching {
        return Ok(());
    }
    if let Some(registration) = &credentials.registration {
        tx.execute(
            "INSERT OR REPLACE INTO auth_kv (key,value) VALUES (?1,?2)",
            params!["kirocli:odic:device-registration", registration.to_string()],
        )
        .map_err(|_| "accountError.write")?;
    }
    for key in ["auth.idc.start-url", "auth.idc.region"] {
        tx.execute("DELETE FROM state WHERE key = ?1", [key])
            .map_err(|_| "accountError.write")?;
    }
    if kind == "odic" && credentials.provider().eq_ignore_ascii_case("Enterprise") {
        for (key, value) in [
            ("auth.idc.start-url", credentials.token["start_url"].clone()),
            ("auth.idc.region", json!(credentials.auth_region()?)),
        ] {
            if !value.is_null() {
                tx.execute(
                    "INSERT OR REPLACE INTO state (key,value) VALUES (?1,?2)",
                    params![key, value.to_string()],
                )
                .map_err(|_| "accountError.write")?;
            }
        }
    }
    let existing_profile =
        json_row(tx, "state", "api.codewhisperer.profile")?.filter(Value::is_object);
    if credentials.profile().is_some() || existing_profile.is_some() {
        let mut value = existing_profile.unwrap_or_else(|| json!({}));
        if let Some(profile) = credentials.profile() {
            value["arn"] = json!(profile);
            if let Some(name) = &credentials.name {
                value["profile_name"] = json!(name);
            }
        } else {
            let profile = value.as_object_mut().ok_or("accountError.format")?;
            profile.remove("arn");
            profile.remove("profile_name");
        }
        tx.execute(
            "INSERT OR REPLACE INTO state (key,value) VALUES (?1,?2)",
            params!["api.codewhisperer.profile", value.to_string()],
        )
        .map_err(|_| "accountError.write")?;
    }
    if let Some(subject) = &credentials.subject {
        tx.execute(
            "INSERT OR REPLACE INTO state (key,value) VALUES (?1,?2)",
            params!["telemetryUserId", json!(subject).to_string()],
        )
        .map_err(|_| "accountError.write")?;
    }
    Ok(())
}

fn private_dir(path: &Path) -> Result<(), String> {
    unlinked(path)?;
    fs::create_dir_all(path).map_err(|_| "accountError.write")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "accountError.write")?;
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").map_err(|_| "accountError.keychain")?;
        let domain = std::env::var("USERDOMAIN").unwrap_or_default();
        let principal = if domain.is_empty() {
            user
        } else {
            format!("{domain}\\{user}")
        };
        let result = crate::utils::process::command("icacls")
            .arg(path)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{principal}:(OI)(CI)F"),
            ])
            .output()
            .map_err(|_| "accountError.keychain")?;
        if !result.status.success() {
            return Err("accountError.keychain".into());
        }
    }
    Ok(())
}

#[cfg(test)]
fn write_crew(dir: &Path, credentials: &Credentials, switching: bool) -> Result<(), String> {
    credentials.access()?;
    private_dir(dir)?;
    private_dir(&dir.join(".vault"))?;
    let _locks = crew_locks(dir)?;
    write_crew_locked(dir, credentials, switching)
}

fn crew_locks(dir: &Path) -> Result<Vec<fs::File>, String> {
    unlinked(&dir.join(".vault"))?;
    let locks = IDENTITIES
        .iter()
        .map(|identity| {
            let path = dir.join(format!("refresh-{identity}.lock"));
            unlinked(&path)?;
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)
                .map_err(|_| "accountError.write")?;
            lock_file(&file)?;
            Ok(file)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let lock_path = dir.join(".vault/.secrets.enc.lock");
    unlinked(&lock_path)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|_| "accountError.write")?;
    lock_file(&lock)?;
    let mut locks = locks;
    locks.push(lock);
    Ok(locks)
}

fn write_crew_locked(dir: &Path, credentials: &Credentials, switching: bool) -> Result<(), String> {
    let mut value = envelope(dir)?;
    let key_path = dir.join(".vault/.vault_key");
    unlinked(&key_path)?;
    let key = if key_path.exists() {
        fs::read(&key_path).map_err(|_| "accountError.keychain")?
    } else {
        if dir.join(".vault/secrets.enc").exists() {
            return Err("accountError.keychain".into());
        }
        let mut key = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&key_path)
            .map_err(|_| "accountError.keychain")?;
        file.write_all(&key)
            .and_then(|()| file.sync_all())
            .map_err(|_| "accountError.keychain")?;
        key
    };
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "accountError.keychain")?;
    let identity = if credentials.is_social() {
        "social"
    } else if credentials.provider().eq_ignore_ascii_case("ExternalIdp") {
        "external_idp"
    } else if credentials.provider().eq_ignore_ascii_case("Enterprise") {
        "identity_center"
    } else {
        "builder_id"
    };
    let mut token = json!({});
    for key in [
        "access_token",
        "expires_at",
        "refresh_token",
        "profile_arn",
        "region",
        "auth_method",
        "client_id",
        "client_secret",
        "token_endpoint",
        "extra",
    ] {
        if key == "profile_arn" && credentials.profile().is_none() {
            continue;
        }
        if let Some(value) = credentials.token.get(key) {
            token[key] = value.clone();
        }
    }
    token["identity"] = json!(identity);
    token["provider"] = json!(match credentials.provider().to_ascii_lowercase().as_str() {
        "google" => "Google",
        "github" => "Github",
        _ => credentials.provider(),
    });
    if !token["extra"].is_object() {
        token["extra"] = json!({});
    }
    if let Some(subject) = &credentials.subject {
        token["extra"]["user_id"] = json!(subject);
    }
    if let Some(name) = &credentials.name {
        token["extra"]["email"] = json!(name);
    }
    if let Some(registration) = &credentials.registration {
        token["client_id"] = registration["client_id"].clone();
        token["client_secret"] = registration["client_secret"].clone();
    }
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: token.to_string().as_bytes(),
                aad: &aad(identity),
            },
        )
        .map_err(|_| "accountError.keychain")?;
    if switching {
        for identity in IDENTITIES {
            value["entries"]
                .as_object_mut()
                .ok_or("accountError.format")?
                .remove(identity);
        }
    }
    value["entries"][identity] = json!({"nonce":hex::encode(nonce),"ct":hex::encode(ct)});
    super::super::cursor_auth::write(&dir.join(".vault/secrets.enc"), &value)?;
    for name in if switching {
        IDENTITIES.to_vec()
    } else {
        vec![identity]
    } {
        let path = dir.join(format!("refresh-rejected-{name}"));
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn lock_file(file: &fs::File) -> Result<(), String> {
    let start = Instant::now();
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(()),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && start.elapsed() < Duration::from_secs(3) =>
            {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(_) => return Err("accountError.busy".into()),
        }
    }
}

pub(super) fn binding_key(edition: Edition, path: &Path) -> String {
    format!("{}:{}", edition.tool(), path.display())
}

pub(super) fn target_for_apply(edition: Edition) -> Result<PathBuf, String> {
    if edition == Edition::Crew {
        Ok(crew_home()?.join("kas"))
    } else {
        cli_path()
    }
}

pub(super) fn targets() -> Result<Vec<(Edition, PathBuf)>, String> {
    let mut targets = Vec::new();
    for edition in [Edition::Crew, Edition::Cli] {
        if !super::installed(edition) {
            continue;
        }
        if edition == Edition::Crew {
            let path = crew_home()?.join("kas");
            if path.join(".vault/secrets.enc").exists() {
                targets.push((Edition::Crew, path));
            } else {
                let path = cli_path()?;
                if path.exists() && !targets.iter().any(|(_, existing)| existing == &path) {
                    targets.push((Edition::Cli, path));
                }
            }
        } else {
            let path = cli_path()?;
            if path.exists() && !targets.iter().any(|(_, existing)| existing == &path) {
                targets.push((Edition::Cli, path));
            }
        }
    }
    Ok(targets)
}

enum Guard {
    Cli(Connection),
    Crew(Vec<fs::File>),
}

pub(super) struct Session {
    pub(super) edition: Edition,
    pub(super) path: PathBuf,
    guard: Guard,
}

impl Session {
    pub(super) fn open_for_apply(edition: Edition, path: &Path) -> Result<Self, String> {
        if edition == Edition::Crew && !path.join(".vault/secrets.enc").exists() {
            private_dir(path)?;
            private_dir(&path.join(".vault"))?;
            return Ok(Self {
                edition,
                path: path.into(),
                guard: Guard::Crew(crew_locks(path)?),
            });
        }
        Self::open(edition, path)?.ok_or_else(|| "accountError.initializeClient".into())
    }
    pub(super) fn open(edition: Edition, path: &Path) -> Result<Option<Self>, String> {
        unlinked(path)?;
        let guard = if edition == Edition::Cli {
            if !path.exists() {
                return Ok(None);
            }
            let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
                .map_err(|_| "accountError.read")?;
            db.busy_timeout(Duration::from_secs(3))
                .map_err(|_| "accountError.read")?;
            db.execute_batch("BEGIN IMMEDIATE")
                .map_err(|_| "accountError.busy")?;
            Guard::Cli(db)
        } else {
            if !path.join(".vault/secrets.enc").exists() {
                return Ok(None);
            }
            Guard::Crew(crew_locks(path)?)
        };
        Ok(Some(Self {
            edition,
            path: path.into(),
            guard,
        }))
    }

    pub(super) fn key(&self) -> String {
        binding_key(self.edition, &self.path)
    }

    pub(super) fn read(&self) -> Result<Option<Credentials>, String> {
        let mut credentials = match &self.guard {
            Guard::Cli(db) => read_cli_db(db)?,
            Guard::Crew(_) => read_crew(&self.path)?,
        };
        if let Some(credentials) = &mut credentials {
            credentials.source_path = Some(self.path.clone());
        }
        Ok(credentials)
    }

    pub(super) fn write(
        &mut self,
        credentials: &Credentials,
        switching: bool,
    ) -> Result<(), String> {
        match &mut self.guard {
            Guard::Cli(db) => {
                write_cli_db(db, credentials, switching)?;
                db.execute_batch("COMMIT; BEGIN IMMEDIATE")
                    .map_err(|_| "accountError.write".into())
            }
            Guard::Crew(locks) => {
                if locks.is_empty() {
                    return Err("accountError.busy".into());
                }
                write_crew_locked(&self.path, credentials, switching)
            }
        }
    }

    pub(super) fn begin_apply(&mut self, credentials: &Credentials) -> Result<Apply<'_>, String> {
        let original = match &self.guard {
            Guard::Cli(db) => {
                db.execute_batch("SAVEPOINT echobird_kiro_apply")
                    .map_err(|_| "accountError.write")?;
                Vec::new()
            }
            Guard::Crew(_) => {
                let mut paths = vec![
                    self.path.join(".vault/secrets.enc"),
                    self.path.join(".vault/.vault_key"),
                ];
                paths.extend(
                    IDENTITIES
                        .iter()
                        .map(|identity| self.path.join(format!("refresh-rejected-{identity}"))),
                );
                paths
                    .into_iter()
                    .map(|path| {
                        unlinked(&path)?;
                        let bytes = super::super::native_account_files::read_optional(&path)?;
                        Ok((path, bytes))
                    })
                    .collect::<Result<Vec<_>, String>>()?
            }
        };
        let apply = Apply {
            session: self,
            original,
            committed: false,
        };
        match &apply.session.guard {
            Guard::Cli(db) => write_cli_db(db, credentials, true)?,
            Guard::Crew(_) => write_crew_locked(&apply.session.path, credentials, true)?,
        }
        Ok(apply)
    }
}

pub(super) struct Apply<'a> {
    session: &'a mut Session,
    original: Vec<(PathBuf, Option<Vec<u8>>)>,
    committed: bool,
}

impl Apply<'_> {
    pub(super) fn commit(mut self) -> Result<(), String> {
        if let Guard::Cli(db) = &self.session.guard {
            db.execute_batch("RELEASE echobird_kiro_apply; COMMIT")
                .map_err(|_| "accountError.write")?;
        }
        self.committed = true;
        Ok(())
    }
}

impl Drop for Apply<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let Guard::Cli(db) = &self.session.guard {
            if db
                .execute_batch("ROLLBACK TO echobird_kiro_apply; RELEASE echobird_kiro_apply")
                .is_err()
            {
                log::error!("Failed to restore native account selection");
            }
        } else {
            for (path, original) in self.original.iter().rev() {
                let restored = match original {
                    Some(bytes) => super::super::native_account_files::write(path, bytes),
                    None => match fs::remove_file(path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(_) => Err("accountError.write".into()),
                    },
                };
                if restored.is_err() {
                    log::error!("Failed to restore native account selection");
                }
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Guard::Cli(db) = &self.guard {
            let _ = db.execute_batch("ROLLBACK");
        }
    }
}

#[cfg(test)]
pub(super) fn fixture_write(
    edition: Edition,
    path: &Path,
    credentials: &Credentials,
) -> Result<(), String> {
    if edition == Edition::Crew {
        write_crew(path, credentials, true)
    } else {
        write_cli(path, credentials, true)
    }
}
