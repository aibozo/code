use std::fs::File;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Result, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::memory::summarizer::Summary;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Filename used for the JSONL memory store within `~/.codex`.
const MEMORY_FILENAME: &str = "memory.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSummary {
    pub repo_key: String,
    pub session_id: String,
    pub ts: u64, // unix ms
    pub kind: String, // "summary"
    pub title: String,
    pub text: String,
    #[serde(default)]
    pub msg_ids: Vec<String>,
}

/// Append-only JSONL store for conversation summaries.
pub struct JsonlMemoryStore {
    path: PathBuf,
}

impl JsonlMemoryStore {
    /// Create a new store under the given `home` directory (e.g., `~/.codex`).
    pub fn new(home: &Path) -> Self {
        let mut p = home.to_path_buf();
        p.push(MEMORY_FILENAME);
        Self { path: p }
    }

    /// Append a summary for the given repo and session.
    pub fn append(
        &self,
        repo_key: &str,
        session_id: &Uuid,
        summary: &Summary,
        msg_ids: &[String],
    ) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Build record
        let ts_ms: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| std::io::Error::other(format!("system clock before Unix epoch: {e}")))?
            .as_millis() as u64;

        let record = StoredSummary {
            repo_key: repo_key.to_string(),
            session_id: session_id.to_string(),
            ts: ts_ms,
            kind: "summary".to_string(),
            title: summary.title.clone(),
            text: summary.text.clone(),
            msg_ids: msg_ids.to_vec(),
        };

        let mut line = serde_json::to_string(&record)
            .map_err(|e| std::io::Error::other(format!("failed to serialize memory record: {e}")))?;
        line.push('\n');

        // Open file in append-only mode
        let mut options = OpenOptions::new();
        options.append(true).read(true).create(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }

        let mut file = options.open(&self.path)?;
        ensure_owner_only_permissions(&file)?;
        acquire_exclusive_lock_with_retry(&file)?;

        // Write in a single syscall where possible
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    /// Return up to `limit` most recent summaries for `repo_key` (newest first).
    pub fn recent(&self, repo_key: &str, limit: usize) -> Result<Vec<StoredSummary>> {
        // Open for reading; return empty on not found
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };

        acquire_shared_lock_with_retry(&file)?;
        let reader = BufReader::new(&file);
        let mut entries: Vec<StoredSummary> = Vec::new();
        for line in reader.lines() {
            let line = match line {
                Ok(s) => s,
                Err(_) => continue,
            };
            let Ok(rec) = serde_json::from_str::<StoredSummary>(&line) else { continue };
            if rec.repo_key == repo_key {
                entries.push(rec);
            }
        }

        // Sort by ts desc and take limit
        entries.sort_by(|a, b| b.ts.cmp(&a.ts));
        if entries.len() > limit {
            entries.truncate(limit);
        }
        Ok(entries)
    }
}

const MAX_RETRIES: usize = 10;
const RETRY_SLEEP_MS: u64 = 100;

#[cfg(unix)]
fn acquire_exclusive_lock_with_retry(file: &File) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MS));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "could not acquire exclusive lock on memory file",
    ))
}

#[cfg(not(unix))]
fn acquire_exclusive_lock_with_retry(_file: &File) -> Result<()> { Ok(()) }

#[cfg(unix)]
fn acquire_shared_lock_with_retry(file: &File) -> Result<()> {
    for _ in 0..MAX_RETRIES {
        match fs2::FileExt::try_lock_shared(file) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_SLEEP_MS));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "could not acquire shared lock on memory file",
    ))
}

#[cfg(not(unix))]
fn acquire_shared_lock_with_retry(_file: &File) -> Result<()> { Ok(()) }

#[cfg(unix)]
fn ensure_owner_only_permissions(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    let current_mode = metadata.permissions().mode() & 0o777;
    if current_mode != 0o600 {
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        file.set_permissions(perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_permissions(_file: &File) -> Result<()> { Ok(()) }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn recent_empty_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let rows = store.recent("rk", 5).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn append_and_recent_filters_and_limits() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let sid = Uuid::new_v4();

        let s1 = Summary { title: "A".into(), text: "alpha".into() };
        let s2 = Summary { title: "B".into(), text: "bravo".into() };
        let s3 = Summary { title: "C".into(), text: "charlie".into() };

        store.append("rk1", &sid, &s1, &[]).unwrap();
        store.append("rk2", &sid, &s2, &[]).unwrap();
        store.append("rk1", &sid, &s3, &[]).unwrap();

        let rows_all = store.recent("rk1", 10).unwrap();
        assert_eq!(rows_all.len(), 2);
        let titles: Vec<_> = rows_all.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.contains(&"A") && titles.contains(&"C"));

        let rows_limit = store.recent("rk1", 1).unwrap();
        assert_eq!(rows_limit.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_are_0600() {
        use std::os::unix::fs::MetadataExt;
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let sid = Uuid::new_v4();
        let s = Summary { title: "T".into(), text: "t".into() };
        store.append("rk", &sid, &s, &[]).unwrap();

        let path = tmp.path().join(super::MEMORY_FILENAME);
        let meta = std::fs::metadata(path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
