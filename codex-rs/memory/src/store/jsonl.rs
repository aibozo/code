use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::knn::top_k_cosine;
use super::{EmbeddedRecord, SearchHit};

const FILENAME: &str = "memory_embeddings.jsonl";
const MAX_RETRIES: usize = 10;
const RETRY_MS: u64 = 100;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, Clone)]
pub struct JsonlVectorStore {
    path: PathBuf,
}

impl JsonlVectorStore {
    pub fn new(home: &Path) -> Self {
        let mut p = home.to_path_buf();
        p.push(FILENAME);
        Self { path: p }
    }

    pub fn add(&self, rec: &EmbeddedRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() { std::fs::create_dir_all(parent)?; }

        let mut opts = OpenOptions::new();
        opts.create(true).append(true).read(true);
        #[cfg(unix)]
        { opts.mode(0o600); }
        let mut file = opts.open(&self.path)?;
        ensure_owner_only_permissions(&file)?;
        lock_exclusive_with_retry(&file)?;

        let mut line = serde_json::to_string(rec)
            .map_err(|e| std::io::Error::other(format!("serialize embedding record failed: {e}")))?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    pub fn query(&self, repo_key: &str, query_vec: &[f32], top_k: usize) -> std::io::Result<Vec<SearchHit>> {
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        lock_shared_with_retry(&file)?;
        let reader = BufReader::new(&file);
        let mut recs: Vec<EmbeddedRecord> = Vec::new();
        for line in reader.lines() {
            let Ok(s) = line else { continue };
            let Ok(rec) = serde_json::from_str::<EmbeddedRecord>(&s) else { continue };
            if rec.repo_key == repo_key && rec.dim == query_vec.len() {
                recs.push(rec);
            }
        }
        if recs.is_empty() { return Ok(Vec::new()); }
        let hay: Vec<Vec<f32>> = recs.iter().map(|r| r.vec.clone()).collect();
        let top = top_k_cosine(&hay, query_vec, top_k);
        let out = top.into_iter().map(|s| {
            let r = &recs[s.idx];
            SearchHit { id: r.id.clone(), score: s.score, title: r.title.clone(), text: r.text.clone(), ts: r.ts }
        }).collect();
        Ok(out)
    }

    /// Return up to `top_k` most similar vectors of a specific `kind` for `repo_key`.
    pub fn query_kind(&self, repo_key: &str, kind: &str, query_vec: &[f32], top_k: usize) -> std::io::Result<Vec<SearchHit>> {
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        lock_shared_with_retry(&file)?;
        let reader = BufReader::new(&file);
        let mut recs: Vec<EmbeddedRecord> = Vec::new();
        for line in reader.lines() {
            let Ok(s) = line else { continue };
            let Ok(rec) = serde_json::from_str::<EmbeddedRecord>(&s) else { continue };
            if rec.repo_key == repo_key && rec.kind == kind && rec.dim == query_vec.len() {
                recs.push(rec);
            }
        }
        if recs.is_empty() { return Ok(Vec::new()); }
        let hay: Vec<Vec<f32>> = recs.iter().map(|r| r.vec.clone()).collect();
        let top = top_k_cosine(&hay, query_vec, top_k);
        let out = top.into_iter().map(|s| {
            let r = &recs[s.idx];
            SearchHit { id: r.id.clone(), score: s.score, title: r.title.clone(), text: r.text.clone(), ts: r.ts }
        }).collect();
        Ok(out)
    }

    /// Return true if there exists at least one record for the given `repo_key` and `kind`.
    pub fn any_kind(&self, repo_key: &str, kind: &str) -> std::io::Result<bool> {
        let file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        };
        lock_shared_with_retry(&file)?;
        let reader = BufReader::new(&file);
        for line in reader.lines() {
            let Ok(s) = line else { continue };
            if let Ok(rec) = serde_json::from_str::<EmbeddedRecord>(&s) {
                if rec.repo_key == repo_key && rec.kind == kind {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Atomically replace all records of `kind` for `repo_key` with the provided `records`.
    /// This rewrites the underlying JSONL file by filtering out matching entries and then
    /// appending the new ones. File permissions and locks are preserved.
    pub fn replace_kind(&self, repo_key: &str, kind: &str, mut records: Vec<EmbeddedRecord>) -> std::io::Result<()> {
        // Open source for read (if missing, we'll just write new records)
        let existing_opt = OpenOptions::new().read(true).open(&self.path);

        // Prepare temp output file in the same directory
        if let Some(parent) = self.path.parent() { std::fs::create_dir_all(parent)?; }
        let mut out_opts = OpenOptions::new();
        out_opts.create(true).write(true).truncate(true).read(true);
        #[cfg(unix)]
        { out_opts.mode(0o600); }
        let tmp_path = self.path.with_extension("jsonl.tmp");
        let mut out = out_opts.open(&tmp_path)?;
        ensure_owner_only_permissions(&out)?;
        lock_exclusive_with_retry(&out)?;

        // If an existing file is present, copy over non-matching entries
        if let Ok(file) = existing_opt {
            lock_shared_with_retry(&file)?;
            let reader = BufReader::new(&file);
            for line in reader.lines() {
                let Ok(s) = line else { continue };
                match serde_json::from_str::<EmbeddedRecord>(&s) {
                    Ok(rec) => {
                        if rec.repo_key == repo_key && rec.kind == kind {
                            // skip old kind entries for this repo
                            continue;
                        }
                        let mut line = serde_json::to_string(&rec)
                            .map_err(|e| std::io::Error::other(format!("serialize record failed: {e}")))?;
                        line.push('\n');
                        out.write_all(line.as_bytes())?;
                    }
                    Err(_) => {
                        // Preserve unparsable lines untouched
                        out.write_all(s.as_bytes())?;
                        out.write_all(b"\n")?;
                    }
                }
            }
        }

        // Append new records
        for rec in records.drain(..) {
            let mut line = serde_json::to_string(&rec)
                .map_err(|e| std::io::Error::other(format!("serialize record failed: {e}")))?;
            line.push('\n');
            out.write_all(line.as_bytes())?;
        }
        out.flush()?;
        drop(out);

        // Atomic swap
        std::fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_owner_only_permissions(file: &std::fs::File) -> std::io::Result<()> {
    let meta = file.metadata()?;
    let mode = meta.permissions().mode() & 0o777;
    if mode != 0o600 {
        let mut p = meta.permissions();
        p.set_mode(0o600);
        file.set_permissions(p)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_permissions(_file: &std::fs::File) -> std::io::Result<()> { Ok(()) }

fn lock_exclusive_with_retry(file: &std::fs::File) -> std::io::Result<()> {
    for _ in 0..MAX_RETRIES {
        match fs2::FileExt::try_lock_exclusive(file) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "embed store: lock timeout"))
}

fn lock_shared_with_retry(file: &std::fs::File) -> std::io::Result<()> {
    for _ in 0..MAX_RETRIES {
        match fs2::FileExt::try_lock_shared(file) {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::WouldBlock, "embed store: lock timeout"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::cosine_similarity;
    use tempfile::TempDir;

    #[test]
    fn add_and_query_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlVectorStore::new(tmp.path());
        let now = 1_700_000_000_000u64;

        let rec1 = EmbeddedRecord {
            repo_key: "/r".into(), id: "a".into(), ts: now, kind: "summary".into(), title: "A".into(), text: "alpha".into(), dim: 3, vec: vec![1.0, 0.0, 0.0]
        };
        let rec2 = EmbeddedRecord {
            repo_key: "/r".into(), id: "b".into(), ts: now+1, kind: "summary".into(), title: "B".into(), text: "bravo".into(), dim: 3, vec: vec![0.7, 0.3, 0.0]
        };
        let rec3 = EmbeddedRecord {
            repo_key: "/other".into(), id: "c".into(), ts: now+2, kind: "summary".into(), title: "C".into(), text: "charlie".into(), dim: 3, vec: vec![0.0, 1.0, 0.0]
        };
        store.add(&rec1).unwrap();
        store.add(&rec2).unwrap();
        store.add(&rec3).unwrap();

        let hits = store.query("/r", &[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id, "a");
        assert!(hits[0].score >= cosine_similarity(&[1.0,0.0,0.0], &[0.7,0.3,0.0]));
    }

    #[test]
    fn query_kind_and_any_kind_filter_by_kind_and_dim() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlVectorStore::new(tmp.path());
        let now = 1_700_000_000_100u64;

        // Two entries for same repo with different kinds and dims
        let rec_code = EmbeddedRecord {
            repo_key: "/r".into(), id: "code1".into(), ts: now, kind: "code".into(), title: "file.rs:1".into(), text: "fn main(){}".into(), dim: 4, vec: vec![1.0, 0.0, 0.0, 0.0]
        };
        let rec_sum = EmbeddedRecord {
            repo_key: "/r".into(), id: "sum1".into(), ts: now+1, kind: "summary".into(), title: "Sum".into(), text: "alpha".into(), dim: 4, vec: vec![0.0, 1.0, 0.0, 0.0]
        };
        let rec_wrong_dim = EmbeddedRecord {
            repo_key: "/r".into(), id: "code2".into(), ts: now+2, kind: "code".into(), title: "file.rs:2".into(), text: "fn f(){}".into(), dim: 3, vec: vec![0.0, 1.0, 0.0]
        };
        store.add(&rec_code).unwrap();
        store.add(&rec_sum).unwrap();
        store.add(&rec_wrong_dim).unwrap();

        // Query for code-kind vectors with a 4-dim query vector should only return the 4-dim code rec
        let hits = store.query_kind("/r", "code", &[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "code1");

        // any_kind matches only when at least one record of that kind exists
        assert!(store.any_kind("/r", "code").unwrap());
        assert!(store.any_kind("/r", "summary").unwrap());
        assert!(!store.any_kind("/r", "notes").unwrap());

        // Query with wrong dim (5 instead of stored 4) should produce no results
        let miss = store.query_kind("/r", "code", &[1.0, 0.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn replace_kind_rewrites_only_target_kind_for_repo() {
        let tmp = TempDir::new().unwrap();
        let store = JsonlVectorStore::new(tmp.path());
        let now = 1_700_000_100_000u64;

        // Seed store with mixed kinds and repos
        let base = vec![
            EmbeddedRecord { repo_key: "/r".into(), id: "keep1".into(), ts: now, kind: "summary".into(), title: "S1".into(), text: "sum".into(), dim: 2, vec: vec![0.1, 0.2] },
            EmbeddedRecord { repo_key: "/r".into(), id: "oldcode".into(), ts: now, kind: "code".into(), title: "code.old".into(), text: "old".into(), dim: 2, vec: vec![0.2, 0.1] },
            EmbeddedRecord { repo_key: "/other".into(), id: "othercode".into(), ts: now, kind: "code".into(), title: "other".into(), text: "x".into(), dim: 2, vec: vec![0.0, 1.0] },
        ];
        for r in &base { store.add(r).unwrap(); }

        // New code records for /r
        let new_records = vec![
            EmbeddedRecord { repo_key: "/r".into(), id: "new1".into(), ts: now+1, kind: "code".into(), title: "file.rs:#1".into(), text: "fn a(){}".into(), dim: 2, vec: vec![1.0, 0.0] },
            EmbeddedRecord { repo_key: "/r".into(), id: "new2".into(), ts: now+2, kind: "code".into(), title: "file.rs:#2".into(), text: "fn b(){}".into(), dim: 2, vec: vec![0.9, 0.1] },
        ];
        store.replace_kind("/r", "code", new_records).unwrap();

        // Query code for /r returns only new ids
        let hits = store.query_kind("/r", "code", &[1.0, 0.0], 10).unwrap();
        let ids: Vec<_> = hits.into_iter().map(|h| h.id).collect();
        assert!(ids.contains(&"new1".to_string()) && ids.contains(&"new2".to_string()));
        assert!(!ids.contains(&"oldcode".to_string()));

        // Other kinds and repos are preserved
        assert!(store.any_kind("/r", "summary").unwrap());
        assert!(store.any_kind("/other", "code").unwrap());
    }

    #[test]
    fn replace_kind_preserves_malformed_lines() {
        use std::io::Write;
        let tmp = TempDir::new().unwrap();
        let store = JsonlVectorStore::new(tmp.path());
        let now = 1_700_100_000_000u64;

        // Write a valid record and a malformed line directly
        let rec = EmbeddedRecord { repo_key: "/r".into(), id: "id1".into(), ts: now, kind: "summary".into(), title: "T".into(), text: "t".into(), dim: 2, vec: vec![0.1, 0.2] };
        store.add(&rec).unwrap();

        // Append malformed line
        let path = tmp.path().join(super::FILENAME);
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"THIS IS NOT JSON\n").unwrap();
            f.flush().unwrap();
        }

        // Replace code kind (none exist yet) with new records; malformed must remain
        store.replace_kind("/r", "code", vec![EmbeddedRecord {
            repo_key: "/r".into(), id: "code1".into(), ts: now+1, kind: "code".into(), title: "f.rs:#1".into(), text: "fn x(){}".into(), dim: 2, vec: vec![1.0, 0.0]
        }]).unwrap();

        // Read back file and ensure malformed line still exists
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.lines().any(|l| l == "THIS IS NOT JSON"));
    }
}
