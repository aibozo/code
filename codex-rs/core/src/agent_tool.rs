use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config_types::AgentConfig;
use crate::openai_tools::JsonSchema;
use crate::openai_tools::OpenAiTool;
use crate::openai_tools::ResponsesApiTool;
use crate::protocol::AgentInfo;
use crate::protocol::AgentStatusUpdateEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;

// Agent status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// Agent information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub batch_id: Option<String>,
    pub model: String,
    pub prompt: String,
    pub context: Option<String>,
    pub output_goal: Option<String>,
    pub files: Vec<String>,
    pub read_only: bool,
    pub status: AgentStatus,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub progress: Vec<String>,
    pub worktree_path: Option<String>,
    pub branch_name: Option<String>,
    #[serde(skip)]
    #[allow(dead_code)]
    pub config: Option<AgentConfig>,
}

// Global agent manager
lazy_static::lazy_static! {
    pub static ref AGENT_MANAGER: Arc<RwLock<AgentManager>> = Arc::new(RwLock::new(AgentManager::new()));
}

pub struct AgentManager {
    agents: HashMap<String, Agent>,
    handles: HashMap<String, JoinHandle<()>>,
    event_sender: Option<mpsc::UnboundedSender<Event>>,
    /// Simple concurrency caps by logical model name (e.g., "gpt-5", "gpt-5-mini").
    caps_by_model: HashMap<String, usize>,
    /// Queue of agent IDs waiting to start due to concurrency caps.
    queued: Vec<String>,
    /// Minimum pacing interval per model; a new start will wait at least this long since the last start.
    min_interval_by_model: HashMap<String, std::time::Duration>,
    /// Last actual start time per model, for pacing.
    last_started_at_by_model: HashMap<String, std::time::Instant>,
    /// Agents that have a delayed start scheduled (to avoid duplicate scheduling).
    scheduled: HashSet<String>,
}

impl AgentManager {
    pub fn new() -> Self {
        let mut caps = HashMap::new();
        // Hard caps per requirements: allow only one GPT‑5 at a time; up to five GPT‑5‑mini.
        caps.insert("gpt-5".to_string(), 1);
        caps.insert("gpt-5-mini".to_string(), 5);
        let mut min_intervals = HashMap::new();
        // Provider pacing requirements from M7 docs:
        // - Linearize GPT‑5 (OAuth) with ≥2s spacing between requests
        // - Pace minis via queue with ≥0.5s spacing
        min_intervals.insert("gpt-5".to_string(), std::time::Duration::from_millis(2000));
        min_intervals.insert("gpt-5-mini".to_string(), std::time::Duration::from_millis(500));
        Self {
            agents: HashMap::new(),
            handles: HashMap::new(),
            event_sender: None,
            caps_by_model: caps,
            queued: Vec::new(),
            min_interval_by_model: min_intervals,
            last_started_at_by_model: HashMap::new(),
            scheduled: HashSet::new(),
        }
    }

    pub fn set_event_sender(&mut self, sender: mpsc::UnboundedSender<Event>) {
        self.event_sender = Some(sender);
    }

    fn send_agent_status_update(&self) {
        if let Some(ref sender) = self.event_sender {
            let agents: Vec<AgentInfo> = self
                .agents
                .values()
                .map(|agent| {
                    // Just show the model name - status provides the useful info
                    let name = agent.model.clone();

                    AgentInfo {
                        id: agent.id.clone(),
                        name,
                        status: format!("{:?}", agent.status).to_lowercase(),
                        model: Some(agent.model.clone()),
                        created_at: Some(agent.created_at.to_rfc3339()),
                        started_at: agent.started_at.map(|t| t.to_rfc3339()),
                        completed_at: agent.completed_at.map(|t| t.to_rfc3339()),
                        error: agent.error.clone(),
                        progress_tail: Some(agent.progress.iter().rev().take(3).cloned().collect::<Vec<_>>().into_iter().rev().collect()),
                        worktree_path: agent.worktree_path.clone(),
                        branch_name: agent.branch_name.clone(),
                    }
                })
                .collect();

            // Get context and task from the first agent (they're all the same)
            let (context, task) = self
                .agents
                .values()
                .next()
                .map(|agent| (agent.context.clone(), agent.output_goal.clone()))
                .unwrap_or((None, None));

            let event = Event {
                id: uuid::Uuid::new_v4().to_string(),
                msg: EventMsg::AgentStatusUpdate(AgentStatusUpdateEvent {
                    agents,
                    context,
                    task,
                }),
            };

            let _ = sender.send(event);
        }
    }

    /// Returns number of agents currently Running for a specific logical model.
    fn running_count_for_model(&self, model: &str) -> usize {
        self.agents
            .values()
            .filter(|a| a.model == model && a.status == AgentStatus::Running)
            .count()
    }

    /// Try to start the given agent now (respecting caps and pacing).
    /// Returns true if started immediately, false if queued or scheduled for later.
    async fn try_start_agent_now(&mut self, agent_id: &str) -> bool {
        let model = match self.agents.get(agent_id) { Some(a) => a.model.clone(), None => return false };
        let cap = self.caps_by_model.get(&model).copied().unwrap_or(usize::MAX);
        let active = self.running_count_for_model(&model);
        if active >= cap {
            // Queue the agent; will be started when a slot frees up
            if !self.queued.iter().any(|id| id == agent_id) {
                self.queued.push(agent_id.to_string());
            }
            return false;
        }
        // Determine pacing delay based on last start time for this model
        let now = std::time::Instant::now();
        let min_interval = self
            .min_interval_by_model
            .get(&model)
            .copied()
            .unwrap_or(std::time::Duration::from_millis(0));
        let delay = if let Some(last) = self.last_started_at_by_model.get(&model) {
            let elapsed = now.saturating_duration_since(*last);
            if elapsed < min_interval {
                min_interval - elapsed
            } else {
                std::time::Duration::from_millis(0)
            }
        } else {
            std::time::Duration::from_millis(0)
        };

        if delay.as_millis() > 0 {
            // Schedule a delayed start, keep status as Pending until it actually starts
            if self.scheduled.insert(agent_id.to_string()) {
                // Add a pacing hint to the agent's progress
                if let Some(agent) = self.agents.get_mut(agent_id) {
                    agent.progress.push(format!(
                        "{}: pacing: waiting {}ms before start (model {})",
                        Utc::now().format("%H:%M:%S"),
                        delay.as_millis(),
                        model
                    ));
                }
                // Emit status update with progress hint
                self.send_agent_status_update();
                let agent_id_s = agent_id.to_string();
                let model_s = model.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    // Re-lock manager and ensure capacity still allows start
                    let mut mgr = AGENT_MANAGER.write().await;
                    // If agent no longer present (cancelled), bail
                    if !mgr.agents.contains_key(&agent_id_s) { return; }
                    let cap = mgr.caps_by_model.get(&model_s).copied().unwrap_or(usize::MAX);
                    let active = mgr.running_count_for_model(&model_s);
                    if active >= cap {
                        // Move to queue; will be picked up when capacity frees
                        if !mgr.queued.iter().any(|id| id == &agent_id_s) {
                            mgr.queued.push(agent_id_s.clone());
                        }
                        mgr.scheduled.remove(&agent_id_s);
                        // Emit status update to reflect queuing
                        mgr.send_agent_status_update();
                        return;
                    }
                    // Start now
                    let mut config = None;
                    if let Some(agent) = mgr.agents.get_mut(&agent_id_s) {
                        agent.status = AgentStatus::Running;
                        if agent.started_at.is_none() { agent.started_at = Some(Utc::now()); }
                        config = agent.config.clone();
                    }
                    let agent_id_clone = agent_id_s.clone();
                    let handle = tokio::spawn(async move { execute_agent(agent_id_clone, config).await });
                    mgr.handles.insert(agent_id_s.clone(), handle);
                    mgr.last_started_at_by_model.insert(model_s.clone(), std::time::Instant::now());
                    mgr.scheduled.remove(&agent_id_s);
                    mgr.send_agent_status_update();
                });
            }
            return false;
        }

        // Start immediately
        let mut config = None;
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.status = AgentStatus::Running;
            if agent.started_at.is_none() { agent.started_at = Some(Utc::now()); }
            config = agent.config.clone();
        }
        let agent_id_clone = agent_id.to_string();
        let handle = tokio::spawn(async move { execute_agent(agent_id_clone, config).await });
        self.handles.insert(agent_id.to_string(), handle);
        self.last_started_at_by_model.insert(model.clone(), std::time::Instant::now());
        self.send_agent_status_update();
        true
    }

    /// Attempt to start a queued agent for the given model, if capacity is available.
    fn maybe_start_next_for_model(&mut self, model: &str) {
        let cap = self.caps_by_model.get(model).copied().unwrap_or(usize::MAX);
        if self.running_count_for_model(model) >= cap { return; }
        if let Some(pos) = self
            .queued
            .iter()
            .position(|id| self.agents.get(id).map(|a| a.model.as_str()) == Some(model))
        {
            let id = self.queued.remove(pos);
            // Defer to the pacing-aware starter
            // Note: we cannot .await in this method; schedule the start via a detached task
            tokio::spawn(async move {
                let mut mgr = AGENT_MANAGER.write().await;
                let _ = mgr.try_start_agent_now(&id).await;
            });
        }
    }

    pub async fn create_agent(
        &mut self,
        model: String,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        read_only: bool,
        batch_id: Option<String>,
    ) -> String {
        self.create_agent_internal(
            model,
            prompt,
            context,
            output_goal,
            files,
            read_only,
            batch_id,
            None,
        )
        .await
    }

    pub async fn create_agent_with_config(
        &mut self,
        model: String,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        read_only: bool,
        batch_id: Option<String>,
        config: AgentConfig,
    ) -> String {
        self.create_agent_internal(
            model,
            prompt,
            context,
            output_goal,
            files,
            read_only,
            batch_id,
            Some(config),
        )
        .await
    }

    async fn create_agent_internal(
        &mut self,
        model: String,
        prompt: String,
        context: Option<String>,
        output_goal: Option<String>,
        files: Vec<String>,
        read_only: bool,
        batch_id: Option<String>,
        config: Option<AgentConfig>,
    ) -> String {
        let agent_id = Uuid::new_v4().to_string();

        let agent = Agent {
            id: agent_id.clone(),
            batch_id,
            model,
            prompt,
            context,
            output_goal,
            files,
            read_only,
            status: AgentStatus::Pending,
            result: None,
            error: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            progress: Vec::new(),
            worktree_path: None,
            branch_name: None,
            config: config.clone(),
        };

        self.agents.insert(agent_id.clone(), agent.clone());

        // Try to start now (obeys concurrency caps), or queue if over cap
        let _ = self.try_start_agent_now(&agent_id).await;

        // Send initial status update (pending or running)
        self.send_agent_status_update();

        agent_id
    }

    pub fn get_agent(&self, agent_id: &str) -> Option<Agent> {
        self.agents.get(agent_id).cloned()
    }

    pub fn get_all_agents(&self) -> impl Iterator<Item = &Agent> {
        self.agents.values()
    }

    pub fn list_agents(
        &self,
        status_filter: Option<AgentStatus>,
        batch_id: Option<String>,
        recent_only: bool,
    ) -> Vec<Agent> {
        let cutoff = if recent_only {
            Some(Utc::now() - Duration::hours(2))
        } else {
            None
        };

        self.agents
            .values()
            .filter(|agent| {
                if let Some(ref filter) = status_filter {
                    if agent.status != *filter {
                        return false;
                    }
                }
                if let Some(ref batch) = batch_id {
                    if agent.batch_id.as_ref() != Some(batch) {
                        return false;
                    }
                }
                if let Some(cutoff) = cutoff {
                    if agent.created_at < cutoff {
                        return false;
                    }
                }
                true
            })
            .cloned()
            .collect()
    }

    pub async fn cancel_agent(&mut self, agent_id: &str) -> bool {
        if let Some(handle) = self.handles.remove(agent_id) {
            handle.abort();
            if let Some(agent) = self.agents.get_mut(agent_id) {
                agent.status = AgentStatus::Cancelled;
                agent.completed_at = Some(Utc::now());
            }
            true
        } else {
            false
        }
    }

    pub async fn cancel_batch(&mut self, batch_id: &str) -> usize {
        let agent_ids: Vec<String> = self
            .agents
            .values()
            .filter(|agent| agent.batch_id.as_ref() == Some(&batch_id.to_string()))
            .map(|agent| agent.id.clone())
            .collect();

        let mut count = 0;
        for agent_id in agent_ids {
            if self.cancel_agent(&agent_id).await {
                count += 1;
            }
        }
        count
    }

    pub async fn update_agent_status(&mut self, agent_id: &str, status: AgentStatus) {
        let mut model_to_consider: Option<String> = None;
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.status = status;
            if agent.status == AgentStatus::Running && agent.started_at.is_none() {
                agent.started_at = Some(Utc::now());
            }
            if matches!(
                agent.status,
                AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
            ) {
                agent.completed_at = Some(Utc::now());
                model_to_consider = Some(agent.model.clone());
            }
        }
        // Send status update event outside the mutable borrow
        self.send_agent_status_update();
        // Capacity freed; queued agents remain pending until a future caller starts them.
        if let Some(m) = model_to_consider { self.maybe_start_next_for_model(&m); }
    }

    pub async fn update_agent_result(&mut self, agent_id: &str, result: Result<String, String>) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            match result {
                Ok(output) => {
                    agent.result = Some(output);
                    agent.status = AgentStatus::Completed;
                }
                Err(error) => {
                    agent.error = Some(error);
                    agent.status = AgentStatus::Failed;
                }
            }
            agent.completed_at = Some(Utc::now());
            // Send status update event
            self.send_agent_status_update();
        }
    }

    pub async fn add_progress(&mut self, agent_id: &str, message: String) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent
                .progress
                .push(format!("{}: {}", Utc::now().format("%H:%M:%S"), message));
            // Send updated agent status with the latest progress
            self.send_agent_status_update();
        }
    }

    pub async fn update_worktree_info(
        &mut self,
        agent_id: &str,
        worktree_path: String,
        branch_name: String,
    ) {
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.worktree_path = Some(worktree_path);
            agent.branch_name = Some(branch_name);
        }
    }
}

async fn get_git_root() -> Result<PathBuf, String> {
    let output = Command::new("git")
        .args(&["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| format!("Git not installed or not in a git repository: {}", e))?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    } else {
        Err("Not in a git repository".to_string())
    }
}

fn generate_branch_id(model: &str, agent: &str) -> String {
    // Extract first few meaningful words from agent for the branch name
    let words: Vec<&str> = agent
        .split_whitespace()
        .filter(|w| w.len() > 2 && !["the", "and", "for", "with", "from", "into"].contains(w))
        .take(3)
        .collect();

    let agent_suffix = if words.is_empty() {
        Uuid::new_v4()
            .to_string()
            .split('-')
            .next()
            .unwrap_or("agent")
            .to_string()
    } else {
        words.join("-").to_lowercase()
    };

    format!("code-{}-{}", model, agent_suffix)
}

async fn setup_worktree(git_root: &Path, branch_id: &str) -> Result<PathBuf, String> {
    // Create .code/branches directory if it doesn't exist
    let code_dir = git_root.join(".code").join("branches");
    tokio::fs::create_dir_all(&code_dir)
        .await
        .map_err(|e| format!("Failed to create .code/branches directory: {}", e))?;

    // Path for this model's worktree
    let worktree_path = code_dir.join(branch_id);

    // Remove existing worktree if it exists (cleanup from previous runs)
    if worktree_path.exists() {
        Command::new("git")
            .args(&[
                "worktree",
                "remove",
                worktree_path.to_str().unwrap(),
                "--force",
            ])
            .output()
            .await
            .ok(); // Ignore errors, it might not be a worktree
    }

    // Create new worktree
    let output = Command::new("git")
        .current_dir(git_root)
        .args(&[
            "worktree",
            "add",
            "-b",
            branch_id,
            worktree_path.to_str().unwrap(),
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to create git worktree: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to create worktree: {}", stderr));
    }

    Ok(worktree_path)
}

async fn execute_agent(agent_id: String, config: Option<AgentConfig>) {
    let mut manager = AGENT_MANAGER.write().await;

    // Get agent details
    let agent = match manager.get_agent(&agent_id) {
        Some(t) => t,
        None => return,
    };

    // Update status to running
    manager
        .update_agent_status(&agent_id, AgentStatus::Running)
        .await;
    manager
        .add_progress(
            &agent_id,
            format!("Starting agent with model: {}", agent.model),
        )
        .await;

    let model = agent.model.clone();
    let prompt = agent.prompt.clone();
    let read_only = agent.read_only;
    let context = agent.context.clone();
    let output_goal = agent.output_goal.clone();
    let files = agent.files.clone();

    drop(manager); // Release the lock before executing

    // Build the full prompt with context
    let mut full_prompt = prompt.clone();
    if let Some(context) = &context {
        full_prompt = format!("Context: {}\n\nAgent: {}", context, full_prompt);
    }
    if let Some(output_goal) = &output_goal {
        full_prompt = format!("{}\n\nDesired output: {}", full_prompt, output_goal);
    }
    if !files.is_empty() {
        full_prompt = format!("{}\n\nFiles to consider: {}", full_prompt, files.join(", "));
    }

    // Setup working directory and execute
    let result = if !read_only {
        // Check git and setup worktree for non-read-only mode
        match get_git_root().await {
            Ok(git_root) => {
                let branch_id = generate_branch_id(&model, &prompt);

                let mut manager = AGENT_MANAGER.write().await;
                manager
                    .add_progress(&agent_id, format!("Creating git worktree: {}", branch_id))
                    .await;
                drop(manager);

                match setup_worktree(&git_root, &branch_id).await {
                    Ok(worktree_path) => {
                        let mut manager = AGENT_MANAGER.write().await;
                        manager
                            .add_progress(
                                &agent_id,
                                format!("Executing in worktree: {}", worktree_path.display()),
                            )
                            .await;
                        manager
                            .update_worktree_info(
                                &agent_id,
                                worktree_path.display().to_string(),
                                branch_id.clone(),
                            )
                            .await;
                        drop(manager);

                        // Execute with full permissions in the worktree
                        execute_model_with_permissions(
                            &model,
                            &full_prompt,
                            false,
                            Some(worktree_path),
                            config.clone(),
                        )
                        .await
                    }
                    Err(e) => Err(format!("Failed to setup worktree: {}", e)),
                }
            }
            Err(e) => Err(format!("Git is required for non-read-only agents: {}", e)),
        }
    } else {
        // Execute in read-only mode
        full_prompt = format!(
            "{}\n\n[Running in read-only mode - no modifications allowed]",
            full_prompt
        );
        execute_model_with_permissions(&model, &full_prompt, true, None, config).await
    };

    // Update result
    let mut manager = AGENT_MANAGER.write().await;
    manager.update_agent_result(&agent_id, result).await;
}

async fn execute_model_with_permissions(
    model: &str,
    prompt: &str,
    read_only: bool,
    working_dir: Option<PathBuf>,
    config: Option<AgentConfig>,
) -> Result<String, String> {
    // Use config command if provided, otherwise use model name
    let command = if let Some(ref cfg) = config {
        cfg.command.clone()
    } else {
        model.to_lowercase()
    };

    let mut cmd = Command::new(command.clone());

    // Set working directory if provided
    if let Some(dir) = working_dir {
        cmd.current_dir(dir);
    }

    // Add environment variables from config if provided
    if let Some(ref cfg) = config {
        if let Some(ref env) = cfg.env {
            for (key, value) in env {
                cmd.env(key, value);
            }
        }

        // Add any configured args first
        for arg in &cfg.args {
            cmd.arg(arg);
        }
    }

    // Build command based on model and permissions
    // Use command instead of model for matching if config provided
    let model_lower = model.to_lowercase();
    let model_name = if config.is_some() {
        command.as_str()
    } else {
        model_lower.as_str()
    };

    match model_name {
        "claude" => {
            if read_only {
                cmd.args(&[
                    "--allowedTools",
                    "Bash(ls:*), Bash(cat:*), Bash(grep:*), Bash(git status:*), Bash(git log:*), Bash(find:*), Read, Grep, Glob, LS, WebFetch, TodoRead, TodoWrite, WebSearch",
                    "-p",
                    prompt
                ]);
            } else {
                cmd.args(&["--dangerously-skip-permissions", "-p", prompt]);
            }
        }
        "gemini" => {
            if read_only {
                cmd.args(&["-p", prompt]);
            } else {
                cmd.args(&["-y", "-p", prompt]);
            }
        }
        "codex" => {
            // Respect preconfigured sandbox/approval flags in config args when present.
            let mut has_s = false;
            let mut has_a = false;
            if let Some(ref cfg) = config {
                let mut iter = cfg.args.iter();
                while let Some(a) = iter.next() {
                    if a == "-s" { has_s = true; /* skip value inspection */ }
                    if a == "-a" { has_a = true; }
                }
            }
            if read_only {
                if !has_s { cmd.args(&["-s", "read-only"]); }
                if !has_a { cmd.args(&["-a", "never"]); }
                cmd.args(&["exec", prompt]);
            } else {
                if !has_s { cmd.args(&["-s", "workspace-write"]); }
                if !has_a { cmd.args(&["-a", "on-request"]); }
                cmd.args(&["exec", prompt]);
            }
        }
        _ => {
            return Err(format!("Unknown model: {}", model));
        }
    }

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to execute {}: {}", model, e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("Command failed: {}", stderr))
    }
}

// Tool creation functions
pub fn create_run_agent_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "task".to_string(),
        JsonSchema::String {
            description: Some("The task prompt - what to perform (required)".to_string()),
        },
    );

    properties.insert(
        "model".to_string(),
        JsonSchema::String {
            description: Some(
                "Model: 'claude', 'gemini', or 'codex' (or array of models for batch execution)"
                    .to_string(),
            ),
        },
    );

    properties.insert(
        "context".to_string(),
        JsonSchema::String {
            description: Some("Optional: Background context for the agent".to_string()),
        },
    );

    properties.insert(
        "output".to_string(),
        JsonSchema::String {
            description: Some("Optional: The desired output/success state".to_string()),
        },
    );

    properties.insert(
        "files".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "Optional: Array of file paths to include in the agent context".to_string(),
            ),
        },
    );

    properties.insert(
        "read_only".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Optional: When true, agent runs in read-only mode (default: false)".to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_run".to_string(),
        description: "Start a complex AI task asynchronously. Returns a agent ID immediately to check status and retrieve results.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["task".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub fn create_check_agent_status_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("The agent ID returned from run_agent".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_check".to_string(),
        description: "Check the status of a running agent. Returns current status, progress, and partial results if available.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["agent_id".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub fn create_get_agent_result_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some("The agent ID returned from run_agent".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_result".to_string(),
        description: "Get the final result of a completed agent.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["agent_id".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub fn create_cancel_agent_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some(
                "The agent ID to cancel (required if batch_id not provided)".to_string(),
            ),
        },
    );

    properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some(
                "Cancel all agents with this batch ID (required if agent_id not provided)"
                    .to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_cancel".to_string(),
        description: "Cancel a pending or running agent, or all agents in a batch.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

pub fn create_wait_for_agent_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "agent_id".to_string(),
        JsonSchema::String {
            description: Some(
                "Wait for this specific agent to complete (required if batch_id not provided)"
                    .to_string(),
            ),
        },
    );

    properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some(
                "Wait for any agent in this batch to complete (required if agent_id not provided)"
                    .to_string(),
            ),
        },
    );

    properties.insert(
        "timeout_seconds".to_string(),
        JsonSchema::Number {
            description: Some(
                "Maximum seconds to wait before timing out (default: 300, max: 600)".to_string(),
            ),
        },
    );

    properties.insert(
        "return_all".to_string(),
        JsonSchema::Boolean {
            description: Some("For batch_id: return all completed agents instead of just the first one (default: false)".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_wait".to_string(),
        description: "Wait for a agent or any agent in a batch to complete, fail, or be cancelled."
            .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

pub fn create_list_agents_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "status_filter".to_string(),
        JsonSchema::String {
            description: Some("Optional: Filter agents by status (pending, running, completed, failed, cancelled)".to_string()),
        },
    );

    properties.insert(
        "batch_id".to_string(),
        JsonSchema::String {
            description: Some("Optional: Filter agents by batch ID".to_string()),
        },
    );

    properties.insert(
        "recent_only".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "Optional: Only show agents from the last 2 hours (default: false)".to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "agent_list".to_string(),
        description: "List all agents with their current status.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

// Parameter structs for handlers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAgentParams {
    pub task: String,
    pub model: Option<serde_json::Value>, // Can be string or array
    pub context: Option<String>,
    pub output: Option<String>,
    pub files: Option<Vec<String>>,
    pub read_only: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckAgentStatusParams {
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetAgentResultParams {
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAgentParams {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitForAgentParams {
    pub agent_id: Option<String>,
    pub batch_id: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub return_all: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAgentsParams {
    pub status_filter: Option<String>,
    pub batch_id: Option<String>,
    pub recent_only: Option<bool>,
}
