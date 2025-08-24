use std::fs;
use std::path::{Path, PathBuf};
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Config;
use crate::agent_tool::AGENT_MANAGER;
use crate::config_types::AgentConfig as ExtAgentConfig;
use crate::research::{query_arxiv_offline, query_arxiv_online, ResearchSource, load_research_state, save_research_state};

/// Simple budget configuration for the orchestration loop.
#[derive(Clone, Copy, Debug, Default)]
pub struct Budgets {
    pub wall_time_secs: Option<u64>,
    pub token_budget: Option<u64>,
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ImproveOptions {
    pub goal: String,
    pub max_attempts: usize,
    /// When true, supervisor should not ask for human approvals for writes.
    pub no_approval: bool,
    pub budgets: Budgets,
    pub context_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImproveResult {
    pub accepted: bool,
    pub attempts: usize,
    pub last_run_day: Option<String>,
    pub last_run_ts: Option<String>,
    pub delta_ok: Option<isize>,
    pub delta_error: Option<isize>,
}

/// Run a minimal improvement cycle: plan → harness → reflect → decide.
///
/// Notes:
/// - Diffs are not generated in this MVP; workspace writes remain under existing
///   user-mediated flows. We focus on pacing, gating, and reporting.
pub async fn run_improve(cfg: &Config, opts: ImproveOptions) -> anyhow::Result<ImproveResult> {
    let cwd = &cfg.cwd;

    // Prepare episode directory for this improvement cycle
    let ep_ts = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let ep_dir = cwd.join("orchestrator").join("episodes").join(&ep_ts);
    if let Err(e) = fs::create_dir_all(&ep_dir) {
        anyhow::bail!(format!("Failed to create episode dir {}: {}", ep_dir.display(), e));
    }

    // Diagnose weakest subsystem and record assessment
    let (target_id, target_label, diagnosis_text) = diagnose_weak_subsystem(cwd);
    let mut assessment = String::new();
    assessment.push_str(&format!("# Assessment\nSelected focus: {} ({})\n\n", target_label, target_id));
    assessment.push_str("Rationale:\n");
    assessment.push_str(&diagnosis_text);
    let _ = fs::write(ep_dir.join("assessment.md"), assessment);

    // Research topic combines user goal with the subsystem focus
    let research_topic = format!(
        "{} — focus on subsystem: {} ({}). Use citations and emphasize practical techniques to apply in this codebase.",
        opts.goal, target_label, target_id
    );

    // Run research pipeline on the topic (writes sources/notes/research artifacts)
    let _ = run_research_pipeline(cfg, &research_topic, &ep_dir, &opts.budgets).await;

    // Planner: generate an actionable plan based on research and the selected subsystem
    let _ = run_planner_step(cfg, &ep_dir, &target_id, &target_label).await;

    let mut attempts = 0usize;
    let mut accepted = false;
    let mut last_run_day: Option<String> = None;
    let mut last_run_ts: Option<String> = None;
    let mut delta_ok: Option<isize> = None;
    let mut delta_err: Option<isize> = None;

    // Ephemeral Q/A context for the Coder; decays after one use
    let mut coder_ephemeral_msgs: Vec<String> = Vec::new();

    // Attempt loop (bounded) with optional wall-time budget
    let wall_budget = opts.budgets.wall_time_secs;
    let run_future = async {
    while attempts < opts.max_attempts {
        attempts += 1;

            // Pause control
            while is_paused() { tokio::time::sleep(std::time::Duration::from_millis(200)).await; }

            // Execute plan: spawn a Coder agent to implement the next slice of the plan
            let coder_out = run_coder_step(cfg, &ep_dir, &coder_ephemeral_msgs)
                .await
                .unwrap_or_else(|e| format!("coder error: {}", e));
            let _ = fs::write(ep_dir.join("coder-output.md"), &coder_out);
            // Decay the ephemeral messages after they were consumed
            coder_ephemeral_msgs.clear();

            // Route any questions embedded in coder output to Planner/Researcher and stage their answers for next coder step
            let mut answers_next: Vec<String> = Vec::new();
            if let Some(a) = route_coder_questions(cfg, &ep_dir, &coder_out).await { answers_next = a; }
            if !answers_next.is_empty() { coder_ephemeral_msgs = answers_next; }

            // Run the harness in workspace-write (as configured by the CLI caller).
            let (day, ts) = run_harness_once(
                cwd,
                None,
                opts.context_label.clone().or(Some("improve".to_string())),
            )
            .await?;
            last_run_day = Some(day.clone());
            last_run_ts = Some(ts.clone());

            // Compute delta vs previous run to gate acceptance
            let (dok, derr) = compute_delta_from_runs_dir(&cwd.join("harness").join("results"))?;
            delta_ok = Some(dok);
            delta_err = Some(derr);

            // Reviewer reflection
            let reflection = subagents::review_stub(dok, derr);
            let _ = fs::write(ep_dir.join("reflection.md"), reflection);

            // Accept if error decreased or ok increased, and no new high-sev
            let security_ok = !last_run_has_security_error(&cwd.join("harness").join("results").join(&day).join(format!("{}.json", ts)))
                && !prev_run_had_more_security_errors(&cwd.join("harness").join("results"));
            let improved = derr <= 0 && dok >= 0 && security_ok;
            if improved {
                accepted = true;
                break;
            }

            // If not accepted, continue next attempt
        }
        anyhow::Ok(())
    };
    if let Some(secs) = wall_budget { let _ = tokio::time::timeout(std::time::Duration::from_secs(secs.max(1)), run_future).await?; } else { let _ = run_future.await?; }

    // Summarize and persist to episode
    {
        let next_targets = if accepted {
            "Consolidate improvements; expand test coverage; consider refactors"
        } else {
            "Reduce failing stages and/or increase passes in next run"
        };
        let mut summary = format!(
            "# Summary\nAttempts: {}\nAccepted: {}\nDelta OK: {}\nDelta Error: {}\nLast run: {}/{}\n",
            attempts,
            accepted,
            delta_ok.map(|v| v.to_string()).unwrap_or_else(|| "n/a".to_string()),
            delta_err.map(|v| v.to_string()).unwrap_or_else(|| "n/a".to_string()),
            last_run_day.clone().unwrap_or_else(|| "n/a".to_string()),
            last_run_ts.clone().unwrap_or_else(|| "n/a".to_string()),
        );
        if opts.budgets.wall_time_secs.is_some()
            || opts.budgets.token_budget.is_some()
            || opts.budgets.max_concurrency.is_some()
        {
            let wall = opts.budgets.wall_time_secs.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let tok = opts.budgets.token_budget.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let conc = opts.budgets.max_concurrency.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            summary.push_str(&format!("Budget: wall_time={}s tokens={} concurrency={}\n", wall, tok, conc));
        }
        summary.push_str(&format!("Next targets: {}\n", next_targets));
        let _ = fs::write(ep_dir.join("summary.md"), summary);
    }

    // Best-effort memory ingestion for this episode and last harness run
    let _ = (|| -> anyhow::Result<()> {
        use codex_memory::graph::{FileGraph, ingest};
        let graph = FileGraph::new(cwd)?;
        if let (Some(day), Some(ts)) = (last_run_day.as_deref(), last_run_ts.as_deref()) {
            let run_path = cwd.join("harness").join("results").join(day).join(format!("{}.json", ts));
            let _ = ingest::ingest_run_file(&graph, &run_path);
        }
        let _ = ingest::ingest_episode_dir(&graph, &ep_dir);
        Ok(())
    })();

    Ok(ImproveResult { accepted, attempts, last_run_day, last_run_ts, delta_ok, delta_error: delta_err })
}

/// Execute the harness runner once and return (day, ts) for the generated result.
async fn run_harness_once(cwd: &Path, seed: Option<u64>, context: Option<String>) -> anyhow::Result<(String, String)> {
    use tokio::process::Command;
    let harness = cwd.join("harness").join("run.sh");
    if !harness.is_file() {
        anyhow::bail!(format!("Harness runner not found at {}", harness.display()));
    }
    let mut cmd = Command::new(&harness);
    cmd.current_dir(cwd);
    if let Some(s) = seed { cmd.arg("--seed").arg(s.to_string()); }
    if let Some(ctx) = context { cmd.arg("--context").arg(ctx); }
    let status = cmd.status().await?;
    if !status.success() {
        anyhow::bail!("Harness run failed");
    }

    // Determine latest run
    let day = chrono::Local::now().format("%Y%m%d").to_string();
    let runs_dir = cwd.join("harness").join("results").join(&day);
    let mut runs: Vec<String> = list_run_ts_in_dir(&runs_dir)?;
    runs.sort();
    let Some(ts) = runs.pop() else { anyhow::bail!(format!("No runs found in {}", runs_dir.display())); };
    Ok((day, ts))
}

fn list_run_ts_in_dir(day_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    if !day_dir.is_dir() { return Ok(out); }
    for ent in fs::read_dir(day_dir)? {
        let ent = ent?;
        let p = ent.path();
        if p.is_file() && p.extension().map(|e| e == "json").unwrap_or(false) {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                // Skip internal summary/index files
                if stem.starts_with('_') { continue; }
                out.push(stem.to_string());
            }
        }
    }
    Ok(out)
}

/// Compute delta between the last two runs across all days.
fn compute_delta_from_runs_dir(results_dir: &Path) -> anyhow::Result<(isize, isize)> {
    // Gather all runs recursively as (path, ok, error)
    #[derive(Clone, Default)]
    struct Counts { ok: i64, err: i64 }
    let mut runs: Vec<(PathBuf, Counts)> = Vec::new();

    let mut stack = vec![results_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for ent in fs::read_dir(&dir).unwrap_or_else(|_| fs::read_dir("/").unwrap()) {
            if let Ok(ent) = ent {
                let p = ent.path();
                if p.is_dir() { stack.push(p); continue; }
                if p.is_file() && p.extension().map(|e| e == "json").unwrap_or(false) {
                    // Only include YYYYMMDD-*.json style run files
                    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                    if name.starts_with("_") { continue; }
                    if !name.chars().take(8).all(|c| c.is_ascii_digit()) { continue; }
                    let text = match fs::read_to_string(&p) { Ok(s) => s, Err(_) => continue };
                    let val: serde_json::Value = match serde_json::from_str(&text) { Ok(v) => v, Err(_) => continue };
                    let mut ok = 0i64; let mut err = 0i64;
                    if let Some(stages) = val.get("stages").and_then(|v| v.as_array()) {
                        for st in stages { match st.get("status").and_then(|v| v.as_str()).unwrap_or("") { "ok" => ok += 1, "error" => err += 1, _ => {} } }
                    }
                    runs.push((p.clone(), Counts { ok, err }));
                }
            }
        }
    }
    // Sort by filename (YYYYMMDD-HHMMSS) across dirs
    runs.sort_by_key(|(p, _)| p.file_name().map(|s| s.to_owned()));
    if runs.len() < 2 { return Ok((0, 0)); }
    let (_, prev) = &runs[runs.len() - 2];
    let (_, last) = &runs[runs.len() - 1];
    let d_ok = last.ok as isize - prev.ok as isize;
    let d_err = last.err as isize - prev.err as isize;
    Ok((d_ok, d_err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn delta_between_two_runs_across_days() {
        let tmp = tempdir().unwrap();
        let results_dir = tmp.path().join("harness").join("results");
        let day1 = results_dir.join("20250101");
        let day2 = results_dir.join("20250102");
        fs::create_dir_all(&day1).unwrap();
        fs::create_dir_all(&day2).unwrap();
        let run1 = serde_json::json!({
            "timestamp": "20250101-000000",
            "stages": [
                {"name":"unit","status":"ok"},
                {"name":"integration","status":"error"}
            ]
        });
        let run2 = serde_json::json!({
            "timestamp": "20250102-000001",
            "stages": [
                {"name":"unit","status":"ok"},
                {"name":"integration","status":"ok"}
            ]
        });
        fs::write(day1.join("20250101-000000.json"), serde_json::to_string_pretty(&run1).unwrap()).unwrap();
        fs::write(day2.join("20250102-000001.json"), serde_json::to_string_pretty(&run2).unwrap()).unwrap();

        let (d_ok, d_err) = compute_delta_from_runs_dir(&results_dir).unwrap();
        assert_eq!(d_ok, 1);
        assert_eq!(d_err, -1);
    }
}

/// Read a harness run JSON and return true if a stage named "security" has status "error".
fn last_run_has_security_error(run_path: &Path) -> bool {
    std::fs::read_to_string(run_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|val| val.get("stages").and_then(|v| v.as_array()).cloned())
        .map(|stages| {
            stages.iter().any(|st| {
                st.get("name").and_then(|v| v.as_str()) == Some("security")
                    && st.get("status").and_then(|v| v.as_str()) == Some("error")
            })
        })
        .unwrap_or(false)
}

/// Compare last two runs across all days and return true if the previous run had more security errors (allowing equality).
fn prev_run_had_more_security_errors(results_dir: &Path) -> bool {
    // Collect all run files
    let mut runs: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![results_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_dir() { stack.push(p); }
                else if p.is_file() && p.extension().map(|e| e == "json").unwrap_or(false) {
                    let stem_ok = p.file_stem().and_then(|s| s.to_str()).map(|s| !s.starts_with('_')).unwrap_or(false);
                    if stem_ok { runs.push(p); }
                }
            }
        }
    }
    runs.sort();
    if runs.len() < 2 { return true; }
    let prev = runs[runs.len() - 2].clone();
    let last = runs[runs.len() - 1].clone();
    let count_errors = |path: &Path| -> i32 {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|val| val.get("stages").and_then(|v| v.as_array()).cloned())
            .map(|stages| {
                stages.iter().filter(|st| st.get("name").and_then(|v| v.as_str()) == Some("security")
                    && st.get("status").and_then(|v| v.as_str()) == Some("error")).count() as i32
            })
            .unwrap_or(0)
    };
    count_errors(&prev) >= count_errors(&last)
}

// Pause/resume control for orchestrator
static ORCH_PAUSED: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

pub fn pause() { ORCH_PAUSED.store(true, Ordering::Relaxed); }
pub fn resume() { ORCH_PAUSED.store(false, Ordering::Relaxed); }
pub fn is_paused() -> bool { ORCH_PAUSED.load(Ordering::Relaxed) }

mod subagents {
    #[derive(Clone, Copy, Debug)]
    pub enum AuthMode { OAuthPreferred, ApiKeyFallback }

    pub fn choose_auth_mode() -> AuthMode {
        // Stub: prefer OAuth; fall back to API key if env hints exist.
        // Real implementation would check configured provider credentials.
        AuthMode::OAuthPreferred
    }

    /// Stub planner that emits a plain-text plan
    pub fn plan_stub(goal: &str) -> String {
        let _auth = choose_auth_mode();
        format!("# Plan\nGoal: {}\n\n- Analyze current failures and cached stages\n- Identify quick wins to reduce errors\n- Re-run harness to observe delta\n", goal)
    }

    /// Stub actor that records a changes note (no diffs applied in MVP)
    pub fn act_stub() -> String {
        "# Changes\nNo code diffs applied in MVP; proceeding to observe current baseline.".to_string()
    }

    /// Stub reviewer that reflects on deltas and suggests next steps
    pub fn review_stub(delta_ok: isize, delta_err: isize) -> String {
        let verdict = if delta_err < 0 { "improved (fewer errors)" } else if delta_ok > 0 { "improved (more passes)" } else { "no improvement" };
        format!(
            "# Reflection\nObserved Δok={:+}, Δerr={:+} → {}.\nNext: reduce failing stages and increase passing ones.",
            delta_ok, delta_err, verdict
        )
    }
}

/// Spawn up to `count` simple research agents using `gpt-5-mini` (OpenAI provider).
/// If `count` is 0, uses a default of at most 5 minis.
pub async fn spawn_research_minis(cfg: &Config, topic: &str, count: usize) -> Vec<String> {
    let k = if count == 0 { 5 } else { count.min(5) };
    let mut ids = Vec::new();
    let mut mgr = AGENT_MANAGER.write().await;
    for _ in 0..k {
        // Configure Code CLI to use OpenAI gpt-5-mini with conservative reasoning
        let mini_cfg = ExtAgentConfig {
            name: "gpt-5-mini".to_string(),
            command: "codex".to_string(),
            args: vec![
                "-c".to_string(), "model_provider=openai".to_string(),
                "-m".to_string(), "gpt-5-mini".to_string(),
                // mini defaults suffice; avoid forcing high reasoning here
            ],
            read_only: true,
            enabled: true,
            description: Some("Simple research mini agent (OpenAI)".to_string()),
            env: None,
        };
        let id = mgr
            .create_agent_with_config(
                "gpt-5-mini".to_string(),
                format!(
                    "Research minis: produce curated notes for: {}\n\n- Work offline using local docs and cache; do not browse.\n- For each selected source, emit: abstract (trimmed), contribution_one_liner, methodology_bullets (3–5), citation.\n- Preserve salient phrasing; avoid over-paraphrasing terms of art.\n- Fan-in budgeting: keep each source note ≈250–300 tokens; include ≤10 strongest sources; mention omitted count if any.\n- Plain text only; no JSON.\n\nOutput: bullet-style sections per source following the schema strictly.",
                    topic
                ),
                Some("research".to_string()),
                Some("concise-notes".to_string()),
                Vec::new(),
                true,
                None,
                mini_cfg.clone(),
            )
            .await;
        ids.push(id);
    }
    ids
}

/// Spawn a single synthesizer agent using GPT‑5 with high reasoning effort.
/// Enforced by AgentManager to run at most one at a time.
pub async fn spawn_synthesizer(cfg: &Config, context: &str, notes: &[String]) -> Option<String> {
    let prompt = {
        let mut p = String::new();
        p.push_str(&format!(
            "Synthesize curated research notes into an executive summary for: {}\n\n",
            context
        ));
        p.push_str(
            "Constraints:\n- Plain text; no JSON.\n- Prioritize clarity and key contributions; preserve citations.\n- Include a short 'What’s next' section.\n- Keep provider pacing implicit; you run as a single synthesizer.\n\nNotes:\n",
        );
        for (i, n) in notes.iter().enumerate() { p.push_str(&format!("[{}] {}\n", i + 1, n)); }
        p
    };
    let mut mgr = AGENT_MANAGER.write().await;
    // Configure Code CLI to use OpenAI gpt-5 with high reasoning effort
    let cfg5 = ExtAgentConfig {
        name: "gpt-5".to_string(),
        command: "codex".to_string(),
        args: vec![
            "-c".to_string(), "model_provider=openai".to_string(),
            "-m".to_string(), "gpt-5".to_string(),
            "-c".to_string(), "model_reasoning_effort=high".to_string(),
        ],
        read_only: true,
        enabled: true,
        description: Some("GPT‑5 synthesizer (high reasoning)".to_string()),
        env: None,
    };
    let id = mgr
        .create_agent_with_config(
            "gpt-5".to_string(),
            prompt,
            Some("synthesis".to_string()),
            Some("executive-summary".to_string()),
            Vec::new(),
            true,
            None,
            cfg5,
        )
        .await;
    Some(id)
}
/// End-to-end research pipeline (M7):
/// - Retrieve sources via offline-first arXiv connector (cached)
/// - Fan-out minis (≤5) with sharded sources; collect notes
/// - Fan-in synthesizer (1) to write research.md
/// - Persist sources.json and notes under the episode directory
pub async fn run_research_pipeline(
    cfg: &Config,
    topic: &str,
    ep_dir: &std::path::Path,
    budgets: &Budgets,
) -> anyhow::Result<()> {
    use tokio::time::{sleep, Duration, Instant};
    // Retrieve normalized sources (online-first with cache fallback)
    let mut all_sources = query_arxiv_online(&cfg.cwd, topic, None, 50).await;
    if all_sources.is_empty() {
        all_sources = query_arxiv_offline(&cfg.cwd, topic, None);
    }

    // Determine next volley from the top of the stack (highest rated), excluding used
    let mut state = load_research_state(ep_dir);
    let mut used: std::collections::HashSet<String> = state.used_urls.iter().cloned().collect();
    let mut to_use: Vec<ResearchSource> = Vec::new();
    for s in &all_sources {
        if !used.contains(&s.url) { to_use.push(s.clone()); }
        if to_use.len() >= 10 { break; }
    }
    // If still insufficient, allow reuse of earlier top items (bounded)
    if to_use.is_empty() && !all_sources.is_empty() {
        to_use.extend(all_sources.iter().take(10).cloned());
    }

    // Save sources.json regardless (empty is OK)
    let sources_json = serde_json::to_string_pretty(&to_use).unwrap_or_else(|_| "[]".to_string());
    let _ = fs::write(ep_dir.join("sources.json"), sources_json);

    // Prepare minis count by budget
    const MINI_COST: i64 = 1_000;
    const SYNTH_COST: i64 = 3_000;
    let mut tokens_left: i64 = budgets.token_budget.map(|v| v as i64).unwrap_or(i64::MAX);
    let desired_minis = 5usize;
    let mut allowed = desired_minis;
    if tokens_left != i64::MAX {
        let by_tokens = (tokens_left / MINI_COST).max(0) as usize;
        allowed = allowed.min(by_tokens);
    }
    let k = allowed.clamp(1, 5);

    // Shard sources across minis (round-robin); allow empty shards if no sources
    let mut shards: Vec<Vec<ResearchSource>> = vec![Vec::new(); k];
    for (i, src) in to_use.iter().cloned().enumerate() { shards[i % k].push(src); }

    // Spawn minis with shard-specific prompts
    let mut mgr = AGENT_MANAGER.write().await;
    let mut mini_ids: Vec<String> = Vec::new();
    for (idx, shard) in shards.iter().enumerate() {
        let mut prompt = String::new();
        prompt.push_str(&format!("Research minis: curated notes for: {}\n\n", topic));
        prompt.push_str("Schema per source: abstract (trimmed), contribution_one_liner, methodology_bullets (3–5), citation.\n");
        prompt.push_str("Budget: ≈250–300 tokens per source; include ≤10 total; mention omitted count if any.\n\nSources:\n");
        if shard.is_empty() {
            prompt.push_str("(no cached sources; use local knowledge and cached docs only)\n");
        } else {
            for s in shard {
                prompt.push_str(&format!(
                    "- {} ({}), {}
  URL: {}
  Abstract: {}
",
                    s.title, s.year, s.authors.join(", "), s.url, s.summary
                ));
            }
        }
        prompt.push_str("\nOutput plain text only; no JSON.\n");

        let mini_cfg = ExtAgentConfig {
            name: "gpt-5-mini".to_string(),
            command: "codex".to_string(),
            args: vec![
                "-c".to_string(), "model_provider=openai".to_string(),
                "-m".to_string(), "gpt-5-mini".to_string(),
            ],
            read_only: true,
            enabled: true,
            description: Some(format!("Research mini shard {}", idx + 1)),
            env: None,
        };
        let id = mgr
            .create_agent_with_config(
                "gpt-5-mini".to_string(),
                prompt,
                Some("research".to_string()),
                Some("curated-notes".to_string()),
                Vec::new(),
                true,
                None,
                mini_cfg,
            )
            .await;
        mini_ids.push(id);
    }
    drop(mgr);
    tokens_left = tokens_left.saturating_sub((k as i64) * MINI_COST);

    // Await minis with a soft wall-time budget slice (e.g., ≤ 60s or ≤ total budget)
    let minis_timeout = budgets.wall_time_secs.unwrap_or(120).min(300);
    let deadline = Instant::now() + Duration::from_secs(minis_timeout as u64);
    let mut mini_outputs: Vec<String> = Vec::new();
    loop {
        let mut done = 0usize;
        let mut outputs = Vec::new();
        {
            let mgr = AGENT_MANAGER.read().await;
            for id in &mini_ids {
                if let Some(a) = mgr.get_agent(id) {
                    match a.status {
                        crate::agent_tool::AgentStatus::Completed => {
                            done += 1;
                            outputs.push(a.result.unwrap_or_default());
                        }
                        crate::agent_tool::AgentStatus::Failed | crate::agent_tool::AgentStatus::Cancelled => {
                            done += 1;
                            outputs.push(a.error.unwrap_or_else(|| "mini failed".to_string()));
                        }
                        _ => {}
                    }
                }
            }
        }
        if done == mini_ids.len() { mini_outputs = outputs; break; }
        if Instant::now() >= deadline { mini_outputs = outputs; break; }
        sleep(Duration::from_millis(200)).await;
    }

    // Persist mini notes
    for (i, notes) in mini_outputs.iter().enumerate() {
        let path = ep_dir.join(format!("notes-mini-{}.md", i + 1));
        let _ = fs::write(path, notes);
    }

    // Write research_report.md summarizing this volley
    {
        let mut report = String::new();
        report.push_str(&format!("# Research Report\nTopic: {}\n\n", topic));
        report.push_str(&format!("Sources considered: {}\n", all_sources.len()));
        report.push_str(&format!("Sources used in this volley: {}\n", to_use.len()));
        let omitted = all_sources.len().saturating_sub(to_use.len());
        if omitted > 0 { report.push_str(&format!("Omitted (still available): {}\n", omitted)); }
        report.push_str("\nUsed sources:\n");
        for s in &to_use {
            let first_author = s.authors.get(0).cloned().unwrap_or_default();
            report.push_str(&format!("- {} ({}), {} — {}\n", s.title, s.year, first_author, s.url));
        }
        let _ = fs::write(ep_dir.join("research_report.md"), report);
    }

    // Update research state to record used URLs for this volley
    for s in &to_use { used.insert(s.url.clone()); }
    state.used_urls = used.into_iter().collect();
    let _ = save_research_state(ep_dir, &state);

    // Spawn synthesizer if we have budget left
    if tokens_left >= SYNTH_COST {
        if let Some(synth_id) = spawn_synthesizer(cfg, topic, &mini_outputs).await {
            // Wait for completion with pacing (queue ensures single `gpt-5`)
            let synth_timeout = budgets.wall_time_secs.unwrap_or(120).min(600);
            let deadline = Instant::now() + Duration::from_secs(synth_timeout as u64);
            let mut result: Option<String> = None;
            loop {
                let (status, out) = {
                    let mgr = AGENT_MANAGER.read().await;
                    if let Some(a) = mgr.get_agent(&synth_id) {
                        (a.status.clone(), a.result.clone())
                    } else {
                        (crate::agent_tool::AgentStatus::Failed, None)
                    }
                };
                match status {
                    crate::agent_tool::AgentStatus::Completed => { result = out; break; }
                    crate::agent_tool::AgentStatus::Failed | crate::agent_tool::AgentStatus::Cancelled => { break; }
                    _ => {}
                }
                if Instant::now() >= deadline { break; }
                sleep(Duration::from_millis(250)).await;
            }
            if let Some(text) = result { let _ = fs::write(ep_dir.join("research.md"), text); }
        }
    }

    // Best-effort ingestion of episode artifacts
    let _ = (|| -> anyhow::Result<()> {
        use codex_memory::graph::{FileGraph, ingest};
        let graph = FileGraph::new(&cfg.cwd)?;
        let _ = ingest::ingest_episode_dir(&graph, ep_dir);
        Ok(())
    })();

    // Append to daily research summary report
    {
        use std::io::Write;
        let day = chrono::Local::now().format("%Y%m%d").to_string();
        let reports_dir = cfg.cwd.join("orchestrator").join("reports");
        let _ = std::fs::create_dir_all(&reports_dir);
        let path = reports_dir.join(format!("research-{}.md", day));
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let line = format!(
            "- {} • topic: {} • used: {} of {}\n",
            ep_dir.file_name().and_then(|s| s.to_str()).unwrap_or("episode"),
            topic,
            to_use.len(),
            all_sources.len()
        );
        let _ = f.write_all(line.as_bytes());
    }

    Ok(())
}

/// Fixed finite taxonomy of subsystems (≤ 20) for targeted long-horizon improvements.
fn subsystem_taxonomy() -> Vec<(&'static str, &'static str)> {
    vec![
        ("planning-integration", "Planning & phase integration"),
        ("context-handoff", "Context handoff between agents"),
        ("context-management", "Conversation and context management"),
        ("memory-graph", "Memory graph ingestion & retrieval"),
        ("research-orchestration", "Research orchestration (connectors, sharding)"),
        ("subagent-scheduling", "Subagent caps, queueing, pacing"),
        ("harness-integration", "Harness integration and gating"),
        ("testing-coverage", "Tests and coverage heuristics"),
        ("caching-determinism", "Caching and determinism"),
        ("rate-limiting", "Provider rate limiting"),
        ("error-handling", "Error handling & retries"),
        ("sandboxes-approvals", "Sandboxes and approvals"),
        ("patch-application", "Patch generation & application"),
        ("browser-integration", "Browser integration and UX"),
        ("reporting", "Reports and observability"),
        ("docs-alignment", "Docs and milestone alignment"),
    ]
}

/// Lightweight heuristic to pick a weakest subsystem from the taxonomy.
/// This MVP favors planning integration due to absence of a true Planner phase.
fn diagnose_weak_subsystem(repo_root: &std::path::Path) -> (String, String, String) {
    // Signals
    let mut scores: std::collections::HashMap<&str, i32> = std::collections::HashMap::new();
    for (id, _label) in subsystem_taxonomy() { scores.insert(id, 0); }

    // 1) Planning integration missing: we currently rely on a simple stub
    if repo_root.join("codex-rs").join("core").join("src").join("plan_tool.rs").is_file() {
        *scores.get_mut("planning-integration").unwrap() += 2;
    }
    // 2) Context handoff: no explicit cross-agent dialog channel exists
    if repo_root.join("codex-rs").join("core").join("src").join("agent_tool.rs").is_file() {
        *scores.get_mut("context-handoff").unwrap() += 1;
    }
    // 3) Reporting: room to improve summarized research + planner reports
    *scores.get_mut("reporting").unwrap() += 1;

    // Pick highest score; default to planning-integration
    let mut best_id = "planning-integration";
    let mut best_score = i32::MIN;
    for (id, sc) in scores.iter() { if *sc > best_score { best_score = *sc; best_id = id; } }
    let label = subsystem_taxonomy().into_iter().find(|(i, _)| *i == best_id).map(|(_, l)| l.to_string()).unwrap_or_else(|| best_id.to_string());
    let reason = match best_id {
        "planning-integration" => "Planner step not integrated as a distinct phase; plan.md is currently produced via a simple stub. Introducing a Planner that consumes research artifacts can yield higher-quality, actionable plans with citations.".to_string(),
        "context-handoff" => "Agents lack an explicit context handoff protocol beyond artifacts. Introducing clearer handoff channels improves coordination between Researcher, Planner, and Coder.".to_string(),
        other => format!("Heuristic flagged subsystem '{}' for improvement.", other),
    };
    (best_id.to_string(), label, reason)
}

/// Spawn a Planner agent to generate an actionable plan based on research artifacts and target subsystem.
async fn run_planner_step(cfg: &Config, ep_dir: &std::path::Path, target_id: &str, target_label: &str) -> anyhow::Result<()> {
    let research_md = std::fs::read_to_string(ep_dir.join("research.md")).unwrap_or_default();
    let sources_json = std::fs::read_to_string(ep_dir.join("sources.json")).unwrap_or_else(|_| "[]".to_string());
    let dir_context = list_key_dirs(&cfg.cwd.join("codex-rs"));
    let prompt = format!(
        "You are the Planner. Produce a concrete, plain-text plan for integrating research into the codebase.\n\nFocus subsystem: {} ({})\n\nContext:\n- Key directories:\n{}\n- Research synthesis (excerpt):\n{}\n\nSources (JSON):\n{}\n\nImportant:
- You may (and should) read `harness/results/` JSON files to understand recent stage outcomes and trends. Use these results together with your codebase analysis to justify which subsystem deserves focus and to prioritize research angles.
- Tie observations from the harness to specific files/modules when possible. Keep reasoning crisp and practical; reference paths you inspected.
- Prefer stable, auditable changes that improve the selected subsystem without regressing others.

Write a plan in 3–5 sections with:
- Objectives specific to this subsystem
- Proposed changes (files/modules, functions to touch)
- Acceptance criteria and quick validation steps
- Citations in-line as [#] indices corresponding to sources order when relevant

Output: plain text only, suitable to save as plan.md.",
        target_label, target_id, dir_context, trim_for_prompt(&research_md, 1200), trim_for_prompt(&sources_json, 1200)
    );

    // Spawn with gpt-5 high reasoning (read-only)
    let cfg5 = ExtAgentConfig {
        name: "gpt-5".to_string(),
        command: "codex".to_string(),
        args: vec![
            "-c".to_string(), "model_provider=openai".to_string(),
            "-m".to_string(), "gpt-5".to_string(),
            "-c".to_string(), "model_reasoning_effort=high".to_string(),
            // Enable semantic compression memory for persistent planner context
            "-c".to_string(), "memory.enabled=true".to_string(),
            "-c".to_string(), "memory.summarize_on_prune=true".to_string(),
            "-c".to_string(), "memory.inject.max_items=2".to_string(),
            "-c".to_string(), "memory.inject.max_chars=500".to_string(),
        ],
        read_only: true,
        enabled: true,
        description: Some("Planner (gpt‑5) to generate actionable plan".to_string()),
        env: None,
    };
    let id = {
        let mut mgr = AGENT_MANAGER.write().await;
        mgr
            .create_agent_with_config(
                "gpt-5".to_string(),
                prompt,
                Some("plan".to_string()),
                Some("actionable-plan".to_string()),
                Vec::new(),
                true,
                None,
                cfg5,
            )
            .await
    };
    // Wait for completion (bounded)
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(120);
    let mut result: Option<String> = None;
    loop {
        let (status, out) = {
            let mgr = AGENT_MANAGER.read().await;
            if let Some(a) = mgr.get_agent(&id) { (a.status.clone(), a.result.clone()) } else { (crate::agent_tool::AgentStatus::Failed, None) }
        };
        match status {
            crate::agent_tool::AgentStatus::Completed => { result = out; break; }
            crate::agent_tool::AgentStatus::Failed | crate::agent_tool::AgentStatus::Cancelled => { break; }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline { break; }
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    }
    if let Some(mut plan) = result {
        // If planner requests research clarifications, route them and optionally refine the plan once
        let mut researcher_qs: Vec<String> = Vec::new();
        for line in plan.lines() {
            let l = line.trim();
            if let Some(q) = l.strip_prefix("ASK_RESEARCHER:") { researcher_qs.push(q.trim().to_string()); }
        }
        if !researcher_qs.is_empty() {
            let mut answers: Vec<String> = Vec::new();
            for q in researcher_qs { if let Some(a) = ask_researcher_question(cfg, ep_dir, &q).await { answers.push(a); } }
            if !answers.is_empty() {
                // Rerun planner with answers to refine the plan
                let refine_prompt = format!(
                    "You are the Planner. Refine your plan with these research answers.\n\nPrior draft:\n{}\n\nAnswers from Researcher:\n{}\n\nProduce a clean, final plan (plain text) integrating the answers.",
                    trim_for_prompt(&plan, 2400),
                    answers.join("\n")
                );
                let cfg5b = ExtAgentConfig { name: "gpt-5".to_string(), command: "codex".to_string(), args: vec!["-c".to_string(), "model_provider=openai".to_string(), "-m".to_string(), "gpt-5".to_string(), "-c".to_string(), "model_reasoning_effort=medium".to_string(), "-c".to_string(), "memory.enabled=true".to_string(),], read_only: true, enabled: true, description: Some("Planner refine".to_string()), env: None };
                let id2 = { let mut mgr = AGENT_MANAGER.write().await; mgr.create_agent_with_config("gpt-5".to_string(), refine_prompt, Some("plan".to_string()), Some("refine".to_string()), Vec::new(), true, None, cfg5b).await };
                if let Some(p2) = wait_agent_result(id2).await { plan = p2; }
            }
        }
        let _ = std::fs::write(ep_dir.join("plan.md"), plan);
        // Append to daily planner report
        use std::io::Write;
        let day = chrono::Local::now().format("%Y%m%d").to_string();
        let reports_dir = cfg.cwd.join("orchestrator").join("reports");
        let _ = std::fs::create_dir_all(&reports_dir);
        let path = reports_dir.join(format!("planner-{}.md", day));
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let line = format!(
            "- {} • subsystem: {} ({})\n",
            ep_dir.file_name().and_then(|s| s.to_str()).unwrap_or("episode"),
            target_label,
            target_id
        );
        let _ = f.write_all(line.as_bytes());
    }
    Ok(())
}

fn list_key_dirs(codex_rs: &std::path::Path) -> String {
    let mut lines = String::new();
    for name in ["core", "tui", "memory", "browser", "protocol", "apply-patch" ] {
        let p = codex_rs.join(name);
        if p.is_dir() { lines.push_str(&format!("  - {}/\n", p.strip_prefix(codex_rs).unwrap_or(&p).display())); }
    }
    lines
}

fn trim_for_prompt(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars { return s.to_string(); }
    let head = &s[..max_chars];
    format!("{}\n[…trimmed…]", head)
}

/// Spawn a Coder agent to implement the next slice of the plan with approvals (on-request) and workspace-write.
async fn run_coder_step(cfg: &Config, ep_dir: &std::path::Path, ephemeral_msgs: &[String]) -> anyhow::Result<String> {
    let plan = std::fs::read_to_string(ep_dir.join("plan.md")).unwrap_or_default();
    let research = std::fs::read_to_string(ep_dir.join("research.md")).unwrap_or_default();
    let planner_log = std::fs::read_to_string(ep_dir.join("assessment.md")).unwrap_or_default();
    let extra = if ephemeral_msgs.is_empty() { String::new() } else { format!("\n\nAnswers from Planner/Researcher (use once):\n{}\n", ephemeral_msgs.join("\n")) };
    let prompt = format!(
        "You are the Coder. Implement the next small, safe subset of this plan.\n\nPlan:\n{}\n\nResearch (excerpt):\n{}\n\nAssessment:\n{}\n\nGuidelines:\n- Make minimal, focused changes to progress the plan.\n- Prefer small patches; keep diffs targeted.\n- When uncertain, write a short note to Planner (in output) and stop early.\n- Do not run destructive commands.\n- Ask for approvals when writing files (on-request policy).\n- After applying changes, write a brief status indicating what was changed.",
        trim_for_prompt(&plan, 3000),
        trim_for_prompt(&research, 1500),
        trim_for_prompt(&planner_log, 800),
    );
    let prompt = format!("{}{}", prompt, extra);

    let mut coder_args = vec![
        // Enable memory features for persistent coder context
        "-c".to_string(), "memory.enabled=true".to_string(),
        "-c".to_string(), "memory.summarize_on_prune=true".to_string(),
        "-c".to_string(), "memory.inject.max_items=2".to_string(),
        "-c".to_string(), "memory.inject.max_chars=500".to_string(),
    ];
    // Propagate GODMODE to subagent if active
    if matches!(cfg.sandbox_policy, crate::protocol::SandboxPolicy::DangerFullAccess) {
        coder_args.push("-s".to_string()); coder_args.push("danger-full-access".to_string());
        coder_args.push("-a".to_string()); coder_args.push("never".to_string());
    }
    let coder_cfg = ExtAgentConfig {
        name: "codex".to_string(),
        command: "codex".to_string(),
        args: coder_args,
        read_only: false, // allow writes; approvals are on-request via agent_tool
        enabled: true,
        description: Some("Coder to apply plan changes".to_string()),
        env: None,
    };

    let id = {
        let mut mgr = AGENT_MANAGER.write().await;
        mgr
            .create_agent_with_config(
                "codex".to_string(),
                prompt,
                Some("code".to_string()),
                Some("apply-plan-step".to_string()),
                Vec::new(),
                false,
                None,
                coder_cfg,
            )
            .await
    };

    // Wait for coder to complete/fail (short window); return output or error message
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(180);
    let mut result: Option<Result<String, String>> = None;
    loop {
        let (status, out, err) = {
            let mgr = AGENT_MANAGER.read().await;
            if let Some(a) = mgr.get_agent(&id) { (a.status.clone(), a.result.clone(), a.error.clone()) } else { (crate::agent_tool::AgentStatus::Failed, None, Some("not found".to_string())) }
        };
        match status {
            crate::agent_tool::AgentStatus::Completed => { result = Some(Ok(out.unwrap_or_default())); break; }
            crate::agent_tool::AgentStatus::Failed | crate::agent_tool::AgentStatus::Cancelled => { result = Some(Err(err.unwrap_or_else(|| "unknown coder error".to_string()))); break; }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline { result = Some(Err("coder timed out".to_string())); break; }
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }
    match result.unwrap_or(Err("no result".to_string())) { Ok(s) => Ok(s), Err(e) => Ok(format!("[coder] {}", e)) }
}

/// Parse coder output for embedded questions and route to Planner/Researcher as needed.
/// Looks for lines starting with "ASK_PLANNER:" or "ASK_RESEARCHER:" and returns concise answers
/// prepped for the next coder step; answers decay after a single use.
async fn route_coder_questions(cfg: &Config, ep_dir: &std::path::Path, coder_output: &str) -> Option<Vec<String>> {
    let mut planner_qs: Vec<String> = Vec::new();
    for line in coder_output.lines() {
        let l = line.trim();
        if let Some(q) = l.strip_prefix("ASK_PLANNER:") { planner_qs.push(q.trim().to_string()); }
    }
    if planner_qs.is_empty() { return None; }
    let mut answers: Vec<String> = Vec::new();
    for q in planner_qs { if let Some(a) = ask_planner_question(cfg, ep_dir, &q).await { answers.push(format!("Planner: {}", a)); } }
    if answers.is_empty() { None } else { Some(answers) }
}

async fn ask_planner_question(cfg: &Config, ep_dir: &std::path::Path, question: &str) -> Option<String> {
    let plan = std::fs::read_to_string(ep_dir.join("plan.md")).unwrap_or_default();
    let prompt = format!(
        "Planner, answer concisely.\nQuestion: {}\nContext (plan excerpt):\n{}\n",
        question,
        trim_for_prompt(&plan, 1200)
    );
    let cfg5 = ExtAgentConfig {
        name: "gpt-5".to_string(),
        command: "codex".to_string(),
        args: vec![
            "-c".to_string(), "model_provider=openai".to_string(),
            "-m".to_string(), "gpt-5".to_string(),
            "-c".to_string(), "model_reasoning_effort=medium".to_string(),
            "-c".to_string(), "memory.enabled=true".to_string(),
            "-c".to_string(), "memory.summarize_on_prune=true".to_string(),
        ],
        read_only: true,
        enabled: true,
        description: Some("Planner Q&A".to_string()),
        env: None,
    };
    let id = {
        let mut mgr = AGENT_MANAGER.write().await;
        mgr
            .create_agent_with_config(
                "gpt-5".to_string(),
                prompt,
                Some("plan".to_string()),
                Some("answer".to_string()),
                Vec::new(),
                true,
                None,
                cfg5,
            )
            .await
    };
    wait_agent_result(id).await
}

async fn ask_researcher_question(cfg: &Config, ep_dir: &std::path::Path, question: &str) -> Option<String> {
    let sources = std::fs::read_to_string(ep_dir.join("sources.json")).unwrap_or_else(|_| "[]".to_string());
    let research = std::fs::read_to_string(ep_dir.join("research.md")).unwrap_or_default();
    let prompt = format!(
        "Researcher, answer concisely with citations [#] when relevant.\nQuestion: {}\nContext (brief research excerpt):\n{}\nSources (JSON excerpt):\n{}\n",
        question,
        trim_for_prompt(&research, 800),
        trim_for_prompt(&sources, 800)
    );
    let mini_cfg = ExtAgentConfig {
        name: "gpt-5-mini".to_string(),
        command: "codex".to_string(),
        args: vec![
            "-c".to_string(), "model_provider=openai".to_string(),
            "-m".to_string(), "gpt-5-mini".to_string(),
            "-c".to_string(), "memory.enabled=true".to_string(),
            "-c".to_string(), "memory.summarize_on_prune=true".to_string(),
        ],
        read_only: true,
        enabled: true,
        description: Some("Researcher Q&A".to_string()),
        env: None,
    };
    let id = {
        let mut mgr = AGENT_MANAGER.write().await;
        mgr
            .create_agent_with_config(
                "gpt-5-mini".to_string(),
                prompt,
                Some("research".to_string()),
                Some("answer".to_string()),
                Vec::new(),
                true,
                None,
                mini_cfg,
            )
            .await
    };
    wait_agent_result(id).await
}

async fn wait_agent_result(id: String) -> Option<String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(60);
    loop {
        let (status, out) = {
            let mgr = AGENT_MANAGER.read().await;
            if let Some(a) = mgr.get_agent(&id) { (a.status.clone(), a.result.clone()) } else { (crate::agent_tool::AgentStatus::Failed, None) }
        };
        match status {
            crate::agent_tool::AgentStatus::Completed => { return out; }
            crate::agent_tool::AgentStatus::Failed | crate::agent_tool::AgentStatus::Cancelled => { return None; }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline { return None; }
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }
}
