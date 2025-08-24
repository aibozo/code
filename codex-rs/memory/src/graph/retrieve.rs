use super::Node;
use super::FileGraph;
use serde_json::Value as Json;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RunInfo {
    pub ts: String,
    pub context: Option<String>,
    pub ok: i64,
    pub err: i64,
}

#[derive(Debug, Clone)]
pub struct EpisodeInfo {
    pub ts: String,
    pub research_len: usize,
    pub plan_len: usize,
    pub summary_len: usize,
    pub context: Option<String>,
}

/// Load all Run nodes from the graph.
pub fn all_runs(home: &Path) -> std::io::Result<Vec<RunInfo>> {
    let g = FileGraph::new(home)?;
    let file = match OpenOptions::new().read(true).open(&g.nodes_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    fs2::FileExt::lock_shared(&file)?;
    let reader = BufReader::new(&file);
    let mut runs: Vec<RunInfo> = Vec::new();
    for line in reader.lines() {
        let Ok(s) = line else { continue };
        if let Ok(node) = serde_json::from_str::<Node>(&s) {
            if node.kind == "Run" {
                let ts = if let Some((_, rest)) = node.id.split_once(':') { rest.to_string() } else { node.id.clone() };
                let (context, ok, err) = extract_run_props(&node.props);
                runs.push(RunInfo { ts, context, ok, err });
            }
        }
    }
    fs2::FileExt::unlock(&file)?;
    // Sort by ts ascending then caller can reverse/take
    runs.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(runs)
}

/// Load all Episode nodes from the graph.
pub fn all_episodes(home: &Path) -> std::io::Result<Vec<EpisodeInfo>> {
    let g = FileGraph::new(home)?;
    let file = match OpenOptions::new().read(true).open(&g.nodes_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    fs2::FileExt::lock_shared(&file)?;
    let reader = BufReader::new(&file);
    let mut eps: Vec<EpisodeInfo> = Vec::new();
    for line in reader.lines() {
        let Ok(s) = line else { continue };
        if let Ok(node) = serde_json::from_str::<Node>(&s) {
            if node.kind == "Episode" {
                let ts = if let Some((_, rest)) = node.id.split_once(':') { rest.to_string() } else { node.id.clone() };
                let (r, p, su, ctx) = extract_episode_props(&node.props);
                eps.push(EpisodeInfo { ts, research_len: r, plan_len: p, summary_len: su, context: ctx });
            }
        }
    }
    fs2::FileExt::unlock(&file)?;
    eps.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(eps)
}

/// Build a plain-text pack summarizing recent Run nodes.
pub fn recent_runs_pack(home: &Path, limit: usize) -> std::io::Result<String> {
    let g = FileGraph::new(home)?;
    let file = match OpenOptions::new().read(true).open(&g.nodes_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok("No runs found.".to_string())
        }
        Err(e) => return Err(e),
    };
    fs2::FileExt::lock_shared(&file)?;
    let reader = BufReader::new(&file);
    let mut runs: Vec<(String, Option<String>, i64, i64)> = Vec::new();
    for line in reader.lines() {
        let Ok(s) = line else { continue };
        if let Ok(node) = serde_json::from_str::<Node>(&s) {
            if node.kind == "Run" {
                let ts = if let Some((_, rest)) = node.id.split_once(':') { rest.to_string() } else { node.id.clone() };
                let (context, ok, err) = extract_run_props(&node.props);
                runs.push((ts, context, ok, err));
            }
        }
    }
    fs2::FileExt::unlock(&file)?;
    // Sort by ts descending (lexicographic works for YYYYMMDD-HHMMSS)
    runs.sort_by(|a, b| a.0.cmp(&b.0));
    let iter = runs.into_iter().rev().take(limit.max(1));
    let mut out = String::new();
    out.push_str(&format!("Recent Runs (top {})\n", limit.max(1)));
    for (ts, context, ok, err) in iter {
        let ctx_part = context.map(|c| if c.is_empty() { String::new() } else { format!("  [context: {}]", c) }).unwrap_or_default();
        out.push_str(&format!("- {}  ok {} • error {}{}\n", ts, ok, err, ctx_part));
    }
    if out.trim().is_empty() { Ok("No runs found.".to_string()) } else { Ok(out) }
}

/// Build a plain-text pack summarizing recent Run nodes filtered by context.
pub fn recent_runs_pack_for_context(home: &Path, limit: usize, label: &str) -> std::io::Result<String> {
    let mut runs = all_runs(home)?;
    runs.retain(|r| r.context.as_deref() == Some(label));
    runs.sort_by(|a, b| a.ts.cmp(&b.ts));
    let iter = runs.into_iter().rev().take(limit.max(1));
    let mut out = String::new();
    out.push_str(&format!("Recent Runs (context: {}, top {})\n", label, limit.max(1)));
    for r in iter {
        out.push_str(&format!("- {}  ok {} • error {}\n", r.ts, r.ok, r.err));
    }
    if out.trim().is_empty() { Ok(format!("No runs found for context: {}", label)) } else { Ok(out) }
}

/// Detect the most recent non-empty context label from Run nodes.
pub fn detect_recent_context(home: &Path) -> Option<String> {
    let mut runs = all_runs(home).ok()?;
    runs.sort_by(|a, b| a.ts.cmp(&b.ts));
    runs.into_iter().rev().find_map(|r| r.context.filter(|s| !s.is_empty()))
}

/// Build a plain-text pack summarizing prior Episode nodes.
pub fn prior_episodes_pack(home: &Path, limit: usize) -> std::io::Result<String> {
    let g = FileGraph::new(home)?;
    let file = match OpenOptions::new().read(true).open(&g.nodes_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok("No episodes found.".to_string())
        }
        Err(e) => return Err(e),
    };
    fs2::FileExt::lock_shared(&file)?;
    let reader = BufReader::new(&file);
    let mut eps: Vec<(String, usize, usize, usize)> = Vec::new();
    for line in reader.lines() {
        let Ok(s) = line else { continue };
        if let Ok(node) = serde_json::from_str::<Node>(&s) {
            if node.kind == "Episode" {
                let ts = if let Some((_, rest)) = node.id.split_once(':') { rest.to_string() } else { node.id.clone() };
                let (r, p, su, _ctx) = extract_episode_props(&node.props);
                eps.push((ts, r, p, su));
            }
        }
    }
    fs2::FileExt::unlock(&file)?;
    // Sort by ts descending
    eps.sort_by(|a, b| a.0.cmp(&b.0));
    let iter = eps.into_iter().rev().take(limit.max(1));
    let mut out = String::new();
    out.push_str(&format!("Prior Episodes (top {})\n", limit.max(1)));
    for (ts, r, p, su) in iter {
        out.push_str(&format!("- {}  research {}b • plan {}b • summary {}b\n", ts, r, p, su));
    }
    if out.trim().is_empty() { Ok("No episodes found.".to_string()) } else { Ok(out) }
}

fn extract_run_props(props: &Json) -> (Option<String>, i64, i64) {
    let context = props
        .get("context")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ok = props.get("ok").and_then(|v| v.as_i64()).unwrap_or(0);
    let err = props.get("error").and_then(|v| v.as_i64()).unwrap_or(0);
    (context, ok, err)
}

fn extract_episode_props(props: &Json) -> (usize, usize, usize, Option<String>) {
    let r = props.get("research_len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let p = props.get("plan_len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let s = props.get("summary_len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let ctx = props.get("context").and_then(|v| v.as_str()).map(|s| s.to_string());
    (r, p, s, ctx)
}

/// Build a plain-text pack summarizing prior Episode nodes filtered by context.
pub fn prior_episodes_pack_for_context(home: &Path, limit: usize, label: &str) -> std::io::Result<String> {
    let mut eps = all_episodes(home)?;
    eps.retain(|e| e.context.as_deref() == Some(label));
    eps.sort_by(|a, b| a.ts.cmp(&b.ts));
    let iter = eps.into_iter().rev().take(limit.max(1));
    let mut out = String::new();
    out.push_str(&format!("Prior Episodes (context: {}, top {})\n", label, limit.max(1)));
    for e in iter {
        out.push_str(&format!("- {}  research {}b • plan {}b • summary {}b\n", e.ts, e.research_len, e.plan_len, e.summary_len));
    }
    if out.trim().is_empty() { Ok(format!("No episodes found for context: {}", label)) } else { Ok(out) }
}
