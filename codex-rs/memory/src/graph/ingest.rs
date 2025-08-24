use super::{Edge, GraphStore, Node};
use sha1::Digest;
use serde_json::Value as Json;
use std::fs;
use std::io;
use std::path::Path;

/// Ingest a harness run JSON file into the graph as a Run node and Stage edges.
pub fn ingest_run_file<G: GraphStore>(g: &G, run_path: &Path) -> io::Result<()> {
    let s = fs::read_to_string(run_path)?;
    let v: Json = serde_json::from_str(&s).map_err(|e| io::Error::other(format!("parse run json: {e}")))?;
    let ts = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("unknown");
    let context = v.get("context").and_then(|x| x.as_str()).unwrap_or("");
    let mut ok = 0i64;
    let mut err = 0i64;
    if let Some(stages) = v.get("stages").and_then(|x| x.as_array()) {
        for st in stages {
            match st.get("status").and_then(|x| x.as_str()).unwrap_or("") {
                "ok" => ok += 1,
                "error" => err += 1,
                _ => {}
            }
        }
    }
    let id = format!("run:{}", ts);
    let props = serde_json::json!({ "context": context, "ok": ok, "error": err });
    g.put_node(&Node { kind: "Run".into(), id: id.clone(), props })?;
    if let Some(stages) = v.get("stages").and_then(|x| x.as_array()) {
        for st in stages {
            let name = st.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let cached = st.get("cached").and_then(|x| x.as_bool()).unwrap_or(false);
            let stage_id = format!("stage:{}@{}", name, ts);
            let _ = g.put_node(&Node { kind: "Stage".into(), id: stage_id.clone(), props: serde_json::json!({"name": name, "cached": cached}) });
            g.put_edge(&Edge { src: id.clone(), rel: "includes".into(), dst: stage_id, props: Json::Null })?;
        }
    }
    Ok(())
}

/// Ingest an episode directory (research/plan/summary markdown files) into the graph.
pub fn ingest_episode_dir<G: GraphStore>(g: &G, episode_dir: &Path) -> io::Result<()> {
    let ts = episode_dir.file_name().and_then(|s| s.to_str()).unwrap_or("episode");
    let id = format!("ep:{}", ts);
    let research = fs::read_to_string(episode_dir.join("research.md")).unwrap_or_default();
    let plan = fs::read_to_string(episode_dir.join("plan.md")).unwrap_or_default();
    let summary = fs::read_to_string(episode_dir.join("summary.md")).unwrap_or_default();
    // Try to infer context from summary.md (line starting with "Context:")
    let context = detect_context_from_summary(&summary);
    let props = serde_json::json!({
        "research_len": research.len(),
        "plan_len": plan.len(),
        "summary_len": summary.len(),
        "context": context,
    });
    g.put_node(&Node { kind: "Episode".into(), id: id.clone(), props })?;

    // Ingest citations from sources.json when present
    let sources_path = episode_dir.join("sources.json");
    if let Ok(s) = fs::read_to_string(&sources_path) {
        if let Ok(val) = serde_json::from_str::<Json>(&s) {
            if let Some(arr) = val.as_array() {
                for src in arr {
                    let url = src.get("url").and_then(|x| x.as_str()).unwrap_or("");
                    if url.is_empty() { continue; }
                    let mut hasher = sha1::Sha1::new();
                    hasher.update(url.as_bytes());
                    let out = hasher.finalize();
                    let url_hash = format!("{:x}", out);
                    let rs_id = format!("rs:{}", url_hash);
                    let title = src.get("title").and_then(|x| x.as_str()).unwrap_or("");
                    let year = src.get("year").and_then(|x| x.as_i64()).unwrap_or(0);
                    let authors = src.get("authors").cloned().unwrap_or(Json::Null);
                    let summary = src.get("summary").and_then(|x| x.as_str()).unwrap_or("");
                    let props = serde_json::json!({
                        "url": url,
                        "title": title,
                        "year": year,
                        "authors": authors,
                        "summary": summary,
                    });
                    let _ = g.put_node(&Node { kind: "ResearchSource".into(), id: rs_id.clone(), props });
                    let _ = g.put_edge(&Edge { src: id.clone(), rel: "cites".into(), dst: rs_id, props: Json::Null });
                }
            }
        }
    }
    Ok(())
}

fn detect_context_from_summary(summary: &str) -> String {
    for line in summary.lines() {
        let trimmed = line.trim();
        // Case-insensitive check for "Context: <label>"
        if let Some(rest) = trimmed.strip_prefix("Context:") {
            let lbl = rest.trim();
            if !lbl.is_empty() { return lbl.to_string(); }
        }
        if trimmed.to_ascii_lowercase().starts_with("context ") {
            // e.g., "context label: foo" — best-effort fallback
            if let Some((_, after)) = trimmed.split_once(':') {
                let lbl = after.trim();
                if !lbl.is_empty() { return lbl.to_string(); }
            }
        }
    }
    String::new()
}

/// Reingest all known artifacts in the repository into the graph store under `home`.
///
/// Scans:
/// - `harness/results/YYYYMMDD/*.json` (runs)
/// - `orchestrator/episodes/*/` (episodes)
/// Returns (runs_count, episodes_count).
pub fn reingest_repo(home: &Path) -> io::Result<(usize, usize)> {
    let g = super::FileGraph::new(home)?;
    let mut runs_count = 0usize;
    let results_dir = home.join("harness").join("results");
    if let Ok(days) = fs::read_dir(&results_dir) {
        for de in days.flatten() {
            let day_name = match de.file_name().into_string() {
                Ok(s) if s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()) => s,
                _ => continue,
            };
            let day_path = results_dir.join(&day_name);
            if let Ok(files) = fs::read_dir(&day_path) {
                for fe in files.flatten() {
                    let p = fe.path();
                    if p.is_file() && p.extension().map(|s| s == "json").unwrap_or(false) {
                        let _ = ingest_run_file(&g, &p);
                        runs_count += 1;
                    }
                }
            }
        }
    }

    let mut eps_count = 0usize;
    let episodes_dir = home.join("orchestrator").join("episodes");
    if let Ok(dirs) = fs::read_dir(&episodes_dir) {
        for de in dirs.flatten() {
            let p = de.path();
            if p.is_dir() {
                let _ = ingest_episode_dir(&g, &p);
                eps_count += 1;
            }
        }
    }
    Ok((runs_count, eps_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;
    use crate::graph::FileGraph;

    #[test]
    fn ingest_episode_adds_research_sources_and_edges() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let ep = home.join("orchestrator").join("episodes").join("20250101-000000");
        fs::create_dir_all(&ep).unwrap();
        fs::write(ep.join("plan.md"), "plan").unwrap();
        fs::write(ep.join("summary.md"), "summary").unwrap();
        fs::write(ep.join("research.md"), "research").unwrap();
        let sources = serde_json::json!([
            {"url":"https://arxiv.org/abs/1111.0001","title":"A","year":2023,"authors":["X"],"summary":"s"},
            {"url":"https://arxiv.org/abs/1111.0002","title":"B","year":2022,"authors":["Y"],"summary":"t"}
        ]);
        fs::write(ep.join("sources.json"), serde_json::to_string(&sources).unwrap()).unwrap();

        let g = FileGraph::new(home).unwrap();
        ingest_episode_dir(&g, &ep).unwrap();

        // Read nodes file and count ResearchSource nodes
        let nodes = fs::read_to_string(home.join("graph").join("nodes.jsonl")).unwrap();
        let mut source_count = 0;
        for line in nodes.lines() {
            if let Ok(n) = serde_json::from_str::<Node>(line) {
                if n.kind == "ResearchSource" { source_count += 1; }
            }
        }
        assert_eq!(source_count, 2);

        // Read edges and ensure cites edges exist
        let edges = fs::read_to_string(home.join("graph").join("edges.jsonl")).unwrap();
        let mut cites = 0;
        for line in edges.lines() {
            if let Ok(e) = serde_json::from_str::<Edge>(line) { if e.rel == "cites" { cites += 1; } }
        }
        assert_eq!(cites, 2);
    }
}
