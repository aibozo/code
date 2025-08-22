use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use once_cell::sync::Lazy;
use std::sync::Mutex;

use codex_memory::embedding::EmbeddingProvider;
use codex_memory::store::jsonl::JsonlVectorStore;
use codex_memory::store::EmbeddedRecord;

use crate::memory::openai_embeddings::OpenAiEmbeddingClient;
use crate::model_provider_info::built_in_model_providers;
use sha1::Digest;

/// Avoid re-indexing the same repo key multiple times per process lifetime.
static INDEXED_REPOS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct RepoIndexState {
    /// relative path -> (mtime_ms, size_bytes, sha1_hex)
    files: HashMap<String, (u64, u64, String)>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct CodeIndexState {
    repos: HashMap<String, RepoIndexState>,
}

fn state_path(home: &Path) -> PathBuf {
    home.join("code_index_state.json")
}

fn load_state(home: &Path) -> CodeIndexState {
    let p = state_path(home);
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => CodeIndexState::default(),
    }
}

fn save_state(home: &Path, state: &CodeIndexState) {
    if let Some(parent) = state_path(home).parent() { let _ = std::fs::create_dir_all(parent); }
    if let Ok(data) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(state_path(home), data);
    }
}

fn file_fingerprint(meta: &std::fs::Metadata, bytes: &[u8]) -> (u64, u64, String) {
    let mtime = meta.modified().ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let size = meta.len();
    let mut hasher = sha1::Sha1::new();
    hasher.update(bytes);
    let hex = format!("{:x}", hasher.finalize());
    (mtime, size, hex)
}

// Hard caps
const MAX_FILE_BYTES: u64 = 512 * 1024; // 512 KiB per file
const MAX_REPO_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB total indexed content per run

fn is_probably_binary(bytes: &[u8]) -> bool { bytes.iter().any(|&b| b == 0) }

fn is_skippable_path(path: &Path) -> bool {
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        let lower = name.to_ascii_lowercase();
        // Common generated or lock artifacts
        if lower.ends_with(".lock") || lower.ends_with(".min.js") { return true; }
    }
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        match ext.to_ascii_lowercase().as_str() {
            // Binary or large media formats
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "pdf" | "zip" | "gz" | "xz" | "bz2" | "7z" | "mp3" | "mp4" | "mov" | "avi" | "wasm" => return true,
            _ => {}
        }
    }
    false
}

fn language_aware_chunk_bytes(path: &Path, default_chunk_bytes: usize) -> usize {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        // Allow slightly larger chunks for markup/docs to preserve sections
        "md" | "rst" | "adoc" => (default_chunk_bytes as f32 * 1.5) as usize,
        // Use slightly smaller chunks for dense code to improve recall
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "kt" | "c" | "h" | "cpp" | "hpp" | "cs" =>
            (default_chunk_bytes as f32 * 0.85) as usize,
        _ => default_chunk_bytes,
    }.max(512)
}

/// Ensure a code index exists for the current repo when code_index is enabled and an API key is present.
/// Performs a best-effort scan + embed + store; errors are ignored.
pub fn ensure_code_index(
    repo_key: &str,
    home: &Path,
    cwd: &Path,
    dim: usize,
    chunk_bytes: usize,
) {
    // Only index once per process per repo
    {
        let mut seen = INDEXED_REPOS.lock().unwrap();
        if seen.contains(repo_key) { return; }
        seen.insert(repo_key.to_string());
    }

    let provider = match built_in_model_providers().get("openai").cloned() { Some(p) => p, None => return };
    let client = match OpenAiEmbeddingClient::from_provider(&provider, home) { Ok(c) => c, Err(_) => return };
    let vstore = JsonlVectorStore::new(home);

    // Load previous state and walk repo
    let mut state = load_state(home);
    let repo_state = state.repos.entry(repo_key.to_string()).or_default();
    let files = collect_code_files(cwd);
    let mut batch_texts: Vec<String> = Vec::new();
    let mut batch_meta: Vec<(String, String)> = Vec::new(); // (title, text)
    let mut indexed_bytes_total: u64 = 0;

    fn flush_batch(
        client: &OpenAiEmbeddingClient,
        vstore: &JsonlVectorStore,
        repo_key: &str,
        dim: usize,
        texts: &mut Vec<String>,
        meta: &mut Vec<(String, String)>,
    ) {
        if texts.is_empty() { return; }
        if let Ok(vecs) = client.embed(texts, dim) {
            let now_ms: u64 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            for (i, vec) in vecs.into_iter().enumerate() {
                if let Some((title, text)) = meta.get(i) {
                    let rec = EmbeddedRecord {
                        repo_key: repo_key.to_string(),
                        id: uuid::Uuid::new_v4().to_string(),
                        ts: now_ms,
                        kind: "code".to_string(),
                        title: title.clone(),
                        text: text.clone(),
                        dim,
                        vec,
                    };
                    let _ = vstore.add(&rec);
                }
            }
        }
        texts.clear();
        meta.clear();
    }

    for path in files {
        // Skip obviously non-code and large files quickly
        if is_skippable_path(&path) { continue; }
        let Ok(meta) = fs::metadata(&path) else { continue };
        if meta.len() > MAX_FILE_BYTES { continue; }
        if indexed_bytes_total >= MAX_REPO_BYTES { break; }

        if let Ok(mut f) = fs::File::open(&path) {
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_ok() {
                if is_probably_binary(&buf) { continue; }

                // Compute fingerprint and decide whether to index
                let (mtime_ms, size, sha1_hex) = file_fingerprint(&meta, &buf);
                let rel = path.strip_prefix(cwd).unwrap_or(&path).to_string_lossy().to_string();
                let prev = repo_state.files.get(&rel).cloned();
                let changed = match prev { Some((pm, ps, ph)) => pm != mtime_ms || ps != size || ph != sha1_hex, None => true };
                if !changed { continue; }

                let text = String::from_utf8_lossy(&buf);
                let lang_chunk = language_aware_chunk_bytes(&path, chunk_bytes);
                for (idx, chunk) in chunk_text(&text, lang_chunk).into_iter().enumerate() {
                    let title = format!("{}:#{}", rel, idx + 1);
                    indexed_bytes_total = indexed_bytes_total.saturating_add(chunk.len() as u64);
                    if indexed_bytes_total > MAX_REPO_BYTES { break; }
                    batch_texts.push(chunk.clone());
                    batch_meta.push((title, chunk));
                    if batch_texts.len() >= 64 {
                        flush_batch(&client, &vstore, repo_key, dim, &mut batch_texts, &mut batch_meta);
                    }
                }

                // Update per-file state after processing
                repo_state.files.insert(rel, (mtime_ms, size, sha1_hex));
            }
        }
        if indexed_bytes_total > MAX_REPO_BYTES { break; }
    }

    // flush tail
    flush_batch(&client, &vstore, repo_key, dim, &mut batch_texts, &mut batch_meta);

    // Persist updated state
    save_state(home, &state);
}

/// Rebuild only the `code` entries for a repo by re-indexing the working tree and
/// replacing records of kind "code". Used by the CLI `code memory reindex`.
pub fn rebuild_code_index(
    repo_key: &str,
    home: &Path,
    cwd: &Path,
    dim: usize,
    chunk_bytes: usize,
) -> std::io::Result<()> {
    let provider = match built_in_model_providers().get("openai").cloned() { Some(p) => p, None => return Ok(()) };
    let client = match OpenAiEmbeddingClient::from_provider(&provider, home) { Ok(c) => c, Err(_) => return Ok(()) };
    let vstore = JsonlVectorStore::new(home);

    // Walk all files with filters and produce new EmbeddedRecords
    let mut new_records: Vec<EmbeddedRecord> = Vec::new();
    let files = collect_code_files(cwd);
    let mut texts: Vec<String> = Vec::new();
    let mut meta: Vec<(String, String)> = Vec::new();
    let now_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let mut indexed_bytes_total: u64 = 0;
    for path in files {
        if is_skippable_path(&path) { continue; }
        let Ok(m) = fs::metadata(&path) else { continue };
        if m.len() > MAX_FILE_BYTES { continue; }
        if let Ok(mut f) = fs::File::open(&path) {
            let mut buf = Vec::new(); if f.read_to_end(&mut buf).is_err() { continue; }
            if is_probably_binary(&buf) { continue; }
            let rel = path.strip_prefix(cwd).unwrap_or(&path).to_string_lossy().to_string();
            let lang_chunk = language_aware_chunk_bytes(&path, chunk_bytes);
            let text = String::from_utf8_lossy(&buf);
            for (idx, chunk) in chunk_text(&text, lang_chunk).into_iter().enumerate() {
                indexed_bytes_total = indexed_bytes_total.saturating_add(chunk.len() as u64);
                if indexed_bytes_total > MAX_REPO_BYTES { break; }
                let title = format!("{}:#{}", rel, idx + 1);
                texts.push(chunk.clone());
                meta.push((title, chunk));
                if texts.len() >= 64 { // embed in batches
                    if let Ok(vecs) = client.embed(&texts, dim) {
                        for (i, vec) in vecs.into_iter().enumerate() {
                            let (title, text) = &meta[i];
                            new_records.push(EmbeddedRecord {
                                repo_key: repo_key.to_string(),
                                id: uuid::Uuid::new_v4().to_string(),
                                ts: now_ms,
                                kind: "code".to_string(),
                                title: title.clone(),
                                text: text.clone(),
                                dim,
                                vec,
                            });
                        }
                    }
                    texts.clear(); meta.clear();
                }
            }
        }
    }
    if !texts.is_empty() {
        if let Ok(vecs) = client.embed(&texts, dim) {
            for (i, vec) in vecs.into_iter().enumerate() {
                let (title, text) = &meta[i];
                new_records.push(EmbeddedRecord {
                    repo_key: repo_key.to_string(), id: uuid::Uuid::new_v4().to_string(), ts: now_ms,
                    kind: "code".to_string(), title: title.clone(), text: text.clone(), dim, vec,
                });
            }
        }
    }

    // Replace
    vstore.replace_kind(repo_key, "code", new_records)
}

fn collect_code_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    let skip_dirs = [
        ".git", "target", "node_modules", "dist", "build", ".idea", ".vscode", "__pycache__",
    ];
    while let Some(dir) = stack.pop() {
        if let Ok(read) = fs::read_dir(&dir) {
            for entry in read.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                        if skip_dirs.iter().any(|d| d.eq_ignore_ascii_case(name)) { continue; }
                    }
                    stack.push(p);
                } else if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn chunk_text(s: &str, chunk_bytes: usize) -> Vec<String> {
    if chunk_bytes == 0 { return Vec::new(); }
    let bytes = s.as_bytes();
    if bytes.is_empty() { return Vec::new(); }
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        let end = (start + chunk_bytes).min(bytes.len());
        // try to end on a newline boundary for nicer chunks
        let slice = &bytes[start..end];
        let mut cut = slice.len();
        if let Some(pos) = slice.iter().rposition(|&b| b == b'\n') {
            // Prefer ending on a newline when present anywhere beyond the first byte
            if pos > 0 {
                cut = pos + 1;
            }
        }
        let chunk = &slice[..cut];
        out.push(String::from_utf8_lossy(chunk).to_string());
        start += cut;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_aware_chunk_sizes_adjust() {
        let base = 1000usize;
        let rs = language_aware_chunk_bytes(Path::new("src/main.rs"), base);
        let md = language_aware_chunk_bytes(Path::new("README.md"), base);
        let txt = language_aware_chunk_bytes(Path::new("notes.txt"), base);
        assert!(rs < base, "code should slightly reduce chunk size");
        assert!(md > base, "docs should increase chunk size");
        assert_eq!(txt, base.max(512));
    }

    #[test]
    fn skippable_path_filters_common_binaries_and_minified() {
        assert!(is_skippable_path(&PathBuf::from("logo.png")));
        assert!(is_skippable_path(&PathBuf::from("bundle.min.js")));
        assert!(is_skippable_path(&PathBuf::from("Cargo.lock")));
        assert!(!is_skippable_path(&PathBuf::from("lib.rs")));
    }

    #[test]
    fn chunk_text_prefers_newline_boundaries() {
        let s = "line1\nline2-xxxx\nline3";
        // Force small chunk size so we see newline split behavior
        let chunks = chunk_text(s, 8);
        // Expect first chunk to end with a full line including newline
        assert!(chunks[0].ends_with('\n'));
        assert!(chunks.len() >= 2);
    }

    #[test]
    fn file_fingerprint_reports_size_and_sha1() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.txt");
        let data = b"hello world";
        {
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(data).unwrap();
            f.flush().unwrap();
        }
        let meta = fs::metadata(&path).unwrap();
        let (mtime, size, sha1_hex) = file_fingerprint(&meta, data);
        assert!(mtime > 0);
        assert_eq!(size, data.len() as u64);
        assert_eq!(sha1_hex.len(), 40);
    }
}
