use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const MAX_RECORDS: usize = 200;
const MAX_TEXT_BYTES: usize = 2_000;
const MAX_STORE_BYTES: usize = 1024 * 1024;
const MAX_RECALL_RECORDS: usize = 8;
const MAX_RECALL_BYTES: usize = 8_000;
const MAX_CAPTURE_CHANGES: usize = 8;
static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryScope {
    User,
    Target,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub text: String,
    pub scope: MemoryScope,
    pub target: Option<String>,
    pub source_conversation_id: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum MemoryChange {
    Add { scope: MemoryScope, text: String },
    Update { id: String, text: String },
    Forget { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedMemories {
    version: u8,
    revision: u64,
    records: Vec<MemoryRecord>,
}

impl Default for SavedMemories {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            records: Vec::new(),
        }
    }
}

struct FileLock(File);

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub struct MemoryStore {
    root: PathBuf,
}

impl MemoryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn standard() -> Self {
        Self::new(crate::utils::platform::echobird_dir().join("agent-memory"))
    }

    pub fn list(&self) -> Result<Vec<MemoryRecord>, String> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Memory storage is unavailable")?;
        if !self.check_directory()? {
            return Ok(Vec::new());
        }
        let _file_lock = self.read_lock()?;
        let mut records = self.load(false)?.records;
        records.sort_by(|a, b| (b.updated_at, &b.id).cmp(&(a.updated_at, &a.id)));
        Ok(records)
    }

    #[cfg(test)]
    pub fn remember(
        &self,
        scope: MemoryScope,
        target: Option<&str>,
        text: &str,
        source_conversation_id: Option<&str>,
    ) -> Result<MemoryRecord, String> {
        let text = validate_text(text)?;
        validate_scope(scope, target)?;
        validate_source(source_conversation_id)?;
        self.mutate(|saved| {
            if let Some(existing) = saved.records.iter().find(|record| {
                record.scope == scope && record.target.as_deref() == target && record.text == text
            }) {
                return Ok(existing.clone());
            }
            if saved.records.len() >= MAX_RECORDS {
                return Err("Memory storage is full; delete a memory before saving another".into());
            }
            let now = unix_seconds();
            let record = MemoryRecord {
                id: uuid::Uuid::new_v4().to_string(),
                text: text.to_string(),
                scope,
                target: target.map(str::to_string),
                source_conversation_id: source_conversation_id.map(str::to_string),
                created_at: now,
                updated_at: now,
            };
            saved.records.push(record.clone());
            Ok(record)
        })
    }

    #[cfg(test)]
    pub fn update(&self, id: &str, text: &str) -> Result<MemoryRecord, String> {
        validate_id(id)?;
        let text = validate_text(text)?;
        self.mutate(|saved| {
            let record = saved
                .records
                .iter_mut()
                .find(|record| record.id == id)
                .ok_or("Memory does not exist")?;
            record.text = text.to_string();
            record.updated_at = unix_seconds();
            Ok(record.clone())
        })
    }

    #[cfg(test)]
    pub fn forget(&self, id: &str) -> Result<(), String> {
        validate_id(id)?;
        self.mutate(|saved| {
            let index = saved
                .records
                .iter()
                .position(|record| record.id == id)
                .ok_or("Memory does not exist")?;
            saved.records.remove(index);
            Ok(())
        })
    }

    pub(crate) fn apply_capture(
        &self,
        target: &str,
        expected: &[MemoryRecord],
        mut changes: Vec<MemoryChange>,
        source: &str,
    ) -> Result<(), String> {
        validate_scope(MemoryScope::Target, Some(target))?;
        validate_source(Some(source))?;
        if changes.len() > MAX_CAPTURE_CHANGES {
            return Err("Memory capture exceeds its change limit".into());
        }
        let mut expected: Vec<_> = expected.iter().collect();
        for record in &expected {
            validate_scope(record.scope, record.target.as_deref())?;
            if record.scope == MemoryScope::Target && record.target.as_deref() != Some(target) {
                return Err("Memory capture contains a different machine/server".into());
            }
        }
        expected.sort_by(|a, b| a.id.cmp(&b.id));
        let mut changed_ids = HashSet::new();
        for change in &mut changes {
            match change {
                MemoryChange::Add { text, .. } | MemoryChange::Update { text, .. } => {
                    *text = validate_text(text)?.to_string();
                }
                MemoryChange::Forget { .. } => {}
            }
            if let MemoryChange::Update { id, .. } | MemoryChange::Forget { id } = change {
                validate_id(id)?;
                if !expected.iter().any(|record| record.id == *id) {
                    return Err(
                        "Memory capture can only change notes in its original snapshot".into(),
                    );
                }
                if !changed_ids.insert(id.clone()) {
                    return Err("Memory capture changes the same note more than once".into());
                }
            }
        }
        self.mutate(|saved| {
            let mut current: Vec<_> = saved
                .records
                .iter()
                .filter(|record| {
                    record.scope == MemoryScope::User || record.target.as_deref() == Some(target)
                })
                .collect();
            current.sort_by(|a, b| a.id.cmp(&b.id));
            if current != expected {
                return Err("Saved memories changed while capturing; capture was skipped".into());
            }
            let before = saved.records.clone();
            let now = unix_seconds();
            let mut protected_ids = changed_ids;
            for change in changes {
                match change {
                    MemoryChange::Add { scope, text } => {
                        let target = (scope == MemoryScope::Target).then(|| target.to_string());
                        if let Some(existing) = saved.records.iter().find(|record| {
                            record.scope == scope && record.target == target && record.text == text
                        }) {
                            protected_ids.insert(existing.id.clone());
                            continue;
                        }
                        let id = uuid::Uuid::new_v4().to_string();
                        protected_ids.insert(id.clone());
                        saved.records.push(MemoryRecord {
                            id,
                            text,
                            scope,
                            target,
                            source_conversation_id: Some(source.to_string()),
                            created_at: now,
                            updated_at: now,
                        });
                    }
                    MemoryChange::Update { id, text } => {
                        let record = saved
                            .records
                            .iter_mut()
                            .find(|record| record.id == id)
                            .ok_or("Memory does not exist")?;
                        record.text = text;
                        record.updated_at = now;
                    }
                    MemoryChange::Forget { id } => {
                        let index = saved
                            .records
                            .iter()
                            .position(|record| record.id == id)
                            .ok_or("Memory does not exist")?;
                        saved.records.remove(index);
                    }
                }
            }
            if before == saved.records {
                return Ok(());
            }
            let next_revision = saved
                .revision
                .checked_add(1)
                .ok_or("Memory revision limit reached")?;
            let encoded_len = |saved: &SavedMemories| {
                serde_json::to_vec_pretty(&SavedMemories {
                    version: saved.version,
                    revision: next_revision,
                    records: saved.records.clone(),
                })
                .map(|bytes| bytes.len())
                .map_err(|error| format!("Cannot encode memories: {error}"))
            };
            if saved.records.len() > MAX_RECORDS || encoded_len(saved)? > MAX_STORE_BYTES {
                let mut candidates: Vec<_> = saved
                    .records
                    .iter()
                    .filter(|record| !protected_ids.contains(&record.id))
                    .cloned()
                    .collect();
                candidates.sort_by(|a, b| {
                    (
                        a.scope == MemoryScope::User,
                        a.updated_at,
                        a.created_at,
                        &a.id,
                    )
                        .cmp(&(
                            b.scope == MemoryScope::User,
                            b.updated_at,
                            b.created_at,
                            &b.id,
                        ))
                });
                for candidate in candidates {
                    saved.records.retain(|record| record.id != candidate.id);
                    if saved.records.len() <= MAX_RECORDS && encoded_len(saved)? <= MAX_STORE_BYTES
                    {
                        break;
                    }
                }
            }
            Ok(())
        })
    }

    pub fn recall(&self, target: &str, query: &str) -> Result<Vec<MemoryRecord>, String> {
        validate_scope(MemoryScope::Target, Some(target))?;
        let keywords = keywords(query);
        let mut ranked: Vec<_> = self
            .list()?
            .into_iter()
            .filter(|record| {
                record.scope == MemoryScope::User || record.target.as_deref() == Some(target)
            })
            .filter_map(|record| {
                let normalized = record.text.to_lowercase();
                let score = keywords
                    .iter()
                    .filter(|keyword| normalized.contains(keyword.as_str()))
                    .count();
                (score > 0 || record.scope == MemoryScope::User).then_some((score, record))
            })
            .collect();
        ranked.sort_by(|a, b| (b.0, b.1.updated_at, &b.1.id).cmp(&(a.0, a.1.updated_at, &a.1.id)));
        let mut bytes = 0;
        Ok(ranked
            .into_iter()
            .map(|(_, record)| record)
            .take(MAX_RECALL_RECORDS)
            .take_while(|record| {
                bytes += record.text.len();
                bytes <= MAX_RECALL_BYTES
            })
            .collect())
    }

    fn mutate<T>(
        &self,
        change: impl FnOnce(&mut SavedMemories) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Memory storage is unavailable")?;
        self.check_directory()?;
        fs::create_dir_all(&self.root).map_err(|e| format!("Cannot create memory storage: {e}"))?;
        self.check_directory()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("Cannot protect memory storage: {e}"))?;
        }
        let _file_lock = self.write_lock()?;
        let mut saved = self.load(true)?;
        let before = saved.records.clone();
        let result = change(&mut saved)?;
        if before == saved.records {
            return Ok(result);
        }
        saved.revision = saved
            .revision
            .checked_add(1)
            .ok_or("Memory revision limit reached")?;
        let bytes = serde_json::to_vec_pretty(&saved)
            .map_err(|e| format!("Cannot encode memories: {e}"))?;
        if bytes.len() > MAX_STORE_BYTES {
            return Err("Memory storage exceeds its size limit".into());
        }
        self.atomic_write("memories.json", &bytes)?;
        self.atomic_write("memories.backup.json", &bytes)
            .map_err(|e| format!("Memory update was saved, but its backup failed: {e}"))?;
        Ok(result)
    }

    fn load(&self, strict: bool) -> Result<SavedMemories, String> {
        let primary = self.read_saved("memories.json");
        let backup = self.read_saved("memories.backup.json");
        if strict {
            if let Err(error) = &primary {
                return Err(error.clone());
            }
            if let Err(error) = &backup {
                return Err(error.clone());
            }
        }
        match (primary, backup) {
            (Ok(Some(primary)), Ok(Some(backup))) => Ok(if backup.revision > primary.revision {
                backup
            } else {
                primary
            }),
            (Ok(Some(saved)), Ok(None)) | (Ok(None), Ok(Some(saved))) => Ok(saved),
            (Ok(Some(saved)), Err(error)) | (Err(error), Ok(Some(saved))) => {
                log::warn!("[MotherMemory] using intact memory copy: {error}");
                Ok(saved)
            }
            (Ok(None), Ok(None)) => Ok(SavedMemories::default()),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    fn read_saved(&self, name: &str) -> Result<Option<SavedMemories>, String> {
        let path = self.root.join(name);
        let Some(file) = open_regular_file(&path, false)? else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        file.take((MAX_STORE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Cannot read memories: {e}"))?;
        if bytes.len() > MAX_STORE_BYTES {
            return Err(
                "Saved memories exceed the size limit; existing files were preserved".into(),
            );
        }
        let saved: SavedMemories = serde_json::from_slice(&bytes).map_err(|e| {
            format!("Saved memories are damaged; existing files were preserved: {e}")
        })?;
        if saved.version != 1 || saved.records.len() > MAX_RECORDS {
            return Err(
                "Saved memories have an unsupported format; existing files were preserved".into(),
            );
        }
        let mut ids = HashSet::new();
        for record in &saved.records {
            validate_id(&record.id)?;
            validate_text(&record.text)?;
            validate_scope(record.scope, record.target.as_deref())?;
            validate_source(record.source_conversation_id.as_deref())?;
            if !ids.insert(&record.id) {
                return Err(
                    "Saved memories contain duplicate IDs; existing files were preserved".into(),
                );
            }
        }
        Ok(Some(saved))
    }

    fn atomic_write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let target = self.root.join(name);
        if let Some(file) = open_regular_file(&target, false)? {
            drop(file);
        }
        let temporary = self.root.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        set_private_options(&mut options);
        let mut file = options
            .open(&temporary)
            .map_err(|e| format!("Cannot save memories: {e}"))?;
        let write = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        let result = write.and_then(|_| fs::rename(&temporary, target));
        if let Err(error) = result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Cannot save memories: {error}"));
        }
        #[cfg(unix)]
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| format!("Cannot sync memory storage: {e}"))?;
        Ok(())
    }

    fn read_lock(&self) -> Result<Option<FileLock>, String> {
        let Some(file) = open_regular_file(&self.root.join("writer.lock"), false)? else {
            return Ok(None);
        };
        fs2::FileExt::try_lock_shared(&file)
            .map_err(|e| format!("Cannot read memory storage while it is being updated: {e}"))?;
        Ok(Some(FileLock(file)))
    }

    fn write_lock(&self) -> Result<FileLock, String> {
        let path = self.root.join("writer.lock");
        let file = match open_regular_file(&path, true)? {
            Some(file) => file,
            None => {
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true);
                set_private_options(&mut options);
                match options.open(&path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        open_regular_file(&path, true)?.ok_or("Memory lock is unavailable")?
                    }
                    Err(error) => return Err(format!("Cannot lock memory storage: {error}")),
                }
            }
        };
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|e| format!("Cannot lock memory storage: {e}"))?;
        Ok(FileLock(file))
    }

    fn check_directory(&self) -> Result<bool, String> {
        let mut exists = true;
        for path in self.root.ancestors() {
            if path.as_os_str().is_empty() {
                continue;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => {
                    return Err("Memory storage cannot use linked or non-directory paths".into());
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => exists = false,
                Err(error) => return Err(format!("Cannot inspect memory storage: {error}")),
            }
        }
        Ok(exists)
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) {
        Ok(())
    } else {
        Err("Invalid memory ID".into())
    }
}

fn validate_text(text: &str) -> Result<&str, String> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        Err("Memory text must contain 1 to 2000 UTF-8 bytes and no null characters".into())
    } else if contains_credentials(text) {
        Err("Memory text must not contain passwords, API tokens, or private keys".into())
    } else {
        Ok(text)
    }
}

pub(crate) fn contains_credentials(text: &str) -> bool {
    let normalized = text.to_lowercase();
    if normalized.contains("-----begin") && normalized.contains("private key-----") {
        return true;
    }
    for token in text.split(|character: char| {
        !character.is_ascii_alphanumeric() && character != '-' && character != '_'
    }) {
        if ["sk-", "ghp_", "github_pat_", "AIza"]
            .iter()
            .any(|prefix| token.starts_with(*prefix) && token.len() >= prefix.len() + 20)
        {
            return true;
        }
    }
    for marker in [
        "api_key",
        "api-key",
        "apikey",
        "api key",
        "access_token",
        "access token",
        "password",
        "passwd",
        "密码",
        "密钥",
        "令牌",
    ] {
        for (offset, _) in normalized.match_indices(marker) {
            let value =
                normalized[offset + marker.len()..].trim_start_matches(|character: char| {
                    character.is_whitespace() || character == '\'' || character == '"'
                });
            if ["=", ":", "：", "是", "为"].iter().any(|separator| {
                value.strip_prefix(*separator).is_some_and(|value| {
                    !value
                        .trim_matches(|character: char| {
                            character.is_whitespace() || character == '\'' || character == '"'
                        })
                        .is_empty()
                })
            }) {
                return true;
            }
        }
    }
    normalized.match_indices("bearer ").any(|(offset, _)| {
        normalized[offset + 7..]
            .split_whitespace()
            .next()
            .is_some_and(|value| value.len() >= 8)
    })
}

fn validate_scope(scope: MemoryScope, target: Option<&str>) -> Result<(), String> {
    match (scope, target) {
        (MemoryScope::User, None) => Ok(()),
        (MemoryScope::Target, Some(target))
            if !target.trim().is_empty()
                && target.len() <= 256
                && !target.chars().any(char::is_control) =>
        {
            Ok(())
        }
        _ => Err(
            "User memories have no target; target memories need a valid machine or server ID"
                .into(),
        ),
    }
}

fn validate_source(source: Option<&str>) -> Result<(), String> {
    if source.is_some_and(|source| {
        source.is_empty() || source.len() > 256 || source.chars().any(char::is_control)
    }) {
        Err("Invalid memory source conversation".into())
    } else {
        Ok(())
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn keywords(query: &str) -> HashSet<String> {
    let normalized: String = query.to_lowercase().chars().take(2_000).collect();
    let mut result = HashSet::new();
    for word in normalized.split(|character: char| !character.is_alphanumeric()) {
        if word.chars().count() >= 2 {
            result.insert(word.to_string());
        }
        let characters: Vec<_> = word.chars().collect();
        for pair in characters.windows(2) {
            if pair.iter().all(|character| !character.is_ascii()) {
                result.insert(pair.iter().collect());
            }
        }
    }
    result
}

fn is_link(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}

fn set_private_options(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
}

fn open_regular_file(path: &Path, writable: bool) -> Result<Option<File>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => {}
        Ok(_) => return Err("Memory storage contains a linked or non-regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Cannot inspect memory storage: {error}")),
    }
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    set_private_options(&mut options);
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot open memories: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Cannot inspect memories: {e}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("Memory storage contains a linked or non-regular file".into());
    }
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            let base = fs::canonicalize(&base).unwrap_or(base);
            Self(base.join(format!("echobird-memory-test-{}", uuid::Uuid::new_v4())))
        }

        fn store(&self) -> MemoryStore {
            MemoryStore::new(self.0.clone())
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if self.0.is_dir() {
                let _ = fs::remove_dir_all(&self.0);
            } else {
                let _ = fs::remove_file(&self.0);
            }
        }
    }

    fn applicable_records(store: &MemoryStore, target: &str) -> Vec<MemoryRecord> {
        store
            .list()
            .unwrap()
            .into_iter()
            .filter(|record| {
                record.scope == MemoryScope::User || record.target.as_deref() == Some(target)
            })
            .collect()
    }

    fn full_records(scope: MemoryScope, target: Option<&str>) -> Vec<MemoryRecord> {
        (0..MAX_RECORDS)
            .map(|index| MemoryRecord {
                id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
                text: format!("Old note {index}"),
                scope,
                target: target.map(str::to_string),
                source_conversation_id: None,
                created_at: index as u64,
                updated_at: index as u64,
            })
            .collect()
    }

    fn seed_store(dir: &TestDir, records: &[MemoryRecord]) -> MemoryStore {
        fs::create_dir(&dir.0).unwrap();
        let saved = SavedMemories {
            version: 1,
            revision: 7,
            records: records.to_vec(),
        };
        let bytes = serde_json::to_vec_pretty(&saved).unwrap();
        for name in ["memories.json", "memories.backup.json"] {
            fs::write(dir.0.join(name), &bytes).unwrap();
        }
        dir.store()
    }

    #[test]
    fn capture_applies_corrections_and_forgetting_in_one_revision() {
        let dir = TestDir::new();
        let store = dir.store();
        let preference = store
            .remember(
                MemoryScope::User,
                None,
                "Use Chinese",
                Some("original-chat"),
            )
            .unwrap();
        let local = store
            .remember(
                MemoryScope::Target,
                Some("local"),
                "Install under D:/Old",
                None,
            )
            .unwrap();
        let obsolete = store
            .remember(
                MemoryScope::Target,
                Some("local"),
                "Old executable location",
                None,
            )
            .unwrap();
        let other = store
            .remember(
                MemoryScope::Target,
                Some("other"),
                "Keep another server fact",
                None,
            )
            .unwrap();
        let previous: SavedMemories =
            serde_json::from_slice(&fs::read(dir.0.join("memories.json")).unwrap()).unwrap();
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                vec![
                    MemoryChange::Update {
                        id: preference.id.clone(),
                        text: "Use English".into(),
                    },
                    MemoryChange::Update {
                        id: local.id.clone(),
                        text: "Install under E:/New".into(),
                    },
                    MemoryChange::Forget {
                        id: obsolete.id.clone(),
                    },
                    MemoryChange::Add {
                        scope: MemoryScope::Target,
                        text: "  Verified installation  ".into(),
                    },
                    MemoryChange::Add {
                        scope: MemoryScope::User,
                        text: "Prefer concise replies".into(),
                    },
                ],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        for (original, text) in [(preference, "Use English"), (local, "Install under E:/New")] {
            let updated = records
                .iter()
                .find(|record| record.id == original.id)
                .unwrap();
            let mut expected = original;
            expected.text = text.into();
            expected.updated_at = updated.updated_at;
            assert_eq!(updated, &expected);
        }
        assert!(records.contains(&other));
        assert!(!records.iter().any(|record| record.id == obsolete.id));
        assert!(!records
            .iter()
            .any(|record| record.text == "Install under D:/Old"));
        let added = records
            .iter()
            .find(|record| record.text == "Verified installation")
            .unwrap();
        assert_eq!(added.scope, MemoryScope::Target);
        assert_eq!(added.target.as_deref(), Some("local"));
        assert_eq!(
            added.source_conversation_id.as_deref(),
            Some("capture-chat")
        );
        let added = records
            .iter()
            .find(|record| record.text == "Prefer concise replies")
            .unwrap();
        assert_eq!(added.scope, MemoryScope::User);
        assert!(added.target.is_none());
        let saved: SavedMemories =
            serde_json::from_slice(&fs::read(dir.0.join("memories.json")).unwrap()).unwrap();
        assert_eq!(saved.revision, previous.revision + 1);
        assert_eq!(
            fs::read(dir.0.join("memories.json")).unwrap(),
            fs::read(dir.0.join("memories.backup.json")).unwrap()
        );
    }

    #[test]
    fn capture_ignores_other_targets_but_compares_full_applicable_records() {
        let dir = TestDir::new();
        let store = dir.store();
        store
            .remember(MemoryScope::User, None, "Use Chinese", None)
            .unwrap();
        store
            .remember(MemoryScope::Target, Some("local"), "Local fact", None)
            .unwrap();
        let mut expected = applicable_records(&store, "local");
        expected.reverse();
        let other = store
            .remember(
                MemoryScope::Target,
                Some("other"),
                "New other server fact",
                None,
            )
            .unwrap();
        store
            .apply_capture(
                "local",
                &expected,
                vec![MemoryChange::Add {
                    scope: MemoryScope::Target,
                    text: "Another local fact".into(),
                }],
                "capture-chat",
            )
            .unwrap();
        assert!(store.list().unwrap().contains(&other));
        let mut expected = applicable_records(&store, "local");
        expected[0].source_conversation_id = Some("different-chat".into());
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        assert!(store
            .apply_capture(
                "local",
                &expected,
                vec![MemoryChange::Add {
                    scope: MemoryScope::User,
                    text: "A stale proposal".into(),
                }],
                "capture-chat"
            )
            .is_err());
        assert_eq!(fs::read(dir.0.join("memories.json")).unwrap(), bytes);
    }

    #[test]
    fn stale_captures_cannot_replace_updates_or_restore_forgotten_notes() {
        for intervening in ["update", "forget", "new-user", "new-target"] {
            let dir = TestDir::new();
            let store = dir.store();
            let original = store
                .remember(MemoryScope::Target, Some("local"), "Original fact", None)
                .unwrap();
            let expected = applicable_records(&store, "local");
            match intervening {
                "update" => {
                    store.update(&original.id, "Corrected fact").unwrap();
                }
                "forget" => store.forget(&original.id).unwrap(),
                "new-user" => {
                    store
                        .remember(MemoryScope::User, None, "New preference", None)
                        .unwrap();
                }
                _ => {
                    store
                        .remember(MemoryScope::Target, Some("local"), "New target fact", None)
                        .unwrap();
                }
            }
            let before = store.list().unwrap();
            let bytes = fs::read(dir.0.join("memories.json")).unwrap();
            assert!(
                store
                    .apply_capture(
                        "local",
                        &expected,
                        vec![
                            MemoryChange::Add {
                                scope: MemoryScope::Target,
                                text: "Original fact".into()
                            },
                            MemoryChange::Update {
                                id: original.id,
                                text: "Stale replacement".into()
                            },
                        ],
                        "capture-chat"
                    )
                    .is_err(),
                "{intervening}"
            );
            assert_eq!(store.list().unwrap(), before);
            for name in ["memories.json", "memories.backup.json"] {
                assert_eq!(fs::read(dir.0.join(name)).unwrap(), bytes);
            }
        }
    }

    #[test]
    fn invalid_capture_batches_leave_both_store_copies_unchanged() {
        let dir = TestDir::new();
        let store = dir.store();
        let local = store
            .remember(MemoryScope::Target, Some("local"), "Local fact", None)
            .unwrap();
        let other = store
            .remember(MemoryScope::Target, Some("other"), "Other fact", None)
            .unwrap();
        let expected = applicable_records(&store, "local");
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        let invalid = vec![
            vec![
                MemoryChange::Update {
                    id: local.id.clone(),
                    text: "Valid correction".into(),
                },
                MemoryChange::Add {
                    scope: MemoryScope::User,
                    text: "password=secret".into(),
                },
            ],
            vec![
                MemoryChange::Update {
                    id: local.id.clone(),
                    text: "Valid correction".into(),
                },
                MemoryChange::Forget { id: other.id },
            ],
            vec![
                MemoryChange::Update {
                    id: local.id.clone(),
                    text: "Valid correction".into(),
                },
                MemoryChange::Forget { id: local.id },
            ],
            vec![MemoryChange::Add {
                scope: MemoryScope::Target,
                text: "中".repeat(667),
            }],
            vec![MemoryChange::Forget {
                id: "../invalid".into(),
            }],
            (0..9)
                .map(|index| MemoryChange::Add {
                    scope: MemoryScope::User,
                    text: format!("Preference {index}"),
                })
                .collect(),
        ];
        for changes in invalid {
            assert!(store
                .apply_capture("local", &expected, changes, "capture-chat")
                .is_err());
            for name in ["memories.json", "memories.backup.json"] {
                assert_eq!(fs::read(dir.0.join(name)).unwrap(), bytes);
            }
        }
        assert!(store
            .apply_capture("local", &store.list().unwrap(), Vec::new(), "capture-chat")
            .is_err());
        assert!(store
            .apply_capture("", &expected, Vec::new(), "capture-chat")
            .is_err());
        assert!(store
            .apply_capture("local", &expected, Vec::new(), "")
            .is_err());
    }

    #[test]
    fn capture_rolls_over_the_oldest_user_note_when_all_notes_are_preferences() {
        let dir = TestDir::new();
        let original = full_records(MemoryScope::User, None);
        let store = seed_store(&dir, &original);
        let expected = store.list().unwrap();
        store
            .apply_capture(
                "local",
                &expected,
                vec![MemoryChange::Add {
                    scope: MemoryScope::User,
                    text: "New preference".into(),
                }],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.iter().any(|record| record.text == "New preference"));
        assert!(!records.iter().any(|record| record.id == original[0].id));
        assert!(original[1..].iter().all(|record| records.contains(record)));
        assert_eq!(
            fs::read(dir.0.join("memories.json")).unwrap(),
            fs::read(dir.0.join("memories.backup.json")).unwrap()
        );
    }

    #[test]
    fn capture_add_and_forget_at_capacity_does_not_evict_another_note() {
        let dir = TestDir::new();
        let original = full_records(MemoryScope::User, None);
        let store = seed_store(&dir, &original);
        let expected = store.list().unwrap();
        store
            .apply_capture(
                "local",
                &expected,
                vec![
                    MemoryChange::Add {
                        scope: MemoryScope::User,
                        text: "New preference".into(),
                    },
                    MemoryChange::Forget {
                        id: expected[0].id.clone(),
                    },
                ],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.iter().any(|record| record.text == "New preference"));
        assert!(!records.iter().any(|record| record.id == expected[0].id));
        assert!(original
            .iter()
            .filter(|record| record.id != expected[0].id)
            .all(|record| records.contains(record)));
    }

    #[test]
    fn capture_keeps_all_additions_and_updates_when_pruning_multiple_notes() {
        let dir = TestDir::new();
        let mut original = full_records(MemoryScope::Target, Some("local"));
        for record in &mut original {
            record.updated_at = u64::MAX;
        }
        let store = seed_store(&dir, &original);
        let mut changes = vec![MemoryChange::Update {
            id: original[0].id.clone(),
            text: "Corrected current fact".into(),
        }];
        changes.extend((0..7).map(|index| MemoryChange::Add {
            scope: MemoryScope::Target,
            text: format!("New fact {index}"),
        }));
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                changes,
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records
            .iter()
            .any(|record| record.id == original[0].id && record.text == "Corrected current fact"));
        for index in 0..7 {
            assert!(records
                .iter()
                .any(|record| record.text == format!("New fact {index}")));
        }
        assert!(original[1..8]
            .iter()
            .all(|old| !records.iter().any(|record| record.id == old.id)));
        assert!(original[8..].iter().all(|record| records.contains(record)));
    }

    #[test]
    fn capture_prefers_pruning_target_facts_before_older_user_preferences() {
        let dir = TestDir::new();
        let mut original = full_records(MemoryScope::User, None);
        original[MAX_RECORDS - 1].scope = MemoryScope::Target;
        original[MAX_RECORDS - 1].target = Some("local".into());
        original[MAX_RECORDS - 1].updated_at = u64::MAX;
        let store = seed_store(&dir, &original);
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                vec![MemoryChange::Add {
                    scope: MemoryScope::Target,
                    text: "New target fact".into(),
                }],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(!records
            .iter()
            .any(|record| record.id == original[MAX_RECORDS - 1].id));
        assert!(original[..MAX_RECORDS - 1]
            .iter()
            .all(|record| records.contains(record)));
    }

    #[test]
    fn capture_global_rollover_prunes_the_oldest_foreign_target_fact() {
        let dir = TestDir::new();
        let mut original = full_records(MemoryScope::Target, Some("other"));
        original[1].target = Some("local".into());
        original[0].updated_at = original[1].updated_at;
        original[0].created_at = original[1].created_at;
        let store = seed_store(&dir, &original);
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                vec![MemoryChange::Add {
                    scope: MemoryScope::Target,
                    text: "New local fact".into(),
                }],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(!records.iter().any(|record| record.id == original[0].id));
        assert!(original[1..].iter().all(|record| records.contains(record)));
    }

    #[test]
    fn duplicate_or_empty_capture_at_capacity_does_not_prune_or_write() {
        let dir = TestDir::new();
        let original = full_records(MemoryScope::User, None);
        let store = seed_store(&dir, &original);
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        for changes in [
            Vec::new(),
            vec![MemoryChange::Add {
                scope: MemoryScope::User,
                text: original[0].text.clone(),
            }],
        ] {
            store
                .apply_capture(
                    "local",
                    &applicable_records(&store, "local"),
                    changes,
                    "capture-chat",
                )
                .unwrap();
            for name in ["memories.json", "memories.backup.json"] {
                assert_eq!(fs::read(dir.0.join(name)).unwrap(), bytes);
            }
        }
        assert_eq!(store.list().unwrap().len(), MAX_RECORDS);
    }

    #[test]
    fn capture_keeps_a_reaffirmed_oldest_note_when_another_addition_rolls_over() {
        let dir = TestDir::new();
        let original = full_records(MemoryScope::Target, Some("local"));
        let store = seed_store(&dir, &original);
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                vec![
                    MemoryChange::Add {
                        scope: MemoryScope::Target,
                        text: original[0].text.clone(),
                    },
                    MemoryChange::Add {
                        scope: MemoryScope::Target,
                        text: "New current fact".into(),
                    },
                ],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.contains(&original[0]));
        assert!(!records.iter().any(|record| record.id == original[1].id));
        assert!(original[2..].iter().all(|record| records.contains(record)));
        assert!(records
            .iter()
            .any(|record| record.text == "New current fact"));
    }

    #[test]
    fn capture_rolls_over_at_the_encoded_byte_limit_below_record_capacity() {
        let dir = TestDir::new();
        let mut original = Vec::new();
        for mut record in full_records(MemoryScope::Target, Some("local")) {
            record
                .text
                .push_str(&"\u{0001}".repeat(MAX_TEXT_BYTES - record.text.len()));
            original.push(record);
            let preview = SavedMemories {
                version: 1,
                revision: 7,
                records: original.clone(),
            };
            if serde_json::to_vec_pretty(&preview).unwrap().len() > MAX_STORE_BYTES {
                original.pop();
                break;
            }
        }
        assert!(original.len() < MAX_RECORDS);
        let store = seed_store(&dir, &original);
        let new_text = "\u{0001}".repeat(MAX_TEXT_BYTES);
        let now = unix_seconds();
        let mut preview = SavedMemories {
            version: 1,
            revision: 8,
            records: original.clone(),
        };
        preview.records.push(MemoryRecord {
            id: uuid::Uuid::new_v4().to_string(),
            text: new_text.clone(),
            scope: MemoryScope::Target,
            target: Some("local".into()),
            source_conversation_id: Some("capture-chat".into()),
            created_at: now,
            updated_at: now,
        });
        assert!(preview.records.len() <= MAX_RECORDS);
        assert!(serde_json::to_vec_pretty(&preview).unwrap().len() > MAX_STORE_BYTES);
        let mut removed = 0;
        while serde_json::to_vec_pretty(&preview).unwrap().len() > MAX_STORE_BYTES {
            preview.records.remove(0);
            removed += 1;
        }
        store
            .apply_capture(
                "local",
                &applicable_records(&store, "local"),
                vec![MemoryChange::Add {
                    scope: MemoryScope::Target,
                    text: new_text.clone(),
                }],
                "capture-chat",
            )
            .unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), original.len() + 1 - removed);
        assert!(records.iter().any(|record| record.text == new_text));
        assert!(original[..removed]
            .iter()
            .all(|old| !records.iter().any(|record| record.id == old.id)));
        assert!(original[removed..]
            .iter()
            .all(|record| records.contains(record)));
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        assert!(bytes.len() <= MAX_STORE_BYTES);
        assert_eq!(bytes, fs::read(dir.0.join("memories.backup.json")).unwrap());
    }

    #[test]
    fn invalid_or_stale_capture_at_capacity_does_not_prune() {
        let dir = TestDir::new();
        let original = full_records(MemoryScope::Target, Some("local"));
        let store = seed_store(&dir, &original);
        let stale = applicable_records(&store, "local");
        store.update(&original[0].id, "A newer correction").unwrap();
        let current = applicable_records(&store, "local");
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        assert!(store
            .apply_capture(
                "local",
                &stale,
                vec![MemoryChange::Add {
                    scope: MemoryScope::Target,
                    text: "Stale new note".into(),
                }],
                "capture-chat"
            )
            .is_err());
        assert!(store
            .apply_capture(
                "local",
                &current,
                vec![
                    MemoryChange::Add {
                        scope: MemoryScope::Target,
                        text: "Valid new note".into()
                    },
                    MemoryChange::Add {
                        scope: MemoryScope::User,
                        text: "password=secret".into()
                    },
                ],
                "capture-chat"
            )
            .is_err());
        for name in ["memories.json", "memories.backup.json"] {
            assert_eq!(fs::read(dir.0.join(name)).unwrap(), bytes);
        }
        assert_eq!(store.list().unwrap(), current);
    }

    #[test]
    fn capture_change_format_cannot_supply_backend_scope_fields() {
        for value in [
            serde_json::json!({"action":"add", "scope":"target", "text":"Fact", "target":"other"}),
            serde_json::json!({"action":"update", "id":uuid::Uuid::new_v4().to_string(), "text":"Fact", "source":"other-chat"}),
            serde_json::json!({"action":"forget", "id":uuid::Uuid::new_v4().to_string(), "text":"Fact"}),
        ] {
            assert!(serde_json::from_value::<MemoryChange>(value).is_err());
        }
    }

    #[test]
    fn missing_reads_and_invalid_values_create_nothing() {
        let dir = TestDir::new();
        let store = dir.store();
        assert!(store.list().unwrap().is_empty());
        assert!(store.recall("local", "安装软件").unwrap().is_empty());
        assert!(store.update("../outside", "text").is_err());
        assert!(store.forget("../outside").is_err());
        assert!(store
            .remember(MemoryScope::User, Some("local"), "text", None)
            .is_err());
        assert!(store
            .remember(MemoryScope::Target, None, "text", None)
            .is_err());
        assert!(store.remember(MemoryScope::User, None, " ", None).is_err());
        assert!(store
            .remember(MemoryScope::User, None, &"中".repeat(667), None)
            .is_err());
        assert!(!dir.0.exists());
    }

    #[test]
    fn obvious_credentials_are_rejected_without_echoing_or_changing_them() {
        let dir = TestDir::new();
        let store = dir.store();
        for text in [
            "API_KEY = very-secret",
            "密码是secret",
            "{\"api_key\": \"secret\"}",
            "Bearer very-secret",
            "sk-123456789012345678901234",
            "-----BEGIN RSA PRIVATE KEY-----\nprivate\n-----END RSA PRIVATE KEY-----",
        ] {
            let error = store
                .remember(MemoryScope::User, None, text, None)
                .unwrap_err();
            assert!(!error.contains(text));
            assert!(!dir.0.exists());
        }
        let saved = store
            .remember(MemoryScope::User, None, "不要保存密码或API key", None)
            .unwrap();
        assert!(store.update(&saved.id, "password=secret").is_err());
        assert_eq!(store.list().unwrap(), vec![saved]);
    }

    #[test]
    fn records_survive_restart_and_edit_without_changing_identity_or_scope() {
        let dir = TestDir::new();
        let initial = dir
            .store()
            .remember(
                MemoryScope::Target,
                Some("server-a"),
                "  使用容器部署  ",
                Some("conversation-a"),
            )
            .unwrap();
        assert_eq!(initial.text, "使用容器部署");
        let updated = dir.store().update(&initial.id, "改为本机容器部署").unwrap();
        assert_eq!(updated.id, initial.id);
        assert_eq!(updated.scope, initial.scope);
        assert_eq!(updated.target, initial.target);
        assert_eq!(
            updated.source_conversation_id,
            initial.source_conversation_id
        );
        assert_eq!(dir.store().list().unwrap(), vec![updated]);
    }

    #[test]
    fn recall_is_relevant_and_keeps_servers_separate() {
        let dir = TestDir::new();
        let store = dir.store();
        let global = store
            .remember(MemoryScope::User, None, "请使用中文回复", None)
            .unwrap();
        let local = store
            .remember(
                MemoryScope::Target,
                Some("local"),
                "容器部署路径在D盘",
                None,
            )
            .unwrap();
        let server = store
            .remember(
                MemoryScope::Target,
                Some("server-a"),
                "容器部署路径在/opt",
                None,
            )
            .unwrap();
        let unrelated = store
            .remember(MemoryScope::Target, Some("local"), "Git使用SSH登录", None)
            .unwrap();
        let recalled = store.recall("local", "帮我更新容器部署").unwrap();
        assert!(recalled.contains(&global));
        assert!(recalled.contains(&local));
        assert!(!recalled.contains(&server));
        assert!(!recalled.contains(&unrelated));
        assert_eq!(store.recall("server-b", "容器部署").unwrap(), vec![global]);
    }

    #[test]
    fn forget_is_retained_by_both_copies_and_not_recalled_after_recovery() {
        let dir = TestDir::new();
        let store = dir.store();
        let forgotten = store
            .remember(MemoryScope::User, None, "删除这条偏好", None)
            .unwrap();
        let kept = store
            .remember(MemoryScope::User, None, "保留这条偏好", None)
            .unwrap();
        store.forget(&forgotten.id).unwrap();
        fs::write(dir.0.join("memories.json"), "damaged").unwrap();
        assert_eq!(dir.store().list().unwrap(), vec![kept.clone()]);
        assert_eq!(dir.store().recall("local", "偏好").unwrap(), vec![kept]);
        assert!(store.forget(&forgotten.id).is_err());
        assert_eq!(
            fs::read_to_string(dir.0.join("memories.json")).unwrap(),
            "damaged"
        );
    }

    #[test]
    fn damaged_copies_never_become_an_empty_store_or_get_overwritten() {
        let dir = TestDir::new();
        let store = dir.store();
        store
            .remember(MemoryScope::User, None, "保留我", None)
            .unwrap();
        for name in ["memories.json", "memories.backup.json"] {
            fs::write(dir.0.join(name), "damaged").unwrap();
        }
        assert!(store.list().is_err());
        assert!(store
            .remember(MemoryScope::User, None, "新偏好", None)
            .is_err());
        for name in ["memories.json", "memories.backup.json"] {
            assert_eq!(fs::read_to_string(dir.0.join(name)).unwrap(), "damaged");
        }
    }

    #[test]
    fn duplicate_and_full_stores_do_not_discard_records() {
        let dir = TestDir::new();
        let store = dir.store();
        let first = store
            .remember(MemoryScope::User, None, "偏好0", None)
            .unwrap();
        assert_eq!(
            store
                .remember(MemoryScope::User, None, "偏好0", None)
                .unwrap(),
            first
        );
        for index in 1..MAX_RECORDS {
            store
                .remember(MemoryScope::User, None, &format!("偏好{index}"), None)
                .unwrap();
        }
        assert!(store
            .remember(MemoryScope::User, None, "溢出记录", None)
            .is_err());
        let records = store.list().unwrap();
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(records.contains(&first));
        assert!(store.recall("local", "偏好").unwrap().len() <= MAX_RECALL_RECORDS);
    }

    #[test]
    fn recall_has_a_byte_budget_with_unicode_memories() {
        let dir = TestDir::new();
        let store = dir.store();
        for index in 0..10 {
            store
                .remember(
                    MemoryScope::User,
                    None,
                    &format!("{index}{}", "界".repeat(666)),
                    None,
                )
                .unwrap();
        }
        let recalled = store.recall("local", "界").unwrap();
        assert!(!recalled.is_empty());
        assert!(
            recalled
                .iter()
                .map(|record| record.text.len())
                .sum::<usize>()
                <= MAX_RECALL_BYTES
        );
    }

    #[test]
    fn newer_valid_copy_survives_an_interrupted_backup_update() {
        let dir = TestDir::new();
        let store = dir.store();
        let record = store
            .remember(MemoryScope::User, None, "original", None)
            .unwrap();
        let previous = fs::read(dir.0.join("memories.json")).unwrap();
        let updated = store.update(&record.id, "updated").unwrap();
        fs::write(dir.0.join("memories.backup.json"), previous).unwrap();
        assert_eq!(store.list().unwrap(), vec![updated]);
    }

    #[test]
    fn concurrent_writers_do_not_lose_memories() {
        let dir = TestDir::new();
        std::thread::scope(|threads| {
            for index in 0..8 {
                let path = dir.0.clone();
                threads.spawn(move || {
                    MemoryStore::new(path)
                        .remember(
                            MemoryScope::User,
                            None,
                            &format!("preference-{index}"),
                            None,
                        )
                        .unwrap();
                });
            }
        });
        assert_eq!(dir.store().list().unwrap().len(), 8);
    }

    #[test]
    fn busy_store_returns_an_error_without_waiting_or_changing_files() {
        let dir = TestDir::new();
        let store = dir.store();
        store
            .remember(MemoryScope::User, None, "original", None)
            .unwrap();
        let bytes = fs::read(dir.0.join("memories.json")).unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.0.join("writer.lock"))
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&file).unwrap();
        assert!(store.list().is_err());
        assert!(store
            .remember(MemoryScope::User, None, "new", None)
            .is_err());
        assert_eq!(fs::read(dir.0.join("memories.json")).unwrap(), bytes);
        fs2::FileExt::unlock(&file).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn non_directory_storage_errors_without_overwriting_it() {
        let dir = TestDir::new();
        fs::write(&dir.0, "keep").unwrap();
        assert!(dir.store().list().is_err());
        assert!(dir
            .store()
            .remember(MemoryScope::User, None, "preference", None)
            .is_err());
        assert_eq!(fs::read_to_string(&dir.0).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn linked_roots_and_records_are_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        fs::create_dir(&dir.0).unwrap();
        let outside = dir.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let linked = dir.0.join("linked");
        symlink(&outside, &linked).unwrap();
        assert!(MemoryStore::new(linked).list().is_err());
        let store = MemoryStore::new(dir.0.join("real"));
        store
            .remember(MemoryScope::User, None, "original", None)
            .unwrap();
        let path = store.root.join("memories.json");
        let outside_file = outside.join("private");
        fs::write(&outside_file, "private").unwrap();
        fs::remove_file(&path).unwrap();
        symlink(&outside_file, &path).unwrap();
        assert!(store
            .remember(MemoryScope::User, None, "new", None)
            .is_err());
        assert_eq!(fs::read_to_string(outside_file).unwrap(), "private");
    }

    #[cfg(unix)]
    #[test]
    fn new_storage_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        dir.store()
            .remember(MemoryScope::User, None, "private", None)
            .unwrap();
        assert_eq!(
            fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for name in ["memories.json", "memories.backup.json", "writer.lock"] {
            assert_eq!(
                fs::metadata(dir.0.join(name)).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
