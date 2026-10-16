use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STORE_RECORDS: usize = 128;
const MAX_PAGE_BYTES: usize = 8_000;
const HEADER_RESERVE: usize = 768;
static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputRecord {
    #[serde(skip)]
    pub command: String,
    pub target: String,
    pub exit_code: Option<i64>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl OutputRecord {
    pub fn combined(&self) -> String {
        let mut output = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !output.is_empty() {
                output.push_str("\n--- stderr ---\n");
            }
            output.push_str(&self.stderr);
        }
        output
    }
}

pub struct OutputStore {
    root: PathBuf,
}

impl OutputStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn standard() -> Self {
        Self::new(crate::utils::platform::echobird_dir().join("agent-output"))
    }

    pub fn store(&self, scope: &str, record: &OutputRecord) -> Result<String, String> {
        if record
            .stdout
            .len()
            .saturating_add(record.stderr.len())
            .saturating_add(record.target.len())
            > MAX_RECORD_BYTES
        {
            return Err("Command output exceeds the 8 MiB retention limit".into());
        }
        let bytes =
            serde_json::to_vec(record).map_err(|e| format!("Cannot encode command output: {e}"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err("Command output exceeds the 8 MiB retention limit".into());
        }
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Output store is unavailable")?;
        self.check_directory()?;
        fs::create_dir_all(&self.root).map_err(|e| format!("Cannot create output storage: {e}"))?;
        self.check_directory()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("Cannot protect output storage: {e}"))?;
        }

        let id = uuid::Uuid::new_v4().to_string();
        let path = self.record_path(scope, &id);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&path)
            .map_err(|e| format!("Cannot save command output: {e}"))?;
        let result = file.write_all(&bytes).and_then(|_| file.sync_all());
        drop(file);
        if let Err(e) = result {
            let _ = fs::remove_file(&path);
            return Err(format!("Cannot save complete command output: {e}"));
        }
        if let Err(e) = self.prune(&path) {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        Ok(id)
    }

    pub fn read(
        &self,
        scope: &str,
        output_id: &str,
        offset: usize,
        max_bytes: usize,
    ) -> Result<String, String> {
        validate_id(output_id)?;
        if max_bytes < 4 {
            return Err("max_bytes must be at least 4 to fit any UTF-8 character".into());
        }
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Output store is unavailable")?;
        if !self.check_directory()? {
            return Err("Saved output is unavailable (missing or expired)".into());
        }
        let file = open_regular_file(&self.record_path(scope, output_id))?;
        let mut bytes = Vec::new();
        file.take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Cannot read saved output: {e}"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err("Saved output exceeds the retention limit".into());
        }
        let record: OutputRecord = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Saved output cannot be decoded: {e}"))?;
        let output = record.combined();
        if offset > output.len() {
            return Err(format!(
                "Offset {offset} exceeds the saved output length {}",
                output.len()
            ));
        }
        let mut start = offset;
        while !output.is_char_boundary(start) {
            start += 1;
        }
        let limit = max_bytes.min(MAX_PAGE_BYTES - HEADER_RESERVE);
        let mut end = start.saturating_add(limit).min(output.len());
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        let next = if end < output.len() {
            end.to_string()
        } else {
            "none".into()
        };
        let status = if record.success { "success" } else { "failure" };
        let exit_code = record
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".into());
        let target: String = record
            .target
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect();
        Ok(format!(
            "Output: {output_id}\nTarget: {target}\nStatus: {status}; exit code: {exit_code}\nRequested offset: {offset}; effective offset: {start}\nRequested max bytes: {max_bytes}; effective max bytes: {limit}\nBytes: {start}..{end} of {}\nNext offset: {next}\nUse tool_output_read with the next offset to continue; do not rerun the command.\n--- content ---\n{}",
            output.len(),
            &output[start..end]
        ))
    }

    pub fn clear_scope(&self, scope: &str) -> Result<(), String> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Output store is unavailable")?;
        if !self.check_directory()? {
            return Ok(());
        }
        let prefix = scope_prefix(scope);
        let entries =
            fs::read_dir(&self.root).map_err(|e| format!("Cannot list saved outputs: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("Cannot list saved outputs: {e}"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(&prefix) || !is_record_name(name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("Cannot inspect saved output: {e}"))?;
            if metadata.is_file() && !is_link(&metadata) {
                fs::remove_file(entry.path())
                    .map_err(|e| format!("Cannot remove saved output: {e}"))?;
            }
        }
        Ok(())
    }

    fn record_path(&self, scope: &str, id: &str) -> PathBuf {
        self.root.join(format!("{}{id}.json", scope_prefix(scope)))
    }

    fn check_directory(&self) -> Result<bool, String> {
        let mut exists = true;
        for path in self.root.ancestors() {
            if path.as_os_str().is_empty() {
                continue;
            }
            match fs::symlink_metadata(path) {
                Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => {
                    return Err("Output storage cannot use linked or non-directory paths".into());
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => exists = false,
                Err(e) => return Err(format!("Cannot inspect output storage: {e}")),
            }
        }
        Ok(exists)
    }

    fn prune(&self, keep: &Path) -> Result<(), String> {
        let mut records = Vec::new();
        let mut total = 0u64;
        for entry in fs::read_dir(&self.root)
            .map_err(|e| format!("Cannot check output storage limits: {e}"))?
        {
            let entry = entry.map_err(|e| format!("Cannot check output storage limits: {e}"))?;
            if !entry.file_name().to_str().is_some_and(is_record_name) {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|e| format!("Cannot inspect saved output: {e}"))?;
            if metadata.is_file() && !is_link(&metadata) {
                total = total.saturating_add(metadata.len());
                records.push((
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    entry.path(),
                    metadata.len(),
                ));
            }
        }
        records.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
        let mut count = records.len();
        for (_, path, size) in records {
            if count <= MAX_STORE_RECORDS && total <= MAX_STORE_BYTES {
                break;
            }
            if path == keep {
                continue;
            }
            fs::remove_file(path)
                .map_err(|e| format!("Cannot enforce output storage limits: {e}"))?;
            count -= 1;
            total = total.saturating_sub(size);
        }
        Ok(())
    }
}

fn scope_prefix(scope: &str) -> String {
    format!("{:x}-", Sha256::digest(scope.as_bytes()))
}

fn validate_id(id: &str) -> Result<(), String> {
    if uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == id) {
        Ok(())
    } else {
        Err("Invalid saved output ID".into())
    }
}

fn is_record_name(name: &str) -> bool {
    name.len() == 106
        && name.as_bytes()[..64]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        && name.as_bytes()[64] == b'-'
        && name.ends_with(".json")
        && validate_id(&name[65..101]).is_ok()
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

fn open_regular_file(path: &Path) -> Result<File, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "Saved output is unavailable (missing or expired)".to_string())?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("Saved output is not a regular file".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // Open the reparse point itself.
    }
    let file = options
        .open(path)
        .map_err(|e| format!("Cannot open saved output: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("Cannot inspect saved output: {e}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err("Saved output is not a regular file".into());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let base = std::env::temp_dir();
            let base = fs::canonicalize(&base).unwrap_or(base);
            Self(base.join(format!("echobird-output-test-{}", uuid::Uuid::new_v4())))
        }

        fn store(&self) -> OutputStore {
            OutputStore::new(self.0.clone())
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(stdout: &str) -> OutputRecord {
        OutputRecord {
            command: "tool --token secret".into(),
            target: "local".into(),
            exit_code: Some(12),
            success: false,
            stdout: stdout.into(),
            stderr: "error details".into(),
        }
    }

    fn content(page: &str) -> &str {
        page.split_once("\n--- content ---\n").unwrap().1
    }

    fn next_offset(page: &str) -> Option<usize> {
        page.lines()
            .find_map(|line| line.strip_prefix("Next offset: "))
            .unwrap()
            .parse()
            .ok()
    }

    #[test]
    fn complete_record_retains_metadata_and_both_streams() {
        let dir = TestDir::new();
        let store = dir.store();
        let original = record("start\nfailed\nend");
        let id = store.store("session", &original).unwrap();
        let saved: OutputRecord =
            serde_json::from_slice(&fs::read(store.record_path("session", &id)).unwrap()).unwrap();
        assert!(saved.command.is_empty());
        assert!(!fs::read_to_string(store.record_path("session", &id))
            .unwrap()
            .contains("secret"));
        assert_eq!(saved.stdout, original.stdout);
        assert_eq!(saved.stderr, original.stderr);
        assert_eq!(saved.exit_code, Some(12));
        assert!(!saved.success);
        let page = store.read("session", &id, 0, 6_000).unwrap();
        assert_eq!(content(&page), original.combined());
        assert!(page.contains("Status: failure; exit code: 12"));
        assert!(!page.contains("secret"));
        assert_eq!(next_offset(&page), None);
    }

    #[test]
    fn unicode_and_long_lines_can_be_recovered_exactly() {
        let dir = TestDir::new();
        let store = dir.store();
        let original = record(&"中文🙂".repeat(4_000));
        let id = store.store("session", &original).unwrap();
        let mut recovered = String::new();
        let mut offset = 0;
        loop {
            let page = store.read("session", &id, offset, 8_000).unwrap();
            assert!(page.len() <= MAX_PAGE_BYTES);
            recovered.push_str(content(&page));
            match next_offset(&page) {
                Some(next) => {
                    assert!(next > offset);
                    offset = next;
                }
                None => break,
            }
        }
        assert_eq!(recovered, original.combined());
        assert!(store.read("session", &id, 1, 1).is_err());
        let shifted = store.read("session", &id, 1, 4).unwrap();
        assert!(shifted.contains("Requested offset: 1; effective offset: 3"));
        assert_eq!(content(&shifted), "文");
    }

    #[test]
    fn missing_invalid_and_cross_scope_reads_create_nothing() {
        let dir = TestDir::new();
        let store = dir.store();
        let id = uuid::Uuid::new_v4().to_string();
        assert!(store.read("session", &id, 0, 80).is_err());
        store.clear_scope("session").unwrap();
        assert!(!dir.0.exists());
        assert!(store.read("session", "../outside", 0, 80).is_err());
        let saved = store.store("scope-a", &record("output")).unwrap();
        assert!(store.read("scope-b", &saved, 0, 80).is_err());
        assert!(store.read("scope-a", &saved, usize::MAX, 80).is_err());
    }

    #[test]
    fn clear_scope_preserves_other_scopes_and_unowned_files() {
        let dir = TestDir::new();
        let store = dir.store();
        let a = store.store("a", &record("a")).unwrap();
        let b = store.store("b", &record("b")).unwrap();
        fs::write(dir.0.join("notes.json"), "keep").unwrap();
        store.clear_scope("a").unwrap();
        assert!(!store.record_path("a", &a).exists());
        assert!(store.read("b", &b, 0, 6_000).is_ok());
        assert_eq!(
            fs::read_to_string(dir.0.join("notes.json")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn oversized_record_does_not_create_storage_and_retry_recovers() {
        let dir = TestDir::new();
        let store = dir.store();
        for stdout in [
            "x".repeat(MAX_RECORD_BYTES),
            "\0".repeat(MAX_RECORD_BYTES / 6 + 1),
        ] {
            assert!(store.store("a", &record(&stdout)).is_err());
            assert!(!dir.0.exists());
        }
        let id = store.store("a", &record("small")).unwrap();
        assert!(store.read("a", &id, 0, 6_000).is_ok());
    }

    #[test]
    fn unavailable_directory_recovers_without_leaving_partial_records() {
        let dir = TestDir::new();
        fs::write(&dir.0, "not a directory").unwrap();
        let store = dir.store();
        assert!(store.store("a", &record("output")).is_err());
        fs::remove_file(&dir.0).unwrap();
        let id = store.store("a", &record("output")).unwrap();
        assert!(store.read("a", &id, 0, 6_000).is_ok());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn retention_enforces_count_and_keeps_newest_record() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut last_id = String::new();
        for index in 0..=MAX_STORE_RECORDS {
            last_id = store.store("a", &record(&index.to_string())).unwrap();
        }
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), MAX_STORE_RECORDS);
        assert!(store.read("a", &last_id, 0, 6_000).is_ok());
    }

    #[test]
    fn retention_enforces_total_bytes_without_removing_unowned_files() {
        let dir = TestDir::new();
        fs::create_dir_all(&dir.0).unwrap();
        let store = dir.store();
        for _ in 0..9 {
            let id = uuid::Uuid::new_v4().to_string();
            File::create(store.record_path("old", &id))
                .unwrap()
                .set_len(MAX_RECORD_BYTES as u64)
                .unwrap();
        }
        fs::write(dir.0.join("outside.json"), "keep").unwrap();
        let id = store.store("a", &record("newest")).unwrap();
        let total: u64 = fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_name().to_str().is_some_and(is_record_name))
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(total <= MAX_STORE_BYTES);
        assert!(store.read("a", &id, 0, 6_000).is_ok());
        assert!(dir.0.join("outside.json").exists());
    }

    #[test]
    fn malformed_saved_record_fails_without_changes() {
        let dir = TestDir::new();
        let store = dir.store();
        let id = store.store("a", &record("original")).unwrap();
        let path = store.record_path("a", &id);
        fs::write(&path, "incomplete json").unwrap();
        assert!(store.read("a", &id, 0, 6_000).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "incomplete json");
    }

    #[test]
    fn headers_remain_bounded_without_echoing_command_arguments() {
        let dir = TestDir::new();
        let store = dir.store();
        let mut original = record(&"output".repeat(5_000));
        original.target = "🙂\n".repeat(1_000);
        for exit_code in [i64::from(i32::MIN), i64::from(u32::MAX)] {
            original.exit_code = Some(exit_code);
            let id = store.store("a", &original).unwrap();
            let page = store.read("a", &id, 0, usize::MAX).unwrap();
            assert!(page.len() <= MAX_PAGE_BYTES);
            assert!(!page.contains("secret"));
            assert!(page.contains(&format!("exit code: {exit_code}")));
            assert!(next_offset(&page).is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_roots_and_records_are_not_followed() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        fs::create_dir_all(&dir.0).unwrap();
        let outside = dir.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let linked_root = dir.0.join("linked");
        symlink(&outside, &linked_root).unwrap();
        let linked_store = OutputStore::new(linked_root);
        assert!(linked_store.store("a", &record("output")).is_err());
        assert!(linked_store.clear_scope("a").is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

        let store = OutputStore::new(dir.0.join("real"));
        let id = store.store("a", &record("original")).unwrap();
        let path = store.record_path("a", &id);
        fs::remove_file(&path).unwrap();
        let outside_file = outside.join("private");
        fs::write(&outside_file, "private").unwrap();
        symlink(&outside_file, &path).unwrap();
        assert!(store.read("a", &id, 0, 6_000).is_err());
        store.clear_scope("a").unwrap();
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(outside_file).unwrap(), "private");
    }

    #[cfg(unix)]
    #[test]
    fn new_storage_and_records_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TestDir::new();
        let store = dir.store();
        let id = store.store("a", &record("output")).unwrap();
        assert_eq!(
            fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.record_path("a", &id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
