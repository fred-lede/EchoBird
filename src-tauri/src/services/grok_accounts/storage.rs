use super::*;
use std::{fs::File, io::Write, time::Duration};

pub(super) mod credential {
    use super::*;
    use serde::{de::Error as _, ser::Error as _, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(auth: &Value, serializer: S) -> Result<S::Ok, S::Error> {
        let raw = serde_json::to_string(auth).map_err(S::Error::custom)?;
        let encrypted = encrypt(&raw);
        if !encrypted.starts_with("enc:v1:") {
            return Err(S::Error::custom("accountError.keychain"));
        }
        serializer.serialize_str(&encrypted)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Value, D::Error> {
        let encoded = Value::deserialize(deserializer)?;
        if encoded.is_object() {
            return Ok(encoded);
        }
        let encrypted = encoded
            .as_str()
            .filter(|value| value.starts_with("enc:v1:"))
            .ok_or_else(|| D::Error::custom("accountError.keychain"))?;
        serde_json::from_str(&decrypt(encrypted)).map_err(D::Error::custom)
    }

    #[cfg(not(test))]
    fn encrypt(raw: &str) -> String {
        super::super::super::model_manager::encrypt_key_for_storage(raw)
    }
    #[cfg(not(test))]
    fn decrypt(encoded: &str) -> String {
        super::super::super::model_manager::decrypt_key_for_use(encoded)
    }
    #[cfg(test)]
    fn encrypt(raw: &str) -> String {
        use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit};
        use rand::RngCore;
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let mut bytes = nonce.to_vec();
        bytes.extend(
            Aes256Gcm::new(&[11u8; 32].into())
                .encrypt(aes_gcm::Nonce::from_slice(&nonce), raw.as_bytes())
                .unwrap(),
        );
        format!("enc:v1:{}", hex::encode(bytes))
    }
    #[cfg(test)]
    fn decrypt(encoded: &str) -> String {
        use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit};
        hex::decode(encoded.trim_start_matches("enc:v1:"))
            .ok()
            .filter(|bytes| bytes.len() >= 28)
            .and_then(|bytes| {
                Aes256Gcm::new(&[11u8; 32].into())
                    .decrypt(aes_gcm::Nonce::from_slice(&bytes[..12]), &bytes[12..])
                    .ok()
            })
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .unwrap_or_default()
    }
}

pub(super) struct NativeLock {
    file: File,
    path: PathBuf,
}

impl NativeLock {
    pub(super) async fn acquire(path: &Path, create: bool) -> Result<Option<Self>, String> {
        if !create && !path.exists() {
            return Ok(None);
        }
        let parent = path.parent().ok_or("accountError.home")?;
        if create {
            super::super::native_account_files::private_dir(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path.with_file_name("auth.json.lock"))
            .map_err(|_| "accountError.write")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => {
                    let mut held = Self {
                        file,
                        path: path.with_file_name("auth.json.lock"),
                    };
                    held.ensure_current()?;
                    // Keep the native holder stamp fresh; the OS lock is never broken or removed.
                    held.file.set_len(0).map_err(|_| "accountError.write")?;
                    write!(
                        held.file,
                        "{}:{}",
                        std::process::id(),
                        chrono::Utc::now().timestamp()
                    )
                    .map_err(|_| "accountError.write")?;
                    held.file.sync_all().map_err(|_| "accountError.write")?;
                    return Ok(Some(held));
                }
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
                {
                    if tokio::time::Instant::now() >= deadline {
                        return Err("accountError.busy".into());
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(_) => return Err("accountError.write".into()),
            }
        }
    }
    pub(super) fn ensure_current(&self) -> Result<(), String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let held = self.file.metadata().map_err(|_| "accountError.busy")?;
            let path = fs::metadata(&self.path).map_err(|_| "accountError.busy")?;
            if held.ino() != path.ino() || held.dev() != path.dev() {
                return Err("accountError.busy".into());
            }
        }
        #[cfg(not(unix))]
        let _ = &self.path;
        Ok(())
    }
}

impl Drop for NativeLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn pair(auth: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{}\0{}", auth["key"], auth["refresh_token"]))
    )
}

pub(super) fn same_identity(left: &Saved, auth: &Value) -> bool {
    left.auth["oidc_issuer"] == auth["oidc_issuer"]
        && left.auth["user_id"] == auth["user_id"]
        && left.auth["principal_type"] == auth["principal_type"]
        && left.auth["principal_id"] == auth["principal_id"]
}

pub(super) fn valid_native(saved: &Saved, auth: &Value) -> bool {
    if !auth.is_object() || !same_identity(saved, auth) {
        return false;
    }
    let mut candidate = saved.clone();
    candidate.auth = auth.clone();
    proxy::valid(&candidate, &candidate.summary.id).is_ok()
}

pub(super) fn native_target(path: &Path) -> Result<PathBuf, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::canonicalize(path).map_err(|_| "accountError.read".into())
        }
        Ok(_) => Ok(path.into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.into()),
        Err(_) => Err("accountError.read".into()),
    }
}

pub(super) fn apply_at(
    dir: &Path,
    native_path: &Path,
    cache: &Path,
    saved: &Saved,
) -> Result<Account, String> {
    use super::super::native_account_files::{self as files, Transaction};
    let previous_cache = files::read_optional(cache)?;
    let native_file = native_target(native_path)?;
    let mut current = files::object(native_path)?;
    let mut rows = Vec::new();
    let mut selected = None;
    for entry in fs::read_dir(dir).map_err(|_| "accountError.read")? {
        let path = entry.map_err(|_| "accountError.read")?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let mut row = proxy::current(&path)?;
        let native = &current[&row.key];
        let native_pair = pair(native);
        let own_pair = pair(&row.auth);
        let bound = row.native_pair.is_some() || row.summary.active || native_pair == own_pair;
        if !valid_native(&row, native) {
            row.native_pair = None;
        } else if bound {
            // A matching baseline keeps our newer pair after a failed native write.
            if row.native_pair.as_ref() != Some(&native_pair) || own_pair == native_pair {
                row.auth = native.clone();
                row.native_pair = Some(native_pair);
            }
        }
        row.summary.active = row.summary.id == saved.summary.id;
        if row.summary.active {
            row.native_pair = Some(pair(&row.auth));
            proxy::valid(&row, &row.summary.id)?;
            selected = Some(row.clone());
        }
        rows.push((path, row));
    }
    let selected = selected.ok_or("accountError.invalidAccount")?;
    current[&selected.key] = selected.auth;
    let mut transaction = Transaction::new();
    for (path, row) in &rows {
        transaction.replace(
            path,
            &serde_json::to_vec_pretty(row).map_err(|_| "accountError.format")?,
        )?;
    }
    transaction.replace(&native_file, &files::json(&current)?)?;
    if previous_cache.is_some() {
        fs::remove_file(cache).map_err(|_| "accountError.write")?;
    }
    transaction.commit();
    for (path, _) in rows {
        proxy::forget_pending(&path);
    }
    Ok(selected.summary)
}
