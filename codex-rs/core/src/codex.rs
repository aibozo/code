// Poisoned mutex should fail the program
#![allow(clippy::unwrap_used)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use async_channel::Receiver;
use async_channel::Sender;
use base64::Engine;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::MaybeApplyPatchVerified;
use codex_apply_patch::maybe_parse_apply_patch_verified;
use codex_login::CodexAuth;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use futures::prelude::*;
use mcp_types::CallToolResult;
use serde::Serialize;
use serde_json;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;
use uuid::Uuid;
use codex_memory::store::jsonl::JsonlVectorStore;
use codex_memory::store::EmbeddedRecord as EmbRec;
use codex_memory::embedding::EmbeddingProvider;
use codex_file_search as file_search;
use crate::memory::openai_embeddings::{OpenAiEmbeddingClient, has_openai_api_key};
use crate::memory::code_index::ensure_code_index;
use crate::model_provider_info::built_in_model_providers;

/// Initial submission ID for session configuration
pub(crate) const INITIAL_SUBMIT_ID: &str = "";

/// Gather ephemeral, per-turn context that should not be persisted to history.
/// Combines environment info and (when enabled) a live browser snapshot and status.
struct EphemeralJar {
    items: Vec<ResponseItem>,
}

impl EphemeralJar {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn into_items(self) -> Vec<ResponseItem> {
        self.items
    }
}

fn get_git_branch(cwd: &std::path::Path) -> Option<String> {
    let head_path = cwd.join(".git/HEAD");
    if let Ok(contents) = std::fs::read_to_string(&head_path) {
        if let Some(rest) = contents.trim().strip_prefix("ref: ") {
            if let Some(branch) = rest.trim().rsplit('/').next() {
                return Some(branch.to_string());
            }
        }
    }
    None
}

async fn build_turn_status_items(sess: &Session) -> Vec<ResponseItem> {
    let mut jar = EphemeralJar::new();

    // Collect environment context
    let cwd = sess.cwd.to_string_lossy().to_string();
    let branch = get_git_branch(&sess.cwd).unwrap_or_else(|| "unknown".to_string());
    let reasoning_effort = sess.client.get_reasoning_effort();

    // Build current system status
    let mut current_status = format!(
        r#"== System Status ==
[automatic message added by system]

cwd: {}
branch: {}
reasoning: {:?}"#,
        cwd, branch, reasoning_effort
    );

    // Prepare browser context + optional screenshot
    let mut screenshot_content: Option<ContentItem> = None;
    let mut include_screenshot = false;

    if let Some(browser_manager) = codex_browser::global::get_browser_manager().await {
        if browser_manager.is_enabled().await {
            // Get current URL and browser info
            let url = browser_manager
                .get_current_url()
                .await
                .unwrap_or_else(|| "unknown".to_string());

            // Try to get a tab title if available
            let title = match browser_manager.get_or_create_page().await {
                Ok(page) => page.get_title().await,
                Err(_) => None,
            };

            // Get browser type description
            let browser_type = browser_manager.get_browser_type().await;

            // Get viewport dimensions
            let (viewport_width, viewport_height) = browser_manager.get_viewport_size().await;
            let viewport_info = format!(" | Viewport: {}x{}", viewport_width, viewport_height);

            // Get cursor position
            let cursor_info = match browser_manager.get_cursor_position().await {
                Ok((x, y)) => format!(
                    " | Mouse position: ({:.0}, {:.0}) [shown as a blue cursor in the screenshot]",
                    x, y
                ),
                Err(_) => String::new(),
            };

            // Try to capture screenshot and compare with last one
            let screenshot_status = match capture_browser_screenshot(sess).await {
                Ok((screenshot_path, _url)) => {
                    // Check if screenshot has changed using image hashing
                    let mut last_screenshot_info = sess.last_screenshot_info.lock().unwrap();

                    // Compute hash for current screenshot
                    let current_hash =
                        crate::image_comparison::compute_image_hash(&screenshot_path).ok();

                    let should_include_screenshot = if let (
                        Some((_last_path, last_phash, last_dhash)),
                        Some((cur_phash, cur_dhash)),
                    ) =
                        (last_screenshot_info.as_ref(), current_hash.as_ref())
                    {
                        // Compare hashes to see if screenshots are similar
                        let similar = crate::image_comparison::are_hashes_similar(
                            last_phash, last_dhash, cur_phash, cur_dhash,
                        );

                        if !similar {
                            // Screenshot has changed, include it
                            *last_screenshot_info = Some((
                                screenshot_path.clone(),
                                cur_phash.clone(),
                                cur_dhash.clone(),
                            ));
                            true
                        } else {
                            // Screenshot unchanged
                            false
                        }
                    } else {
                        // No previous screenshot or hash computation failed, include it
                        if let Some((phash, dhash)) = current_hash {
                            *last_screenshot_info = Some((screenshot_path.clone(), phash, dhash));
                        }
                        true
                    };

                    if should_include_screenshot {
                        if let Ok(bytes) = std::fs::read(&screenshot_path) {
                            let mime = mime_guess::from_path(&screenshot_path)
                                .first()
                                .map(|m| m.to_string())
                                .unwrap_or_else(|| "image/png".to_string());
                            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                            screenshot_content = Some(ContentItem::InputImage {
                                image_url: format!("data:{mime};base64,{encoded}"),
                                detail: Some("high".to_string()),
                            });
                            include_screenshot = true;
                            ""
                        } else {
                            " [Screenshot file read failed]"
                        }
                    } else {
                        " [Screenshot unchanged]"
                    }
                }
                Err(err_msg) => {
                    // Include error message so LLM knows screenshot failed
                    format!(" [Screenshot unavailable: {}]", err_msg).leak()
                }
            };

            let status_line = if let Some(t) = title {
                format!(
                    "Browser url: {} — {} ({}){}{}{}. You can interact with it using browser_* tools.",
                    url, t, browser_type, viewport_info, cursor_info, screenshot_status
                )
            } else {
                format!(
                    "Browser url: {} ({}){}{}{}. You can interact with it using browser_* tools.",
                    url, browser_type, viewport_info, cursor_info, screenshot_status
                )
            };
            current_status.push_str("\n");
            current_status.push_str(&status_line);
        }
    }

    // Check if system status has changed
    let mut last_status = sess.last_system_status.lock().unwrap();
    let status_changed = last_status.as_ref() != Some(&current_status);

    if status_changed {
        // Update last status
        *last_status = Some(current_status.clone());
    }

    // Only include items if something has changed or is new
    let mut content: Vec<ContentItem> = Vec::new();

    // Always prepend an ephemeral marker before any per‑turn status content so it
    // is not persisted into future turn inputs. When the status text changed,
    // include the full status text as ephemeral content; otherwise, still emit a
    // small ephemeral marker if we are attaching a screenshot so the image can
    // be filtered from history on subsequent turns.
    if status_changed {
        content.push(ContentItem::InputText {
            text: format!("[EPHEMERAL:turn_status]\n{}", current_status),
        });
    } else if include_screenshot {
        content.push(ContentItem::InputText {
            text: "[EPHEMERAL:turn_status]".to_string(),
        });
    }

    if include_screenshot {
        if let Some(image) = screenshot_content {
            content.push(image);
        }
    }

    if !content.is_empty() {
        jar.items.push(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content,
        });
    }

    jar.into_items()
}
use crate::agent_tool::AGENT_MANAGER;
use crate::agent_tool::AgentStatus;
use crate::agent_tool::CancelAgentParams;
use crate::agent_tool::CheckAgentStatusParams;
use crate::agent_tool::GetAgentResultParams;
use crate::agent_tool::ListAgentsParams;
use crate::agent_tool::RunAgentParams;
use crate::agent_tool::WaitForAgentParams;
use crate::apply_patch::ApplyPatchExec;
use crate::apply_patch::CODEX_APPLY_PATCH_ARG1;
use crate::apply_patch::InternalApplyPatchInvocation;
use crate::apply_patch::convert_apply_patch_to_protocol;
use crate::apply_patch::get_writable_roots;
use crate::apply_patch::{self};
use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::environment_context::EnvironmentContext;
use crate::config::Config;
use crate::config_types::ShellEnvironmentPolicy;
use crate::conversation_history::ConversationHistory;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::error::SandboxErr;
use crate::error::get_error_message_ui;
use crate::exec::ExecParams;
use crate::exec::ExecToolCallOutput;
use crate::exec::SandboxType;
use crate::exec::StdoutStream;
use crate::exec::StreamOutput;
use crate::exec::process_exec_tool_call;
use crate::exec_env::create_env;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::mcp_tool_call::handle_mcp_tool_call;
use crate::models::ContentItem;
use crate::models::FunctionCallOutputPayload;
use crate::models::LocalShellAction;
use crate::models::ReasoningItemContent;
use crate::models::ReasoningItemReasoningSummary;
use crate::models::ResponseInputItem;
use crate::models::ResponseItem;
use crate::models::ShellToolCallParams;
use crate::openai_tools::ToolsConfig;
use crate::openai_tools::get_openai_tools;
use crate::parse_command::parse_command;
use crate::plan_tool::handle_update_plan;
use crate::project_doc::get_user_instructions;
use crate::memory::summarizer::{Summarizer, CompactSummarizer};
use crate::conversation_history::volley::{segment_into_volleys, filter_compaction_candidates};
use crate::protocol::AgentMessageDeltaEvent;
use crate::protocol::AgentMessageEvent;
use crate::protocol::AgentReasoningDeltaEvent;
use crate::protocol::AgentReasoningEvent;
use crate::protocol::AgentReasoningRawContentDeltaEvent;
use crate::protocol::AgentReasoningRawContentEvent;
use crate::protocol::AgentReasoningSectionBreakEvent;
use crate::protocol::AgentStatusUpdateEvent;
use crate::protocol::ApplyPatchApprovalRequestEvent;
use crate::protocol::AskForApproval;
use crate::protocol::BackgroundEventEvent;
use crate::protocol::BrowserScreenshotUpdateEvent;
use crate::protocol::ErrorEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecApprovalRequestEvent;
use crate::protocol::ExecCommandBeginEvent;
use crate::protocol::ExecCommandEndEvent;
use crate::protocol::FileChange;
use crate::protocol::InputItem;
use crate::protocol::Op;
use crate::protocol::PatchApplyBeginEvent;
use crate::protocol::PatchApplyEndEvent;
use crate::protocol::ReviewDecision;
use crate::protocol::SandboxPolicy;
use crate::protocol::SessionConfiguredEvent;
use crate::protocol::Submission;
use crate::protocol::TaskCompleteEvent;
use crate::protocol::TurnDiffEvent;
use crate::rollout::RolloutRecorder;
use crate::safety::SafetyCheck;
use crate::safety::assess_command_safety;
use crate::safety::assess_safety_for_untrusted_command;
use crate::shell;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::user_notification::UserNotification;
use crate::util::backoff;
use serde_json::Value;

/// The high-level interface to the Codex system.
/// It operates as a queue pair where you send submissions and receive events.
pub struct Codex {
    next_id: AtomicU64,
    tx_sub: Sender<Submission>,
    rx_event: Receiver<Event>,
}

/// Wrapper returned by [`Codex::spawn`] containing the spawned [`Codex`],
/// the submission id for the initial `ConfigureSession` request and the
/// unique session id.
pub struct CodexSpawnOk {
    pub codex: Codex,
    pub init_id: String,
    pub session_id: Uuid,
}

impl Codex {
    /// Spawn a new [`Codex`] and initialize the session.
    pub async fn spawn(config: Config, auth: Option<CodexAuth>) -> CodexResult<CodexSpawnOk> {
        // experimental resume path (undocumented)
        let resume_path = config.experimental_resume.clone();
        info!("resume_path: {resume_path:?}");
        let (tx_sub, rx_sub) = async_channel::bounded(64);
        let (tx_event, rx_event) = async_channel::unbounded();

        let user_instructions = get_user_instructions(&config).await;

        let configure_session = Op::ConfigureSession {
            provider: config.model_provider.clone(),
            model: config.model.clone(),
            model_reasoning_effort: config.model_reasoning_effort,
            model_reasoning_summary: config.model_reasoning_summary,
            model_text_verbosity: config.model_text_verbosity,
            user_instructions,
            base_instructions: config.base_instructions.clone(),
            approval_policy: config.approval_policy,
            sandbox_policy: config.sandbox_policy.clone(),
            disable_response_storage: config.disable_response_storage,
            notify: config.notify.clone(),
            cwd: config.cwd.clone(),
            resume_path: resume_path.clone(),
        };

        let config = Arc::new(config);

        // Generate a unique ID for the lifetime of this Codex session.
        let session_id = Uuid::new_v4();

        // This task will run until Op::Shutdown is received.
        tokio::spawn(submission_loop(session_id, config, auth, rx_sub, tx_event));
        let codex = Codex {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event,
        };
        let init_id = codex.submit(configure_session).await?;

        Ok(CodexSpawnOk {
            codex,
            init_id,
            session_id,
        })
    }

    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .to_string();
        let sub = Submission { id: id.clone(), op };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so Codex is responsible for generating
    /// unique IDs for each submission.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(())
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        let event = self
            .rx_event
            .recv()
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(event)
    }
}

/// Mutable state of the agent
#[derive(Default)]
struct State {
    approved_commands: HashSet<Vec<String>>,
    current_agent: Option<AgentAgent>,
    pending_approvals: HashMap<String, oneshot::Sender<ReviewDecision>>,
    pending_input: Vec<ResponseInputItem>,
    history: ConversationHistory,
    /// Last completed turn's provider-reported token usage (for baselining post-prune updates)
    last_completed_token_usage: Option<crate::protocol::TokenUsage>,
    /// Tracks which completed agents (by id) have already been returned to the
    /// model for a given batch when using `agent_wait` without `return_all`.
    /// This enables sequential waiting behavior across multiple calls.
    seen_completed_agents_by_batch: HashMap<String, HashSet<String>>,
}

/// Context for an initialized model agent
///
/// A session has at most 1 running agent at a time, and can be interrupted by user input.
pub(crate) struct Session {
    client: ModelClient,
    tx_event: Sender<Event>,

    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    cwd: PathBuf,
    base_instructions: Option<String>,
    user_instructions: Option<String>,
    approval_policy: AskForApproval,
    sandbox_policy: SandboxPolicy,
    shell_environment_policy: ShellEnvironmentPolicy,
    writable_roots: Vec<PathBuf>,
    disable_response_storage: bool,
    tools_config: ToolsConfig,

    /// Manager for external MCP servers/tools.
    mcp_connection_manager: McpConnectionManager,

    /// Configuration for available agent models
    agents: Vec<crate::config_types::AgentConfig>,

    /// External notifier command (will be passed as args to exec()). When
    /// `None` this feature is disabled.
    notify: Option<Vec<String>>,

    /// Optional rollout recorder for persisting the conversation transcript so
    /// sessions can be replayed or inspected later.
    rollout: Mutex<Option<RolloutRecorder>>,
    state: Mutex<State>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    user_shell: shell::Shell,
    show_raw_agent_reasoning: bool,
    /// Pending browser screenshots to include in the next model request
    #[allow(dead_code)]
    pending_browser_screenshots: Mutex<Vec<PathBuf>>,
    /// Track the last system status to detect changes
    last_system_status: Mutex<Option<String>>,
    /// Track the last screenshot path and hash to detect changes
    last_screenshot_info: Mutex<Option<(PathBuf, Vec<u8>, Vec<u8>)>>, // (path, phash, dhash)
}

impl Session {
    pub(crate) fn get_writable_roots(&self) -> &[PathBuf] {
        &self.writable_roots
    }

    pub(crate) fn get_approval_policy(&self) -> AskForApproval {
        self.approval_policy
    }

    pub(crate) fn get_cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn get_sandbox_policy(&self) -> &SandboxPolicy {
        &self.sandbox_policy
    }

    fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }
}

impl Session {
    pub fn set_agent(&self, agent: AgentAgent) {
        let mut state = self.state.lock().unwrap();
        if let Some(current_agent) = state.current_agent.take() {
            current_agent.abort(TurnAbortReason::Replaced);
        }
        state.current_agent = Some(agent);
    }

    pub fn remove_agent(&self, sub_id: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(agent) = &state.current_agent {
            if agent.sub_id == sub_id {
                state.current_agent.take();
            }
        }
    }

    /// Sends the given event to the client and swallows the send event, if
    /// any, logging it as an error.
    pub(crate) async fn send_event(&self, event: Event) {
        if let Err(e) = self.tx_event.send(event).await {
            error!("failed to send tool call event: {e}");
        }
    }

    pub async fn request_command_approval(
        &self,
        sub_id: String,
        call_id: String,
        command: Vec<String>,
        cwd: PathBuf,
        reason: Option<String>,
    ) -> oneshot::Receiver<ReviewDecision> {
        let (tx_approve, rx_approve) = oneshot::channel();
        let event = Event {
            id: sub_id.clone(),
            msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                call_id,
                command,
                cwd,
                reason,
            }),
        };
        let _ = self.tx_event.send(event).await;
        {
            let mut state = self.state.lock().unwrap();
            state.pending_approvals.insert(sub_id, tx_approve);
        }
        rx_approve
    }

    pub async fn request_patch_approval(
        &self,
        sub_id: String,
        call_id: String,
        action: &ApplyPatchAction,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> oneshot::Receiver<ReviewDecision> {
        let (tx_approve, rx_approve) = oneshot::channel();
        let event = Event {
            id: sub_id.clone(),
            msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                call_id,
                changes: convert_apply_patch_to_protocol(action),
                reason,
                grant_root,
            }),
        };
        let _ = self.tx_event.send(event).await;
        {
            let mut state = self.state.lock().unwrap();
            state.pending_approvals.insert(sub_id, tx_approve);
        }
        rx_approve
    }

    pub fn notify_approval(&self, sub_id: &str, decision: ReviewDecision) {
        let mut state = self.state.lock().unwrap();
        if let Some(tx_approve) = state.pending_approvals.remove(sub_id) {
            tx_approve.send(decision).ok();
        }
    }

    pub fn add_approved_command(&self, cmd: Vec<String>) {
        let mut state = self.state.lock().unwrap();
        state.approved_commands.insert(cmd);
    }

    /// Records items to both the rollout and the chat completions/ZDR
    /// transcript, if enabled.
    async fn record_conversation_items(&self, items: &[ResponseItem]) {
        debug!("Recording items for conversation: {items:?}");
        self.record_state_snapshot(items).await;

        self.state.lock().unwrap().history.record_items(items);
    }

    /// Clean up old screenshots and system status messages from conversation history
    /// This is called when a new user message arrives to keep history manageable
    async fn cleanup_old_status_items(&self) {
        let mut state = self.state.lock().unwrap();

        // Get current history items
        let current_items = state.history.contents();

        // Track various message types and their positions
        let mut real_user_messages = Vec::new(); // Non-status user messages
        let mut status_messages = Vec::new(); // Messages with screenshots or status

        for (idx, item) in current_items.iter().enumerate() {
            match item {
                ResponseItem::Message { role, content, .. } if role == "user" => {
                    // Check message content
                    let has_status = content.iter().any(|c| {
                        if let ContentItem::InputText { text } = c {
                            text.contains("== System Status ==")
                                || text.contains("Current working directory:")
                                || text.contains("Git branch:")
                        } else {
                            false
                        }
                    });

                    let has_screenshot = content
                        .iter()
                        .any(|c| matches!(c, ContentItem::InputImage { .. }));

                    let has_real_text = content.iter().any(|c| {
                        if let ContentItem::InputText { text } = c {
                            // Real user text doesn't contain system status markers
                            !text.contains("== System Status ==")
                                && !text.contains("Current working directory:")
                                && !text.contains("Git branch:")
                                && !text.trim().is_empty()
                        } else {
                            false
                        }
                    });

                    if has_real_text && !has_status && !has_screenshot {
                        // This is a real user message
                        real_user_messages.push(idx);
                    } else if has_status || has_screenshot {
                        // This is a status/screenshot message
                        status_messages.push(idx);
                    }
                }
                _ => {}
            }
        }

        // Find screenshots to keep: last 2 that directly follow real user commands
        let mut screenshots_to_keep = std::collections::HashSet::new();

        // Work backwards through real user messages
        for &user_idx in real_user_messages.iter().rev().take(2) {
            // Find the first status message after this user message
            for &status_idx in status_messages.iter() {
                if status_idx > user_idx {
                    // Check if this status message contains a screenshot
                    if let Some(ResponseItem::Message { content, .. }) =
                        current_items.get(status_idx)
                    {
                        let has_screenshot = content
                            .iter()
                            .any(|c| matches!(c, ContentItem::InputImage { .. }));
                        if has_screenshot {
                            screenshots_to_keep.insert(status_idx);
                            break; // Only keep one screenshot per user message
                        }
                    }
                }
            }
        }

        // Build the filtered history
        let mut items_to_keep = Vec::new();
        let mut removed_screenshots = 0;
        let mut removed_status = 0;

        for (idx, item) in current_items.iter().enumerate() {
            let should_keep = if status_messages.contains(&idx) {
                // This is a status/screenshot message
                if screenshots_to_keep.contains(&idx) {
                    true // Keep this screenshot
                } else {
                    // Count what we're removing
                    if let ResponseItem::Message { content, .. } = item {
                        let has_screenshot = content
                            .iter()
                            .any(|c| matches!(c, ContentItem::InputImage { .. }));
                        if has_screenshot {
                            removed_screenshots += 1;
                        } else {
                            removed_status += 1;
                        }
                    }
                    false // Remove this status/screenshot
                }
            } else {
                true // Keep all non-status messages (real user messages, assistant messages, etc.)
            };

            if should_keep {
                items_to_keep.push(item.clone());
            }
        }

        // Replace the history with cleaned items
        state.history = ConversationHistory::new();
        state.history.record_items(&items_to_keep);

        if removed_screenshots > 0 || removed_status > 0 {
            info!(
                "Cleaned up history: removed {} old screenshots and {} status messages, kept {} recent screenshots",
                removed_screenshots,
                removed_status,
                screenshots_to_keep.len()
            );
        }
    }

    async fn record_state_snapshot(&self, items: &[ResponseItem]) {
        let snapshot = { crate::rollout::SessionStateSnapshot {} };

        let recorder = {
            let guard = self.rollout.lock().unwrap();
            guard.as_ref().cloned()
        };

        if let Some(rec) = recorder {
            if let Err(e) = rec.record_state(snapshot).await {
                error!("failed to record rollout state: {e:#}");
            }
            if let Err(e) = rec.record_items(items).await {
                error!("failed to record rollout items: {e:#}");
            }
        }
    }

    async fn on_exec_command_begin(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        exec_command_context: ExecCommandContext,
    ) {
        let ExecCommandContext {
            sub_id,
            call_id,
            command_for_display,
            cwd,
            apply_patch,
        } = exec_command_context;
        let msg = match apply_patch {
            Some(ApplyPatchCommandContext {
                user_explicitly_approved_this_action,
                changes,
            }) => {
                turn_diff_tracker.on_patch_begin(&changes);

                EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                    call_id,
                    auto_approved: !user_explicitly_approved_this_action,
                    changes,
                })
            }
            None => EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
                call_id,
                command: command_for_display.clone(),
                cwd,
                parsed_cmd: parse_command(&command_for_display),
            }),
        };
        let event = Event {
            id: sub_id.to_string(),
            msg,
        };
        let _ = self.tx_event.send(event).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn on_exec_command_end(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        sub_id: &str,
        call_id: &str,
        output: &ExecToolCallOutput,
        is_apply_patch: bool,
    ) {
        let ExecToolCallOutput {
            stdout,
            stderr,
            duration,
            exit_code,
        } = output;
        // Because stdout and stderr could each be up to 100 KiB, we send
        // truncated versions.
        const MAX_STREAM_OUTPUT: usize = 5 * 1024; // 5KiB
        let stdout = stdout.text.chars().take(MAX_STREAM_OUTPUT).collect();
        let stderr = stderr.text.chars().take(MAX_STREAM_OUTPUT).collect();

        let msg = if is_apply_patch {
            EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id: call_id.to_string(),
                stdout,
                stderr,
                success: *exit_code == 0,
            })
        } else {
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: call_id.to_string(),
                stdout,
                stderr,
                duration: *duration,
                exit_code: *exit_code,
            })
        };

        let event = Event {
            id: sub_id.to_string(),
            msg,
        };
        let _ = self.tx_event.send(event).await;

        // If this is an apply_patch, after we emit the end patch, emit a second event
        // with the full turn diff if there is one.
        if is_apply_patch {
            let unified_diff = turn_diff_tracker.get_unified_diff();
            if let Ok(Some(unified_diff)) = unified_diff {
                let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
                let event = Event {
                    id: sub_id.into(),
                    msg,
                };
                let _ = self.tx_event.send(event).await;
            }
        }
    }
    /// Runs the exec tool call and emits events for the begin and end of the
    /// command even on error.
    ///
    /// Returns the output of the exec tool call.
    async fn run_exec_with_events<'a>(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        begin_ctx: ExecCommandContext,
        exec_args: ExecInvokeArgs<'a>,
    ) -> crate::error::Result<ExecToolCallOutput> {
        let is_apply_patch = begin_ctx.apply_patch.is_some();
        let sub_id = begin_ctx.sub_id.clone();
        let call_id = begin_ctx.call_id.clone();

        self.on_exec_command_begin(turn_diff_tracker, begin_ctx.clone())
            .await;

        let result = process_exec_tool_call(
            exec_args.params,
            exec_args.sandbox_type,
            exec_args.sandbox_policy,
            exec_args.codex_linux_sandbox_exe,
            exec_args.stdout_stream,
        )
        .await;

        let output_stderr;
        let borrowed: &ExecToolCallOutput = match &result {
            Ok(output) => output,
            Err(e) => {
                output_stderr = ExecToolCallOutput {
                    exit_code: -1,
                    stdout: StreamOutput::new(String::new()),
                    stderr: StreamOutput::new(get_error_message_ui(e)),
                    duration: Duration::default(),
                };
                &output_stderr
            }
        };
        self.on_exec_command_end(
            turn_diff_tracker,
            &sub_id,
            &call_id,
            borrowed,
            is_apply_patch,
        )
        .await;

        result
    }

    /// Helper that emits a BackgroundEvent with the given message. This keeps
    /// the call‑sites terse so adding more diagnostics does not clutter the
    /// core agent logic.
    async fn notify_background_event(&self, sub_id: &str, message: impl Into<String>) {
        let event = Event {
            id: sub_id.to_string(),
            msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                message: message.into(),
            }),
        };
        let _ = self.tx_event.send(event).await;
    }

    /// Build the full turn input by concatenating the current conversation
    /// history with additional items for this turn.
    /// Browser screenshots are filtered out from history to keep them ephemeral.
    pub fn turn_input_with_history(&self, extra: Vec<ResponseItem>) -> Vec<ResponseItem> {
        let history = self.state.lock().unwrap().history.contents();

        // Debug: Count function call outputs in history
        let fc_output_count = history
            .iter()
            .filter(|item| matches!(item, ResponseItem::FunctionCallOutput { .. }))
            .count();
        if fc_output_count > 0 {
            debug!(
                "History contains {} FunctionCallOutput items",
                fc_output_count
            );
        }

        // Count images in extra for debugging (we can't distinguish ephemeral at this level anymore)
        let images_in_extra = extra
            .iter()
            .filter(|item| {
                if let ResponseItem::Message { content, .. } = item {
                    content
                        .iter()
                        .any(|c| matches!(c, crate::models::ContentItem::InputImage { .. }))
                } else {
                    false
                }
            })
            .count();

        if images_in_extra > 0 {
            tracing::info!(
                "Found {} images in current turn's extra items",
                images_in_extra
            );
        }

        // Filter out browser screenshots from historical messages
        // We identify them by the [EPHEMERAL:...] marker that precedes them
        let filtered_history: Vec<ResponseItem> = history
            .into_iter()
            .map(|item| {
                if let ResponseItem::Message { id, role, content } = item {
                    if role == "user" {
                        // Filter out ephemeral content from user messages
                        let mut filtered_content: Vec<crate::models::ContentItem> = Vec::new();
                        let mut skip_next_image = false;

                        for content_item in content {
                            match &content_item {
                                crate::models::ContentItem::InputText { text }
                                    if text.starts_with("[EPHEMERAL:") =>
                                {
                                    // This is an ephemeral marker, skip it and the next image
                                    skip_next_image = true;
                                    tracing::info!("Filtering out ephemeral marker: {}", text);
                                }
                                crate::models::ContentItem::InputImage { .. }
                                    if skip_next_image =>
                                {
                                    // Skip this image as it follows an ephemeral marker
                                    skip_next_image = false;
                                    tracing::info!("Filtering out ephemeral image from history");
                                }
                                _ => {
                                    // Keep everything else
                                    filtered_content.push(content_item);
                                }
                            }
                        }

                        ResponseItem::Message {
                            id,
                            role,
                            content: filtered_content,
                        }
                    } else {
                        // Keep assistant messages unchanged
                        ResponseItem::Message { id, role, content }
                    }
                } else {
                    item
                }
            })
            .collect();

        // Concatenate filtered history with current turn's extras (which includes current ephemeral images)
        let result = [filtered_history, extra].concat();

        // Count total images in result for debugging
        let total_images = result
            .iter()
            .filter(|item| {
                if let ResponseItem::Message { content, .. } = item {
                    content
                        .iter()
                        .any(|c| matches!(c, crate::models::ContentItem::InputImage { .. }))
                } else {
                    false
                }
            })
            .count();

        if total_images > 0 {
            tracing::info!("Total images being sent to model: {}", total_images);
        }

        result
    }

    /// Returns the input if there was no agent running to inject into
    pub fn inject_input(&self, input: Vec<InputItem>) -> Result<(), Vec<InputItem>> {
        let mut state = self.state.lock().unwrap();
        if state.current_agent.is_some() {
            state.pending_input.push(input.into());
            Ok(())
        } else {
            Err(input)
        }
    }

    pub fn get_pending_input(&self) -> Vec<ResponseInputItem> {
        let mut state = self.state.lock().unwrap();
        if state.pending_input.is_empty() {
            Vec::with_capacity(0)
        } else {
            let mut ret = Vec::new();
            std::mem::swap(&mut ret, &mut state.pending_input);
            ret
        }
    }

    pub fn add_pending_input(&self, input: ResponseInputItem) {
        let mut state = self.state.lock().unwrap();
        state.pending_input.push(input);
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        timeout: Option<Duration>,
    ) -> anyhow::Result<CallToolResult> {
        self.mcp_connection_manager
            .call_tool(server, tool, arguments, timeout)
            .await
    }

    fn abort(&self) {
        info!("Aborting existing session");
        let mut state = self.state.lock().unwrap();
        state.pending_approvals.clear();
        state.pending_input.clear();
        if let Some(agent) = state.current_agent.take() {
            agent.abort(TurnAbortReason::Interrupted);
        }
    }

    /// Spawn the configured notifier (if any) with the given JSON payload as
    /// the last argument. Failures are logged but otherwise ignored so that
    /// notification issues do not interfere with the main workflow.
    fn maybe_notify(&self, notification: UserNotification) {
        let Some(notify_command) = &self.notify else {
            return;
        };

        if notify_command.is_empty() {
            return;
        }

        let Ok(json) = serde_json::to_string(&notification) else {
            error!("failed to serialise notification payload");
            return;
        };

        let mut command = std::process::Command::new(&notify_command[0]);
        if notify_command.len() > 1 {
            command.args(&notify_command[1..]);
        }
        command.arg(json);

        // Fire-and-forget – we do not wait for completion.
        if let Err(e) = command.spawn() {
            warn!("failed to spawn notifier '{}': {e}", notify_command[0]);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Interrupt any running turn when the session is dropped.
        self.abort();
    }
}

impl State {
    pub fn partial_clone(&self) -> Self {
        Self {
            approved_commands: self.approved_commands.clone(),
            history: self.history.clone(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ExecCommandContext {
    pub(crate) sub_id: String,
    pub(crate) call_id: String,
    pub(crate) command_for_display: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) apply_patch: Option<ApplyPatchCommandContext>,
}

#[derive(Clone, Debug)]
pub(crate) struct ApplyPatchCommandContext {
    pub(crate) user_explicitly_approved_this_action: bool,
    pub(crate) changes: HashMap<PathBuf, FileChange>,
}

/// A series of Turns in response to user input.
pub(crate) struct AgentAgent {
    sess: Arc<Session>,
    sub_id: String,
    handle: AbortHandle,
}

impl AgentAgent {
    fn spawn(sess: Arc<Session>, sub_id: String, input: Vec<InputItem>) -> Self {
        let handle =
            tokio::spawn(run_agent(Arc::clone(&sess), sub_id.clone(), input)).abort_handle();
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    fn compact(
        sess: Arc<Session>,
        sub_id: String,
        input: Vec<InputItem>,
        compact_instructions: String,
    ) -> Self {
        let handle = tokio::spawn(run_compact_agent(
            Arc::clone(&sess),
            sub_id.clone(),
            input,
            compact_instructions,
        ))
        .abort_handle();
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    fn abort(self, reason: TurnAbortReason) {
        // TOCTOU?
        if !self.handle.is_finished() {
            self.handle.abort();
            let event = Event {
                id: self.sub_id,
                msg: EventMsg::Error(ErrorEvent {
                    message: "Turn interrupted".to_string(),
                }),
            };
            let tx_event = self.sess.tx_event.clone();
            tokio::spawn(async move {
                tx_event.send(event).await.ok();
            });
        }
    }
}

async fn submission_loop(
    mut session_id: Uuid,
    config: Arc<Config>,
    auth: Option<CodexAuth>,
    rx_sub: Receiver<Submission>,
    tx_event: Sender<Event>,
) {
    let mut sess: Option<Arc<Session>> = None;
    let mut agent_manager_initialized = false;
    // shorthand - send an event when there is no active session
    let send_no_session_event = |sub_id: String| async {
        let event = Event {
            id: sub_id,
            msg: EventMsg::Error(ErrorEvent {
                message: "No session initialized, expected 'ConfigureSession' as first Op"
                    .to_string(),
            }),
        };
        tx_event.send(event).await.ok();
    };

    // To break out of this loop, send Op::Shutdown.
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        match sub.op {
            Op::Interrupt => {
                let sess = match sess.as_ref() {
                    Some(sess) => sess,
                    None => {
                        send_no_session_event(sub.id).await;
                        continue;
                    }
                };
                sess.abort();
            }
            Op::ConfigureSession {
                provider,
                model,
                model_reasoning_effort,
                model_reasoning_summary,
                model_text_verbosity,
                user_instructions,
                base_instructions,
                approval_policy,
                sandbox_policy,
                disable_response_storage,
                notify,
                cwd,
                resume_path,
            } => {
                debug!(
                    "Configuring session: model={model}; provider={provider:?}; resume={resume_path:?}"
                );
                if !cwd.is_absolute() {
                    let message = format!("cwd is not absolute: {cwd:?}");
                    error!(message);
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent { message }),
                    };
                    if let Err(e) = tx_event.send(event).await {
                        error!("failed to send error message: {e:?}");
                    }
                    return;
                }
                // Optionally resume an existing rollout.
                let mut restored_items: Option<Vec<ResponseItem>> = None;
                let rollout_recorder: Option<RolloutRecorder> =
                    if let Some(path) = resume_path.as_ref() {
                        match RolloutRecorder::resume(path, cwd.clone()).await {
                            Ok((rec, saved)) => {
                                session_id = saved.session_id;
                                if !saved.items.is_empty() {
                                    restored_items = Some(saved.items);
                                }
                                Some(rec)
                            }
                            Err(e) => {
                                warn!("failed to resume rollout from {path:?}: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    };

                let rollout_recorder = match rollout_recorder {
                    Some(rec) => Some(rec),
                    None => {
                        match RolloutRecorder::new(&config, session_id, user_instructions.clone())
                            .await
                        {
                            Ok(r) => Some(r),
                            Err(e) => {
                                warn!("failed to initialise rollout recorder: {e}");
                                None
                            }
                        }
                    }
                };

                // Create debug logger based on config
                let debug_logger = match crate::debug_logger::DebugLogger::new(config.debug) {
                    Ok(logger) => std::sync::Arc::new(std::sync::Mutex::new(logger)),
                    Err(e) => {
                        warn!("Failed to create debug logger: {}", e);
                        // Create a disabled logger as fallback
                        std::sync::Arc::new(std::sync::Mutex::new(
                            crate::debug_logger::DebugLogger::new(false).unwrap(),
                        ))
                    }
                };

                let client = ModelClient::new(
                    config.clone(),
                    auth.clone(),
                    provider.clone(),
                    model_reasoning_effort,
                    model_reasoning_summary,
                    model_text_verbosity,
                    session_id,
                    debug_logger,
                );

                // abort any current running session and clone its state
                let state = match sess.take() {
                    Some(sess) => {
                        sess.abort();
                        sess.state.lock().unwrap().partial_clone()
                    }
                    None => State {
                        history: ConversationHistory::new(),
                        ..Default::default()
                    },
                };

                let writable_roots = get_writable_roots(&cwd);

                // Error messages to dispatch after SessionConfigured is sent.
                let mut mcp_connection_errors = Vec::<Event>::new();
                let (mcp_connection_manager, failed_clients) =
                    match McpConnectionManager::new(config.mcp_servers.clone()).await {
                        Ok((mgr, failures)) => (mgr, failures),
                        Err(e) => {
                            let message = format!("Failed to create MCP connection manager: {e:#}");
                            error!("{message}");
                            mcp_connection_errors.push(Event {
                                id: sub.id.clone(),
                                msg: EventMsg::Error(ErrorEvent { message }),
                            });
                            (McpConnectionManager::default(), Default::default())
                        }
                    };

                // Surface individual client start-up failures to the user.
                if !failed_clients.is_empty() {
                    for (server_name, err) in failed_clients {
                        let message =
                            format!("MCP client for `{server_name}` failed to start: {err:#}");
                        error!("{message}");
                        mcp_connection_errors.push(Event {
                            id: sub.id.clone(),
                            msg: EventMsg::Error(ErrorEvent { message }),
                        });
                    }
                }
                let default_shell = shell::default_user_shell().await;
                sess = Some(Arc::new(Session {
                    client,
                    tools_config: ToolsConfig::new(
                        &config.model_family,
                        approval_policy,
                        sandbox_policy.clone(),
                        config.include_plan_tool,
                    ),
                    tx_event: tx_event.clone(),
                    user_instructions,
                    base_instructions,
                    approval_policy,
                    sandbox_policy,
                    shell_environment_policy: config.shell_environment_policy.clone(),
                    cwd,
                    writable_roots,
                    mcp_connection_manager,
                    agents: config.agents.clone(),
                    notify,
                    state: Mutex::new(state),
                    rollout: Mutex::new(rollout_recorder),
                    codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
                    disable_response_storage,
                    user_shell: default_shell,
                    show_raw_agent_reasoning: config.show_raw_agent_reasoning,
                    pending_browser_screenshots: Mutex::new(Vec::new()),
                    last_system_status: Mutex::new(None),
                    last_screenshot_info: Mutex::new(None),
                }));

                // Patch restored state into the newly created session.
                if let Some(sess_arc) = &sess {
                    if restored_items.is_some() {
                        let mut st = sess_arc.state.lock().unwrap();
                        st.history.record_items(restored_items.unwrap().iter());
                    }
                }

                // Gather history metadata for SessionConfiguredEvent.
                let (history_log_id, history_entry_count) =
                    crate::message_history::history_metadata(&config).await;

                // ack
                let events = std::iter::once(Event {
                    id: INITIAL_SUBMIT_ID.to_string(),
                    msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                        session_id,
                        model,
                        history_log_id,
                        history_entry_count,
                    }),
                })
                .chain(mcp_connection_errors.into_iter());
                for event in events {
                    if let Err(e) = tx_event.send(event).await {
                        error!("failed to send event: {e:?}");
                    }
                }
                
                // Initialize agent manager after SessionConfigured is sent
                if !agent_manager_initialized {
                    let mut manager = AGENT_MANAGER.write().await;
                    let (agent_tx, mut agent_rx) = tokio::sync::mpsc::unbounded_channel();
                    manager.set_event_sender(agent_tx);
                    drop(manager);

                    // Forward agent events to the main event channel
                    let tx_event_clone = tx_event.clone();
                    tokio::spawn(async move {
                        while let Some(event) = agent_rx.recv().await {
                            let _ = tx_event_clone.send(event).await;
                        }
                    });
                    agent_manager_initialized = true;
                }
            }
            Op::UserInput { items } => {
                let sess = match sess.as_ref() {
                    Some(sess) => sess,
                    None => {
                        send_no_session_event(sub.id).await;
                        continue;
                    }
                };

                // Clean up old status items when new user input arrives
                // This prevents token buildup from old screenshots/status messages
                sess.cleanup_old_status_items().await;

                // attempt to inject input into current agent
                if let Err(items) = sess.inject_input(items) {
                    // no current agent, spawn a new one
                    let agent = AgentAgent::spawn(Arc::clone(sess), sub.id, items);
                    sess.set_agent(agent);
                }
            }
            Op::ExecApproval { id, decision } => {
                let sess = match sess.as_ref() {
                    Some(sess) => sess,
                    None => {
                        send_no_session_event(sub.id).await;
                        continue;
                    }
                };
                match decision {
                    ReviewDecision::Abort => {
                        sess.abort();
                    }
                    other => sess.notify_approval(&id, other),
                }
            }
            Op::PatchApproval { id, decision } => {
                let sess = match sess.as_ref() {
                    Some(sess) => sess,
                    None => {
                        send_no_session_event(sub.id).await;
                        continue;
                    }
                };
                match decision {
                    ReviewDecision::Abort => {
                        sess.abort();
                    }
                    other => sess.notify_approval(&id, other),
                }
            }
            Op::AddToHistory { text } => {
                // TODO: What should we do if we got AddToHistory before ConfigureSession?
                // currently, if ConfigureSession has resume path, this history will be ignored
                let id = session_id;
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::message_history::append_entry(&text, &id, &config).await
                    {
                        warn!("failed to append to message history: {e}");
                    }
                });
            }

            Op::GetHistoryEntryRequest { offset, log_id } => {
                let config = config.clone();
                let tx_event = tx_event.clone();
                let sub_id = sub.id.clone();

                tokio::spawn(async move {
                    // Run lookup in blocking thread because it does file IO + locking.
                    let entry_opt = tokio::task::spawn_blocking(move || {
                        crate::message_history::lookup(log_id, offset, &config)
                    })
                    .await
                    .unwrap_or(None);

                    let event = Event {
                        id: sub_id,
                        msg: EventMsg::GetHistoryEntryResponse(
                            crate::protocol::GetHistoryEntryResponseEvent {
                                offset,
                                log_id,
                                entry: entry_opt,
                            },
                        ),
                    };

                    if let Err(e) = tx_event.send(event).await {
                        warn!("failed to send GetHistoryEntryResponse event: {e}");
                    }
                });
            }
            // Upstream protocol no longer includes ListMcpTools; skip handling here.
            Op::Compact => {
                let sess = match sess.as_ref() {
                    Some(sess) => sess,
                    None => {
                        send_no_session_event(sub.id).await;
                        continue;
                    }
                };

                // Create a summarization request as user input
                const SUMMARIZATION_PROMPT: &str = include_str!("prompt_for_compact_command.md");

                // Attempt to inject input into current agent
                if let Err(items) = sess.inject_input(vec![InputItem::Text {
                    text: "Start Summarization".to_string(),
                }]) {
                    let agent = AgentAgent::compact(
                        sess.clone(),
                        sub.id,
                        items,
                        SUMMARIZATION_PROMPT.to_string(),
                    );
                    sess.set_agent(agent);
                }
            }
            Op::Shutdown => {
                info!("Shutting down Codex instance");

                // Gracefully flush and shutdown rollout recorder on session end so tests
                // that inspect the rollout file do not race with the background writer.
                if let Some(sess_arc) = sess {
                    let recorder_opt = sess_arc.rollout.lock().unwrap().take();
                    if let Some(rec) = recorder_opt {
                        if let Err(e) = rec.shutdown().await {
                            warn!("failed to shutdown rollout recorder: {e}");
                            let event = Event {
                                id: sub.id.clone(),
                                msg: EventMsg::Error(ErrorEvent {
                                    message: "Failed to shutdown rollout recorder".to_string(),
                                }),
                            };
                            if let Err(e) = tx_event.send(event).await {
                                warn!("failed to send error message: {e:?}");
                            }
                        }
                    }
                }
                let event = Event {
                    id: sub.id.clone(),
                    msg: EventMsg::ShutdownComplete,
                };
                if let Err(e) = tx_event.send(event).await {
                    warn!("failed to send Shutdown event: {e}");
                }
                break;
            }
        }
    }
    debug!("Agent loop exited");
}

/// Takes a user message as input and runs a loop where, at each turn, the model
/// replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single turn, in practice, we generally one item per turn:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next turn.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the agent complete.
async fn run_agent(sess: Arc<Session>, sub_id: String, input: Vec<InputItem>) {
    if input.is_empty() {
        return;
    }
    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskStarted,
    };
    if sess.tx_event.send(event).await.is_err() {
        return;
    }

    // Debug logging for ephemeral images
    let ephemeral_count = input
        .iter()
        .filter(|item| matches!(item, InputItem::EphemeralImage { .. }))
        .count();

    if ephemeral_count > 0 {
        tracing::info!(
            "Processing {} ephemeral images in user input",
            ephemeral_count
        );
    }

    // Convert input to ResponseInputItem
    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input);
    let initial_response_item: ResponseItem = initial_input_for_turn.clone().into();

    // Record to history but we'll handle ephemeral images separately
    sess.record_conversation_items(&[initial_response_item.clone()])
        .await;

    let mut last_task_message: Option<String> = None;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Agent which contains
    // many turns, from the perspective of the user, it is a single turn.
    let mut turn_diff_tracker = TurnDiffTracker::new();

    // Track if this is the first iteration - if so, include the initial input
    let mut first_iteration = true;

    loop {
        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_input = sess
            .get_pending_input()
            .into_iter()
            .map(ResponseItem::from)
            .collect::<Vec<ResponseItem>>();

        // Do not duplicate the initial input in `pending_input`.
        // It is already recorded to history above; ephemeral items are appended separately.
        if first_iteration {
            first_iteration = false;
        } else {
            // Only record pending input to history on subsequent iterations
            sess.record_conversation_items(&pending_input).await;
        }

        // Construct the input that we will send to the model. When using the
        // Chat completions API (or ZDR clients), the model needs the full
        // conversation history on each turn. The rollout file, however, should
        // only record the new items that originated in this turn so that it
        // represents an append-only log without duplicates.
        let turn_input: Vec<ResponseItem> = sess.turn_input_with_history(pending_input);

        let turn_input_messages: Vec<String> = turn_input
            .iter()
            .filter_map(|item| match item {
                ResponseItem::Message { content, .. } => Some(content),
                _ => None,
            })
            .flat_map(|content| {
                content.iter().filter_map(|item| match item {
                    ContentItem::OutputText { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        match run_turn(&sess, &mut turn_diff_tracker, sub_id.clone(), turn_input).await {
            Ok(turn_output) => {
                let mut items_to_record_in_conversation_history = Vec::<ResponseItem>::new();
                let mut responses = Vec::<ResponseInputItem>::new();
                for processed_response_item in turn_output {
                    let ProcessedResponseItem { item, response } = processed_response_item;
                    match (&item, &response) {
                        (ResponseItem::Message { role, .. }, None) if role == "assistant" => {
                            // If the model returned a message, we need to record it.
                            items_to_record_in_conversation_history.push(item);
                        }
                        (
                            ResponseItem::LocalShellCall { .. },
                            Some(ResponseInputItem::FunctionCallOutput { call_id, output }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output: output.clone(),
                                },
                            );
                        }
                        (
                            ResponseItem::FunctionCall { .. },
                            Some(ResponseInputItem::FunctionCallOutput { call_id, output }),
                        ) => {
                            debug!(
                                "Recording function call and output for call_id: {}",
                                call_id
                            );
                            items_to_record_in_conversation_history.push(item);
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output: output.clone(),
                                },
                            );
                        }
                        (
                            ResponseItem::FunctionCall { .. },
                            Some(ResponseInputItem::McpToolCallOutput { call_id, result }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            let (content, success): (String, Option<bool>) = match result {
                                Ok(CallToolResult {
                                    content,
                                    is_error,
                                    structured_content: _,
                                }) => match serde_json::to_string(content) {
                                    Ok(content) => (content, *is_error),
                                    Err(e) => {
                                        warn!("Failed to serialize MCP tool call output: {e}");
                                        (e.to_string(), Some(true))
                                    }
                                },
                                Err(e) => (e.clone(), Some(true)),
                            };
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output: FunctionCallOutputPayload { content, success },
                                },
                            );
                        }
                        (
                            ResponseItem::Reasoning {
                                id,
                                summary,
                                content,
                                encrypted_content,
                            },
                            None,
                        ) => {
                            items_to_record_in_conversation_history.push(ResponseItem::Reasoning {
                                id: id.clone(),
                                summary: summary.clone(),
                                content: content.clone(),
                                encrypted_content: encrypted_content.clone(),
                            });
                        }
                        _ => {
                            warn!("Unexpected response item: {item:?} with response: {response:?}");
                        }
                    };
                    if let Some(response) = response {
                        responses.push(response);
                    }
                }

                // Only attempt to take the lock if there is something to record.
                if !items_to_record_in_conversation_history.is_empty() {
                    // Record items in their original chronological order to maintain
                    // proper sequence of events. This ensures function calls and their
                    // outputs appear in the correct order in conversation history.
                    sess.record_conversation_items(&items_to_record_in_conversation_history)
                        .await;
                }

                // If there are responses, add them to pending input for the next iteration
                if !responses.is_empty() {
                    for response in &responses {
                        sess.add_pending_input(response.clone());
                    }
                }

                if responses.is_empty() {
                    debug!("Turn completed");
                    last_task_message = get_last_assistant_message_from_turn(
                        &items_to_record_in_conversation_history,
                    );
                    sess.maybe_notify(UserNotification::AgentTurnComplete {
                        turn_id: sub_id.clone(),
                        input_messages: turn_input_messages,
                        last_assistant_message: last_task_message.clone(),
                    });
                    break;
                }
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                let event = Event {
                    id: sub_id.clone(),
                    msg: EventMsg::Error(ErrorEvent {
                        message: e.to_string(),
                    }),
                };
                sess.tx_event.send(event).await.ok();
                // let the user continue the conversation
                break;
            }
        }
    }
    sess.remove_agent(&sub_id);

    // After a completed turn, optionally summarize-and-prune conversation history
    // when semantic memory is enabled. This keeps history bounded and records
    // a compact summary for future retrieval (Phase 1 uses a JSONL store).
    {
        let mem_cfg = sess.client.get_memory_config();
        if mem_cfg.enabled && mem_cfg.summarize_on_prune {
            // Keep a configurable number of recent messages to avoid surprising truncation
            let keep_last_messages: usize = mem_cfg.keep_last_messages.max(1);

            let repo_key = crate::util::repo_key(&sess.cwd);
            let store = crate::memory::store_jsonl::JsonlMemoryStore::new(sess.client.get_codex_home());
            // Prefer LLM-backed summarizer when configured and API key is available.
            let summary_max = mem_cfg.summary_max_chars.max(50);
            let mut summarizer_box: Box<dyn crate::memory::summarizer::Summarizer> = {
                if mem_cfg.use_llm_summarizer && has_openai_api_key(sess.client.get_codex_home()) {
                    if let Some(p) = built_in_model_providers().get("openai").cloned() {
                        if let Ok(s) = crate::memory::summarizer::OpenAiNanoSummarizer::from_provider(
                            &p,
                            sess.client.get_codex_home(),
                            &mem_cfg.summarizer_model,
                            summary_max,
                        ) {
                            Box::new(s)
                        } else {
                            Box::new(crate::memory::summarizer::CompactSummarizer::new(summary_max))
                        }
                    } else {
                        Box::new(crate::memory::summarizer::CompactSummarizer::new(summary_max))
                    }
                } else {
                    Box::new(crate::memory::summarizer::CompactSummarizer::new(summary_max))
                }
            };
            let pruner = crate::conversation_history::prune::ConversationHistoryPruner::new(
                keep_last_messages,
            );

            let (maybe_summary, post_prune) = {
                let mut state = sess.state.lock().unwrap();
                let maybe_summary = pruner.summarize_then_prune(
                    &mut state.history,
                    &*summarizer_box,
                    &store,
                    &repo_key,
                    sess.client.get_session_id(),
                );
                // After pruning, compute a status-only token context update so the UI can
                // refresh the percent-left indicator immediately without changing totals.
                let baseline_cached = state
                    .last_completed_token_usage
                    .as_ref()
                    .and_then(|u| u.cached_input_tokens)
                    .unwrap_or(0);
                let remaining_items = state.history.contents();
                let est_tokens = estimate_tokens_for_items(&remaining_items) as u64;
                let post_prune = crate::protocol::TokenUsage {
                    // Treat total as baseline + estimated remaining to make
                    // tokens_in_context_window() - baseline ~= est_tokens.
                    input_tokens: est_tokens.saturating_add(baseline_cached),
                    cached_input_tokens: Some(baseline_cached),
                    output_tokens: 0,
                    reasoning_output_tokens: Some(0),
                    total_tokens: est_tokens.saturating_add(baseline_cached),
                };
                (maybe_summary, post_prune)
            };
            sess.tx_event
                .send(Event {
                    id: sub_id.clone(),
                    msg: EventMsg::TokenContextUpdate(post_prune),
                })
                .await
                .ok();

            // If embeddings are enabled and an API key is configured, embed and persist the vector.
            if mem_cfg.embedding.enabled && has_openai_api_key(sess.client.get_codex_home()) {
                if let Some(summary) = maybe_summary {
                    // Build OpenAI embeddings client from built-in provider (respects OPENAI_BASE_URL override)
                    if let Some(p) = built_in_model_providers().get("openai").cloned() {
                        if let Ok(emb) = OpenAiEmbeddingClient::from_provider(&p, sess.client.get_codex_home()) {
                            let dim = mem_cfg.embedding.dim;
                            let text = format!("{}\n{}", summary.title, summary.text);
                            if let Ok(vecs) = emb.embed(&[text.clone()], dim) {
                                if let Some(v) = vecs.into_iter().next() {
                                    let now_ms: u64 = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map_err(|e| std::io::Error::other(format!("clock error: {e}")))
                                        .unwrap_or_default()
                                        .as_millis() as u64;
                                    let rec = EmbRec {
                                        repo_key: repo_key.clone(),
                                        id: Uuid::new_v4().to_string(),
                                        ts: now_ms,
                                        kind: "summary".to_string(),
                                        title: summary.title,
                                        text: summary.text,
                                        dim,
                                        vec: v,
                                    };
                                    let vstore = JsonlVectorStore::new(sess.client.get_codex_home());
                                    let _ = vstore.add(&rec);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let event = Event {
        id: sub_id,
        msg: EventMsg::TaskComplete(TaskCompleteEvent {
            last_agent_message: last_task_message,
        }),
    };
    sess.tx_event.send(event).await.ok();
}

async fn run_turn(
    sess: &Session,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    input: Vec<ResponseItem>,
) -> CodexResult<Vec<ProcessedResponseItem>> {
    // Check if browser is enabled
    let browser_enabled = codex_browser::global::get_browser_manager().await.is_some();
    
    let tools = get_openai_tools(
        &sess.tools_config,
        Some(sess.mcp_connection_manager.list_all_tools()),
        browser_enabled,
    );

    let mut retries = 0;
    let mut injection_notice_sent = false;
    loop {
        // Build status items (screenshots, system status) fresh for each attempt
        let status_items = build_turn_status_items(sess).await;

        // Optionally inject semantic memory summaries or retrieval ahead of history
        let mut injected_input: Vec<ResponseItem> = Vec::new();
        if sess.client.get_memory_config().enabled {
            // Compute a conservative per-turn budget for memory injection to avoid context overflows.
            let char_budget = compute_injection_char_budget(sess, &input);
            // Optionally build a hybrid retrieval message combining code index + memory summaries.
            if let Some(mem) = build_hybrid_injection_items(sess, &input, char_budget) {
                injected_input.push(mem);
            }
        }

        let prompt = Prompt {
            input: if injected_input.is_empty() {
                input.clone()
            } else {
                [injected_input, input.clone()].concat()
            },
            user_instructions: sess.user_instructions.clone(),
            store: !sess.disable_response_storage,
            tools: tools.clone(),
            base_instructions_override: sess.base_instructions.clone(),
            environment_context: Some(EnvironmentContext::new(
                Some(sess.cwd.clone()),
                Some(sess.approval_policy),
                Some(sess.sandbox_policy.clone()),
                Some(sess.user_shell.clone()),
            )),
            status_items, // Include status items with this request
        };

        // If we injected memory/code hints, emit a lightweight background notice once
        if !injection_notice_sent {
            if let Some(ResponseItem::Message { content, .. }) = prompt.input.first() {
                // Find the first InputText block (hybrid injection is a single text message)
                if let Some(ContentItem::InputText { text }) = content.iter().find(|c| matches!(c, ContentItem::InputText { .. })) {
                    let mut code_count = 0usize;
                    let mut mem_count = 0usize;
                    enum Sec { None, Code, Mem }
                    let mut sec = Sec::None;
                    for line in text.lines() {
                        if line.starts_with("[memory:code ") {
                            sec = Sec::Code;
                            continue;
                        }
                        if line.starts_with("[memory:retrieval ") || line.starts_with("[memory:summary ") {
                            sec = Sec::Mem;
                            continue;
                        }
                        if line.starts_with("[memory:") {
                            // Unknown memory header
                            sec = Sec::None;
                            continue;
                        }
                        if line.starts_with("- ") {
                            match sec {
                                Sec::Code => code_count += 1,
                                Sec::Mem => mem_count += 1,
                                Sec::None => {}
                            }
                        }
                    }
                    if code_count > 0 || mem_count > 0 {
                        let msg = format!("Injected code hints: {code_count}; memory items: {mem_count}");
                        sess.notify_background_event(&sub_id, msg).await;
                        injection_notice_sent = true;
                    }
                }
            }
        }

        match try_run_turn(sess, turn_diff_tracker, &sub_id, &prompt).await {
            Ok(output) => {
                // Do not record per‑turn status items (screenshots/system status)
                // to the conversation history. They are injected fresh each turn
                // and marked ephemeral so they do not pollute persistent context.
                return Ok(output);
            }
            Err(CodexErr::Interrupted) => return Err(CodexErr::Interrupted),
            Err(CodexErr::EnvVar(var)) => return Err(CodexErr::EnvVar(var)),
            Err(e @ (CodexErr::UsageLimitReached(_) | CodexErr::UsageNotIncluded)) => {
                return Err(e);
            }
            Err(e) => {
                // Use the configured provider-specific stream retry budget.
                let max_retries = sess.client.get_provider().stream_max_retries();
                if retries < max_retries {
                    retries += 1;
                    let delay = match e {
                        CodexErr::Stream(_, Some(delay)) => delay,
                        _ => backoff(retries),
                    };
                    warn!(
                        "stream disconnected - retrying turn ({retries}/{max_retries} in {delay:?})...",
                    );

                    // Surface retry information to any UI/front‑end so the
                    // user understands what is happening instead of staring
                    // at a seemingly frozen screen.
                    sess.notify_background_event(
                        &sub_id,
                        format!(
                            "stream error: {e}; retrying {retries}/{max_retries} in {delay:?}…"
                        ),
                    )
                    .await;

                    tokio::time::sleep(delay).await;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// Build a single user message with recent memory summaries constrained by budget.
fn build_memory_injection_items(sess: &Session, char_budget_override: usize) -> Option<ResponseItem> {
    use crate::models::{ContentItem, ResponseItem};

    let mem_cfg = sess.client.get_memory_config();
    let repo_key = crate::util::repo_key(&sess.cwd);
    let store = crate::memory::store_jsonl::JsonlMemoryStore::new(sess.client.get_codex_home());

    let limit = std::cmp::max(1, mem_cfg.inject.max_items);
    let Ok(rows) = store.recent(&repo_key, limit) else { return None };
    if rows.is_empty() { return None; }

    let max_chars = std::cmp::min(mem_cfg.inject.max_chars, char_budget_override.max(0));
    if max_chars == 0 { return None; }
    if let Some(text) = budget_summaries_to_text(rows, &repo_key, mem_cfg.inject.max_items, max_chars) {
        return Some(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text }],
        });
    }
    None
}

/// Build a retrieval-based memory injection (embeddings KNN) if enabled and available.
fn build_embedding_memory_injection_items(sess: &Session, turn_input: &Vec<ResponseItem>, char_budget_override: usize) -> Option<ResponseItem> {
    use crate::models::{ContentItem, ResponseItem};

    let mem_cfg = sess.client.get_memory_config();
    if !mem_cfg.embedding.enabled { return None; }
    if !has_openai_api_key(sess.client.get_codex_home()) { return None; }

    // Extract the latest user text from the current turn input.
    let mut query = String::new();
    for item in turn_input.iter().rev() {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "user" {
                for c in content {
                    match c {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            if !query.is_empty() { query.push(' '); }
                            query.push_str(text);
                        }
                        _ => {}
                    }
                }
                if !query.trim().is_empty() { break; }
            }
        }
    }
    let query = query.trim();
    if query.is_empty() { return None; }

    // Build OpenAI embeddings client
    let provider = match built_in_model_providers().get("openai").cloned() {
        Some(p) => p,
        None => return None,
    };
    let client = match OpenAiEmbeddingClient::from_provider(&provider, sess.client.get_codex_home()) {
        Ok(c) => c,
        Err(_) => return None,
    };

    let dim = mem_cfg.embedding.dim;
    let repo_key = crate::util::repo_key(&sess.cwd);
    let vstore = JsonlVectorStore::new(sess.client.get_codex_home());
    let Ok(vecs) = client.embed(&[query.to_string()], dim) else { return None };
    let Some(vec) = vecs.into_iter().next() else { return None };
    let top_k = std::cmp::max(1, mem_cfg.embedding.top_k);
    // Prefer only summary-kind hits here to avoid overlap with code section
    let Ok(mut hits) = vstore.query_kind(&repo_key, "summary", &vec, top_k) else { return None };
    // Blend recency priors into similarity for improved ranking
    blend_hits_with_recency_with_params(&mut hits, mem_cfg.recency_blend_alpha, mem_cfg.recency_half_life_days);
    if hits.is_empty() { return None; }

    let max_chars = std::cmp::min(mem_cfg.inject.max_chars, char_budget_override.max(0));
    if max_chars == 0 { return None; }
    if let Some(text) = budget_hits_to_text(hits, &repo_key, mem_cfg.inject.max_items, max_chars) {
        return Some(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText { text }],
        });
    }
    None
}

/// Render retrieved memory hits into a single user message within budgets.
fn budget_hits_to_text(
    mut hits: Vec<codex_memory::store::SearchHit>,
    repo_key: &str,
    max_items: usize,
    max_chars: usize,
) -> Option<String> {
    if hits.is_empty() || max_items == 0 || max_chars == 0 { return None; }
    if hits.len() > max_items { hits.truncate(max_items); }

    let header = format!("[memory:retrieval v1 | repo={repo_key}]");
    if header.len() + 1 > max_chars { return None; }
    let mut remaining = max_chars - (header.len() + 1);
    let mut text = String::new();
    text.push_str(&header);
    text.push('\n');
    let mut bullets_written = 0usize;

    for h in hits.into_iter() {
        let bullet_full = format!("- {}: {}", h.title, h.text);
        let need = bullet_full.len() + 1;
        if need <= remaining {
            text.push_str(&bullet_full);
            text.push('\n');
            remaining -= need;
            bullets_written += 1;
        } else if remaining > 4 {
            let take = remaining - 4;
            let truncated: String = bullet_full.chars().take(take).collect();
            text.push_str(&truncated);
            text.push_str(" ...\n");
            remaining = 0;
            bullets_written += 1;
            break;
        } else {
            break;
        }
    }
    if bullets_written == 0 { None } else { Some(text) }
}

/// Adjust similarity scores with a mild recency prior and sort in‑place.
fn blend_hits_with_recency(hits: &mut Vec<codex_memory::store::SearchHit>) {
    blend_hits_with_recency_with_params(hits, 0.15, 7.0);
}

fn blend_hits_with_recency_with_params(
    hits: &mut Vec<codex_memory::store::SearchHit>,
    alpha: f32,
    half_life_days: f32,
) {
    if hits.is_empty() { return; }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let half_life_days = if half_life_days <= 0.0 { 7.0 } else { half_life_days };
    let alpha = alpha.clamp(0.0, 1.0);
    for h in hits.iter_mut() {
        let age_days = ((now_ms.saturating_sub(h.ts)) as f32) / (1000.0 * 60.0 * 60.0 * 24.0);
        let recency = (-std::f32::consts::LN_2 * (age_days / half_life_days)).exp();
        let s = h.score.clamp(0.0, 1.0);
        let blended = (1.0 - alpha) * s + alpha * recency;
        h.score = blended;
    }
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
}

/// Build a single user message that merges code index retrieval and memory retrieval
/// within the provided budget. Prefers allocating ~60% to code and ~40% to memory.
fn build_hybrid_injection_items(sess: &Session, turn_input: &Vec<ResponseItem>, total_budget: usize) -> Option<ResponseItem> {
    if total_budget == 0 { return None; }
    let mem_cfg = sess.client.get_memory_config();
    let repo_key = crate::util::repo_key(&sess.cwd);

    // If code index is enabled and API key present, ensure index, then query code.
    let mut code_text: Option<String> = None;
    if mem_cfg.embedding.enabled && mem_cfg.code_index.enabled && has_openai_api_key(sess.client.get_codex_home()) {
        // Kick off indexing (best effort, once per repo)
        ensure_code_index(&repo_key, sess.client.get_codex_home(), &sess.cwd, mem_cfg.embedding.dim, mem_cfg.code_index.chunk_bytes);
        if let Some(ct) = build_code_retrieval_text(sess, turn_input, ((total_budget as f64) * 0.6) as usize) {
            code_text = Some(ct);
        }
    }

    // Memory retrieval (embeddings) fallback to summaries
    let mem_budget = total_budget.saturating_sub(code_text.as_ref().map(|s| s.len()).unwrap_or(0));
    let mut mem_text: Option<String> = None;
    if mem_budget > 0 {
        if let Some(rt) = build_embedding_retrieval_text(sess, turn_input, mem_budget.min(mem_cfg.inject.max_chars)) {
            mem_text = Some(rt);
        } else if let Some(st) = build_summary_retrieval_text(sess, mem_budget.min(mem_cfg.inject.max_chars)) {
            mem_text = Some(st);
        }
    }

    if code_text.is_none() && mem_text.is_none() { return None; }
    let mut lines = String::new();
    if let Some(ct) = code_text.as_ref() { lines.push_str(ct); lines.push('\n'); }
    if let Some(mut mt) = mem_text { 
        if let Some(ctxt) = code_text.as_ref() {
            if mem_cfg.fuzzy_dedupe_enabled {
                mt = dedupe_memory_against_code_with_thresholds(
                    ctxt,
                    &mt,
                    mem_cfg.fuzzy_dedupe_title_jaccard,
                    mem_cfg.fuzzy_dedupe_content_jaccard,
                    mem_cfg.fuzzy_dedupe_min_containment_prefix,
                );
            }
        }
        lines.push_str(&mt);
    }

    Some(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: lines }],
    })
}

fn dedupe_memory_against_code(code_text: &str, memory_text: &str) -> String {
    dedupe_memory_against_code_with_thresholds(
        code_text,
        memory_text,
        0.97,
        0.92,
        32,
    )
}

fn dedupe_memory_against_code_with_thresholds(
    code_text: &str,
    memory_text: &str,
    title_jaccard_threshold: f32,
    content_jaccard_threshold: f32,
    min_containment_prefix: usize,
) -> String {
    // Extract bullet texts from code section
    let mut code_snippets: Vec<(String, String)> = Vec::new(); // (title, text)
    for line in code_text.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some((title, txt)) = rest.split_once(':') {
                code_snippets.push((title.trim().to_string(), txt.trim().to_string()));
            }
        }
    }
    if code_snippets.is_empty() { return memory_text.to_string(); }

    // Rebuild memory text filtering bullets that strongly overlap with code text
    let mut out = String::new();
    let mut wrote_header = false;
    for (idx, line) in memory_text.lines().enumerate() {
        if idx == 0 {
            // header line
            out.push_str(line);
            out.push('\n');
            wrote_header = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("- ") {
            if let Some((mtitle, mtxt)) = rest.split_once(':') {
                let mtitle = mtitle.trim();
                let mtxt = mtxt.trim();

                let is_dup = code_snippets.iter().any(|(ctitle, ctxt)| {
                    let title_sim = jaccard_similarity_tokens(mtitle, ctitle) >= title_jaccard_threshold;
                    let content_sim = jaccard_similarity_tokens(mtxt, ctxt) >= content_jaccard_threshold;
                    let strong_containment = {
                        let (a, b): (&str, &str) = if mtxt.len() >= ctxt.len() { (mtxt, ctxt.as_str()) } else { (ctxt, mtxt) };
                        let mut min_take = min_containment_prefix;
                        min_take = min_take.min(a.len()).min(b.len());
                        if min_take == 0 { false } else { a.contains(&b[..min_take]) }
                    };

                    // Require both high title similarity and high content similarity (or strong containment)
                    (title_sim && (content_sim || strong_containment)) || strong_containment
                });
                if is_dup { continue; }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    if wrote_header { out.trim_end().to_string() } else { memory_text.to_string() }
}

fn jaccard_similarity_tokens(a: &str, b: &str) -> f32 {
    use std::collections::HashSet;
    fn tokens(s: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        for w in s.split(|c: char| !c.is_alphanumeric()) {
            let t = w.trim().to_ascii_lowercase();
            if !t.is_empty() { out.insert(t); }
        }
        out
    }
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() && tb.is_empty() { return 1.0; }
    if ta.is_empty() || tb.is_empty() { return 0.0; }
    let inter = ta.intersection(&tb).count() as f32;
    let union = (ta.len() + tb.len()) as f32 - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn build_code_retrieval_text(sess: &Session, turn_input: &Vec<ResponseItem>, char_budget: usize) -> Option<String> {
    if char_budget == 0 { return None; }
    let mem_cfg = sess.client.get_memory_config();
    if !mem_cfg.code_index.enabled { return None; }

    // Prepare budget and header
    let repo_key = crate::util::repo_key(&sess.cwd);
    let header = format!("[memory:code v1 | repo={repo_key}]");
    if header.len() + 1 > char_budget { return None; }
    let mut remaining = char_budget - (header.len() + 1);

    // Collect bullets from semantic code index (when embeddings available)
    let mut bullets: Vec<String> = Vec::new();
    let query = match extract_latest_user_text(turn_input) { Some(q) => q, None => String::new() };

    // Semantic retrieval via code index vectors
    if mem_cfg.embedding.enabled && has_openai_api_key(sess.client.get_codex_home()) {
        if let Some(p) = built_in_model_providers().get("openai").cloned() {
            if let Ok(client) = OpenAiEmbeddingClient::from_provider(&p, sess.client.get_codex_home()) {
                let dim = mem_cfg.embedding.dim;
                if let Ok(vecs) = client.embed(&[query.clone()], dim) {
                    if let Some(vec) = vecs.into_iter().next() {
                        let vstore = JsonlVectorStore::new(sess.client.get_codex_home());
                        let top_k = std::cmp::max(1, mem_cfg.code_index.top_k);
                        if let Ok(hits) = vstore.query_kind(&repo_key, "code", &vec, top_k) {
                            for h in hits.into_iter() {
                                bullets.push(format!("- {}: {}", h.title, h.text));
                            }
                        }
                    }
                }
            }
        }
    }

    // Lexical retrieval via fuzzy file search on workspace paths
    // Split remaining budget roughly in half between semantic and lexical bullets if both exist.
    let lexical_file_limit = std::cmp::max(1, mem_cfg.code_index.top_k);
    let chunk_bytes = std::cmp::max(512, mem_cfg.code_index.chunk_bytes);
    let lexical_bullets = gather_lexical_bullets(sess, &query, lexical_file_limit, chunk_bytes);
    // Interleave: prefer semantic first, then lexical, but keep both.
    if bullets.is_empty() {
        bullets.extend(lexical_bullets);
    } else {
        // Interleave by alternating semantic and lexical to increase variety.
        let mut merged: Vec<String> = Vec::new();
        let mut i = 0usize;
        let mut j = 0usize;
        while i < bullets.len() || j < lexical_bullets.len() {
            if i < bullets.len() { merged.push(bullets[i].clone()); i += 1; }
            if j < lexical_bullets.len() { merged.push(lexical_bullets[j].clone()); j += 1; }
        }
        bullets = merged;
    }

    if bullets.is_empty() { return None; }

    // Render into the budget
    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');
    for b in bullets.into_iter() {
        let need = b.len() + 1;
        if need <= remaining {
            out.push_str(&b);
            out.push('\n');
            remaining -= need;
        } else if remaining > 4 {
            let take = remaining - 4;
            let truncated: String = b.chars().take(take).collect();
            out.push_str(&truncated);
            out.push_str(" ...\n");
            remaining = 0;
            break;
        } else {
            break;
        }
    }
    if out.trim().is_empty() { None } else { Some(out) }
}

fn gather_lexical_bullets(sess: &Session, query: &str, file_limit: usize, chunk_bytes: usize) -> Vec<String> {
    use std::num::NonZeroUsize;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    if query.trim().is_empty() { return Vec::new(); }

    let limit = NonZeroUsize::new(file_limit.max(1)).unwrap();
    let threads = NonZeroUsize::new(4).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let results = file_search::run(
        query,
        limit,
        &sess.cwd,
        Vec::new(),
        threads,
        cancel,
        false,
    );
    let Ok(res) = results else { return Vec::new() };
    let mut bullets: Vec<String> = Vec::new();
    for fm in res.matches.into_iter().take(file_limit) {
        let rel = fm.path;
        let path = sess.cwd.join(&rel);
        // Read a small chunk from the beginning; skip binary files
        let snippet = match std::fs::File::open(&path) {
            Ok(mut f) => {
                use std::io::Read;
                let mut buf = Vec::new();
                // Cap read to chunk_bytes*2 to improve likelihood of a useful excerpt while still bounded
                let cap = (chunk_bytes as u64).saturating_mul(2) as usize;
                let _ = f.by_ref().take(cap as u64).read_to_end(&mut buf);
                if buf.iter().any(|&b| b == 0) { continue; }
                match String::from_utf8(buf) {
                    Ok(s) => s,
                    Err(e) => {
                        // Best effort: take the valid prefix
                        let valid = e.into_bytes();
                        let upto = valid.len().min(cap);
                        String::from_utf8_lossy(&valid[..upto]).to_string()
                    }
                }
            }
            Err(_) => continue,
        };

        let mut text = snippet;
        // Trim to chunk_bytes chars and prefer ending at a newline when present.
        if text.chars().count() > chunk_bytes {
            let prefix: String = text.chars().take(chunk_bytes).collect();
            if let Some(pos) = prefix.rfind('\n') {
                // Safe: pos is a valid byte index into `prefix`
                text = prefix[..=pos].to_string();
            } else {
                text = prefix;
            }
        }
        let title = format!("{}:#1", rel);
        bullets.push(format!("- {}: {}", title, text.trim()));
    }
    bullets
}

fn build_embedding_retrieval_text(sess: &Session, turn_input: &Vec<ResponseItem>, char_budget: usize) -> Option<String> {
    let tmp = build_embedding_memory_injection_items(sess, turn_input, char_budget)?;
    if let ResponseItem::Message { content, .. } = tmp {
        if let Some(crate::models::ContentItem::InputText { text }) = content.into_iter().next() { return Some(text); }
    }
    None
}

fn build_summary_retrieval_text(sess: &Session, char_budget: usize) -> Option<String> {
    let tmp = build_memory_injection_items(sess, char_budget)?;
    if let ResponseItem::Message { content, .. } = tmp {
        if let Some(crate::models::ContentItem::InputText { text }) = content.into_iter().next() { return Some(text); }
    }
    None
}

fn extract_latest_user_text(turn_input: &Vec<ResponseItem>) -> Option<String> {
    for item in turn_input.iter().rev() {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "user" {
                let mut s = String::new();
                for c in content {
                    if let ContentItem::InputText { text } | ContentItem::OutputText { text } = c { if !s.is_empty() { s.push(' ');} s.push_str(text) }
                }
                if !s.trim().is_empty() { return Some(s); }
            }
        }
    }
    None
}

/// Estimate text tokens from a list of response items using a simple heuristic (chars/4).
fn estimate_tokens_for_items(items: &[ResponseItem]) -> usize {
    let mut chars: usize = 0;
    for it in items {
        match it {
            ResponseItem::Message { content, .. } => {
                for c in content {
                    match c {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            chars = chars.saturating_add(text.len());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    // Roughly 4 chars per token for English text
    (chars + 3) / 4
}

/// Compute a conservative character budget for memory injection based on the model's
/// context window, estimated size of the current turn input, and a fixed safety margin.
fn compute_injection_char_budget(sess: &Session, turn_input: &Vec<ResponseItem>) -> usize {
    let window_tokens = sess
        .client
        .get_model_context_window()
        .unwrap_or(128_000);
    let reserve_output = sess
        .client
        .get_model_max_output_tokens()
        .unwrap_or(1_024);
    let input_tokens = estimate_tokens_for_items(turn_input) as u64;
    // Reserve a safety margin for instructions/tools/etc.
    let safety_margin_tokens: u64 = 2_000;
    // Also cap memory injection to at most 10% of the context window
    let hard_cap_tokens: u64 = (window_tokens as f64 * 0.10) as u64;

    let available = window_tokens
        .saturating_sub(reserve_output)
        .saturating_sub(input_tokens)
        .saturating_sub(safety_margin_tokens);
    let allowed_tokens = available.min(hard_cap_tokens);
    if allowed_tokens == 0 { return 0; }
    // Convert tokens to chars (4 chars per token heuristic)
    (allowed_tokens as usize) * 4
}

/// Render the memory summaries into a single user-visible text block constrained by budgets.
fn budget_summaries_to_text(
    mut rows: Vec<crate::memory::store_jsonl::StoredSummary>,
    repo_key: &str,
    max_items: usize,
    max_chars: usize,
) -> Option<String> {
    if rows.is_empty() || max_items == 0 || max_chars == 0 { return None; }
    // Cap items to max_items (rows are newest first)
    if rows.len() > max_items { rows.truncate(max_items); }

    let mut remaining = max_chars;
    let header = format!("[memory:summary v1 | repo={repo_key}]");
    if header.len() + 1 > remaining { return None; }
    let mut text = String::with_capacity(remaining.min(256));
    text.push_str(&header);
    text.push('\n');
    remaining = remaining.saturating_sub(header.len() + 1);

    for row in rows.into_iter() {
        if remaining == 0 { break; }
        let bullet_full = format!("- {}: {}", row.title, row.text);
        if bullet_full.len() + 1 <= remaining {
            text.push_str(&bullet_full);
            text.push('\n');
            remaining -= bullet_full.len() + 1;
            continue;
        }
        // Need to truncate bullet to fit
        if remaining <= 4 { break; } // can't fit meaningful content
        let slice_len = remaining - 4; // space for " ..."
        let truncated: String = bullet_full.chars().take(slice_len).collect();
        text.push_str(&truncated);
        text.push_str(" ...\n");
        remaining = 0;
        break;
    }

    if text.trim().is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod memory_injection_tests {
    use super::{budget_summaries_to_text, budget_hits_to_text, estimate_tokens_for_items, compute_injection_char_budget, blend_hits_with_recency, blend_hits_with_recency_with_params, dedupe_memory_against_code};
    use crate::memory::store_jsonl::StoredSummary;
    use codex_memory::store::SearchHit;
    use crate::models::{ResponseItem, ContentItem};
    use crate::config::{Config, ConfigOverrides, ConfigToml};
    use crate::client::ModelClient;
    use crate::model_provider_info::built_in_model_providers;
    use crate::openai_tools::ToolsConfig;
    use crate::protocol::AskForApproval;
    use crate::protocol::Event;
    use crate::shell;
    use crate::debug_logger::DebugLogger;
    use tempfile::TempDir;
    use uuid::Uuid;
    use std::sync::{Arc, Mutex};

    fn row(title: &str, text: &str) -> StoredSummary {
        StoredSummary {
            repo_key: "rk".into(),
            session_id: "s".into(),
            ts: 1,
            kind: "summary".into(),
            title: title.into(),
            text: text.into(),
            msg_ids: vec![],
        }
    }

    #[test]
    fn budgets_exact_fit() {
        let rows = vec![row("A", "B")];
        let text = budget_summaries_to_text(rows, "/repo", 2, 100).unwrap();
        assert!(text.contains("[memory:summary v1 | repo=/repo]"));
        assert!(text.contains("- A: B"));
    }

    #[test]
    fn budgets_truncates() {
        let rows = vec![row("Title", &"x".repeat(200))];
        // Very small budget that forces truncation but allows some content
        let text = budget_summaries_to_text(rows, "/repo", 1, 60).unwrap();
        assert!(text.contains("[memory:summary v1 | repo=/repo]"));
        assert!(text.contains("- Title:"));
        assert!(text.contains(" ..."));
    }

    #[test]
    fn budgets_zero_caps_none() {
        let rows = vec![row("t", "u")];
        assert!(budget_summaries_to_text(rows.clone(), "/r", 0, 100).is_none());
        assert!(budget_summaries_to_text(rows, "/r", 1, 0).is_none());
    }

    #[test]
    fn budgets_multi_line_mixed_content() {
        let mixed = "line1\nline2 with more text";
        let rows = vec![row("Mix", mixed)];
        // Small-ish budget; should include header and part of the bullet, preserving newline
        let text = budget_summaries_to_text(rows, "/repo", 1, 60).unwrap();
        assert!(text.contains("[memory:summary v1 | repo=/repo]"));
        assert!(text.contains("- Mix: line1\n"));
    }

    #[test]
    fn budget_hits_exact_and_truncate_and_zero() {
        let hits = vec![
            SearchHit { id: "a".into(), score: 0.9, title: "A".into(), text: "x".repeat(200), ts: 1 },
            SearchHit { id: "b".into(), score: 0.8, title: "B".into(), text: "y".repeat(200), ts: 2 },
        ];

        // Exact-fit-ish generous budget
        let text = budget_hits_to_text(hits.clone(), "/repo", 2, 500).unwrap();
        assert!(text.contains("[memory:retrieval v1 | repo=/repo]"));
        assert!(text.contains("- A:"));
        assert!(text.contains("- B:"));

        // Force truncation
        let tiny = budget_hits_to_text(hits.clone(), "/repo", 1, 60).unwrap();
        assert!(tiny.contains("[memory:retrieval v1 | repo=/repo]"));
        assert!(tiny.contains("- A:"));
        assert!(tiny.contains(" ..."));

        // Zero budgets
        assert!(budget_hits_to_text(Vec::new(), "/repo", 1, 100).is_none());
        assert!(budget_hits_to_text(hits, "/repo", 0, 100).is_none());
    }

    #[test]
    fn budget_hits_header_only_returns_none() {
        let hits = vec![
            SearchHit { id: "a".into(), score: 1.0, title: "A".into(), text: "x".repeat(200), ts: 1 },
        ];
        let header = format!("[memory:retrieval v1 | repo={}]", "/r");
        // Budget that fits header + newline only, but no bullets
        let budget = header.len() + 1;
        assert!(budget_hits_to_text(hits, "/r", 1, budget).is_none());
    }

    #[test]
    fn budget_hits_header_plus_truncated_bullet_returns_some() {
        let hits = vec![
            SearchHit { id: "a".into(), score: 1.0, title: "A".into(), text: "alpha".into(), ts: 1 },
        ];
        let header = format!("[memory:retrieval v1 | repo={}]", "/r");
        // Budget that fits header + newline + a few chars of the bullet (must be >4 to allow truncation)
        let budget = header.len() + 1 + 10;
        let out = budget_hits_to_text(hits, "/r", 1, budget);
        assert!(out.is_some());
        let s = out.unwrap();
        assert!(s.contains("[memory:retrieval v1 | repo=/r]"));
        assert!(s.contains("- A:"));
    }

    #[test]
    fn dedupe_does_not_remove_low_similarity() {
        let code = "[memory:code v1 | repo=/r]\n- Util: alpha beta gamma delta epsilon zeta";
        let mem = "[memory:retrieval v1 | repo=/r]\n- Util: alpha xi omicron\n- Other: different content";
        let out = dedupe_memory_against_code(&code, &mem);
        // Because content similarity is low (few overlapping tokens), Util should be kept
        assert!(out.contains("- Util:"));
        assert!(out.contains("- Other:"));
    }

    #[test]
    fn dedupe_content_threshold_controls_removal() {
        // Same title, content shares 3/5 tokens (0.6 Jaccard) without containment
        let code = "[memory:code v1 | repo=/r]\n- TitleA: alpha beta gamma delta";
        let mem  = "[memory:retrieval v1 | repo=/r]\n- TitleA: alpha beta gamma epsilon";

        // High threshold: keep
        let keep = super::dedupe_memory_against_code_with_thresholds(code, mem, 0.99, 0.8, 64);
        assert!(keep.contains("- TitleA:"));

        // Low threshold: remove (0.6 >= 0.6)
        let removed = super::dedupe_memory_against_code_with_thresholds(code, mem, 0.99, 0.6, 64);
        assert!(!removed.contains("- TitleA:"));
    }

    #[test]
    fn dedupe_containment_prefix_controls_short_overlap() {
        // No full containment but first 10 chars overlap strongly
        let code = "[memory:code v1 | repo=/r]\n- T: lorem ipsum dolor";
        let mem  = "[memory:retrieval v1 | repo=/r]\n- U: lorem ipsZZZ";

        // Require long prefix -> keep
        let keep = super::dedupe_memory_against_code_with_thresholds(code, mem, 1.0, 1.0, 16);
        assert!(keep.contains("- U:"));

        // Short prefix allows containment -> remove
        let removed = super::dedupe_memory_against_code_with_thresholds(code, mem, 1.0, 1.0, 5);
        assert!(!removed.contains("- U:"));
    }

    #[test]
    fn estimate_tokens_sanity() {
        let items = vec![
            ResponseItem::Message { id: None, role: "user".into(), content: vec![
                ContentItem::InputText { text: "abcd".into() },
                ContentItem::OutputText { text: "12345678".into() },
            ]},
            ResponseItem::Message { id: None, role: "assistant".into(), content: vec![
                ContentItem::InputText { text: "".into() },
            ]},
        ];
        // 4 + 8 = 12 chars -> ~3 tokens with ceil rounding
        assert_eq!(estimate_tokens_for_items(&items), 3);

        let empty: Vec<ResponseItem> = Vec::new();
        assert_eq!(estimate_tokens_for_items(&empty), 0);
    }

    #[test]
    fn compute_budget_caps_and_zero() {
        // Build a minimal Session with specific window/output settings
        let tmp = TempDir::new().unwrap();
        let mut base = ConfigToml::default();
        base.model_context_window = Some(10_000);
        base.model_max_output_tokens = Some(1_000);
        let cfg = Config::load_from_base_config_with_overrides(
            base,
            ConfigOverrides::default(),
            tmp.path().to_path_buf(),
        ).expect("config load");

        let debug_logger = Arc::new(Mutex::new(DebugLogger::new(false).unwrap()));
        let provider = built_in_model_providers().get(&cfg.model_provider_id).unwrap().clone();
        let client = ModelClient::new(
            Arc::new(cfg.clone()),
            None,
            provider,
            cfg.model_reasoning_effort,
            cfg.model_reasoning_summary,
            cfg.model_text_verbosity,
            Uuid::new_v4(),
            debug_logger,
        );

        let (tx_event, _) = async_channel::unbounded::<Event>();
        let sess = super::Session {
            client,
            tx_event,
            cwd: tmp.path().to_path_buf(),
            base_instructions: None,
            user_instructions: None,
            approval_policy: AskForApproval::Never,
            sandbox_policy: cfg.sandbox_policy.clone(),
            shell_environment_policy: cfg.shell_environment_policy.clone(),
            writable_roots: vec![],
            disable_response_storage: cfg.disable_response_storage,
            tools_config: ToolsConfig::new(&cfg.model_family, cfg.approval_policy, cfg.sandbox_policy.clone(), cfg.include_plan_tool),
            mcp_connection_manager: super::McpConnectionManager::default(),
            agents: cfg.agents.clone(),
            notify: cfg.notify.clone(),
            state: std::sync::Mutex::new(super::State::default()),
            rollout: std::sync::Mutex::new(None),
            codex_linux_sandbox_exe: None,
            user_shell: shell::Shell::Unknown,
            show_raw_agent_reasoning: false,
            pending_browser_screenshots: std::sync::Mutex::new(Vec::new()),
            last_system_status: std::sync::Mutex::new(None),
            last_screenshot_info: std::sync::Mutex::new(None),
        };

        // Input of ~100 tokens → window 10k, reserve 1k, safety 2k, cap 10% (1k)
        let turn_input = vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText { text: "x".repeat(400) }],
        }];
        let budget = compute_injection_char_budget(&sess, &turn_input);
        // Hard cap should win: 1k tokens → 4k chars
        assert_eq!(budget, 4_000);

        // Small window that results in zero available
        let mut base2 = ConfigToml::default();
        base2.model_context_window = Some(1_000);
        base2.model_max_output_tokens = Some(900);
        let cfg2 = Config::load_from_base_config_with_overrides(
            base2,
            ConfigOverrides::default(),
            tmp.path().to_path_buf(),
        ).expect("config load");
        let debug_logger2 = Arc::new(Mutex::new(DebugLogger::new(false).unwrap()));
        let provider2 = built_in_model_providers().get(&cfg2.model_provider_id).unwrap().clone();
        let client2 = ModelClient::new(
            Arc::new(cfg2.clone()),
            None,
            provider2,
            cfg2.model_reasoning_effort,
            cfg2.model_reasoning_summary,
            cfg2.model_text_verbosity,
            Uuid::new_v4(),
            debug_logger2,
        );
        let (tx_event2, _) = async_channel::unbounded::<Event>();
        let sess2 = super::Session {
            client: client2,
            tx_event: tx_event2,
            cwd: tmp.path().to_path_buf(),
            base_instructions: None,
            user_instructions: None,
            approval_policy: AskForApproval::Never,
            sandbox_policy: cfg2.sandbox_policy.clone(),
            shell_environment_policy: cfg2.shell_environment_policy.clone(),
            writable_roots: vec![],
            disable_response_storage: cfg2.disable_response_storage,
            tools_config: ToolsConfig::new(&cfg2.model_family, cfg2.approval_policy, cfg2.sandbox_policy.clone(), cfg2.include_plan_tool),
            mcp_connection_manager: super::McpConnectionManager::default(),
            agents: cfg2.agents.clone(),
            notify: cfg2.notify.clone(),
            state: std::sync::Mutex::new(super::State::default()),
            rollout: std::sync::Mutex::new(None),
            codex_linux_sandbox_exe: None,
            user_shell: shell::Shell::Unknown,
            show_raw_agent_reasoning: false,
            pending_browser_screenshots: std::sync::Mutex::new(Vec::new()),
            last_system_status: std::sync::Mutex::new(None),
            last_screenshot_info: std::sync::Mutex::new(None),
        };
        let budget2 = compute_injection_char_budget(&sess2, &turn_input);
        assert_eq!(budget2, 0);
    }

    #[test]
    fn blend_recency_prefers_newer_when_scores_equal() {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let mut hits = vec![
            SearchHit { id: "old".into(), score: 0.5, title: "t".into(), text: "x".into(), ts: now_ms - 30 * 24 * 60 * 60 * 1000 },
            SearchHit { id: "new".into(), score: 0.5, title: "t".into(), text: "x".into(), ts: now_ms },
        ];
        blend_hits_with_recency(&mut hits);
        assert_eq!(hits[0].id, "new");
    }

    #[test]
    fn dedupe_memory_against_code_removes_overlap() {
        let common = "abcdefghijklmnopqrstuvwxyz0123456789"; // 36 chars common prefix (> min 32)
        let code = format!("[memory:code v1 | repo=/r]\n- Foo: {common} more code");
        let mem = format!("[memory:retrieval v1 | repo=/r]\n- Foo: {common} and memory text\n- Bar: different content");
        let out = dedupe_memory_against_code(&code, &mem);
        assert!(out.contains("[memory:retrieval v1 | repo=/r]"));
        // Overlapping Foo bullet should be removed, Bar remains
        assert!(!out.contains("- Foo:"));
        assert!(out.contains("- Bar:"));
    }
    
    #[test]
    fn recency_alpha_shifts_ranking() {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let base = vec![
            SearchHit { id: "old".into(), score: 0.60, title: "t".into(), text: "x".into(), ts: now_ms - 40 * 24 * 60 * 60 * 1000 },
            SearchHit { id: "new".into(), score: 0.59, title: "t".into(), text: "x".into(), ts: now_ms },
        ];
        let mut a0 = base.clone();
        blend_hits_with_recency_with_params(&mut a0, 0.0, 7.0);
        assert_eq!(a0[0].id, "old");

        let mut a1 = base.clone();
        blend_hits_with_recency_with_params(&mut a1, 0.3, 7.0);
        assert_eq!(a1[0].id, "new");
    }

    #[test]
    fn recency_half_life_controls_decay() {
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let base = vec![
            SearchHit { id: "old".into(), score: 0.62, title: "t".into(), text: "x".into(), ts: now_ms - 60 * 24 * 60 * 60 * 1000 },
            SearchHit { id: "new".into(), score: 0.60, title: "t".into(), text: "x".into(), ts: now_ms },
        ];
        let mut short = base.clone();
        blend_hits_with_recency_with_params(&mut short, 0.4, 3.0);
        assert_eq!(short[0].id, "new");

        let mut long = base.clone();
        blend_hits_with_recency_with_params(&mut long, 0.1, 1000.0);
        assert_eq!(long[0].id, "old");
    }
}

/// When the model is prompted, it returns a stream of events. Some of these
/// events map to a `ResponseItem`. A `ResponseItem` may need to be
/// "handled" such that it produces a `ResponseInputItem` that needs to be
/// sent back to the model on the next turn.
#[derive(Debug)]
struct ProcessedResponseItem {
    item: ResponseItem,
    response: Option<ResponseInputItem>,
}

async fn try_run_turn(
    sess: &Session,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: &str,
    prompt: &Prompt,
) -> CodexResult<Vec<ProcessedResponseItem>> {
    // call_ids that are part of this response.
    let completed_call_ids = prompt
        .input
        .iter()
        .filter_map(|ri| match ri {
            ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id),
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => Some(call_id),
            _ => None,
        })
        .collect::<Vec<_>>();

    // call_ids that were pending but are not part of this response.
    // This usually happens because the user interrupted the model before we responded to one of its tool calls
    // and then the user sent a follow-up message.
    let missing_calls = {
        prompt
            .input
            .iter()
            .filter_map(|ri| match ri {
                ResponseItem::FunctionCall { call_id, .. } => Some(call_id),
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => Some(call_id),
                _ => None,
            })
            .filter_map(|call_id| {
                if completed_call_ids.contains(&call_id) {
                    None
                } else {
                    Some(call_id.clone())
                }
            })
            .map(|call_id| ResponseItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: FunctionCallOutputPayload {
                    content: "aborted".to_string(),
                    success: Some(false),
                },
            })
            .collect::<Vec<_>>()
    };
    let prompt: Cow<Prompt> = if missing_calls.is_empty() {
        Cow::Borrowed(prompt)
    } else {
        // Add the synthetic aborted missing calls to the beginning of the input to ensure all call ids have responses.
        let input = [missing_calls, prompt.input.clone()].concat();
        Cow::Owned(Prompt {
            input,
            ..prompt.clone()
        })
    };

    // Apply preflight compaction if needed to ensure prompt fits in context window
    let prompt: Cow<Prompt> = match preflight_compact_if_needed(sess, &prompt) {
        std::borrow::Cow::Borrowed(_) => prompt,
        std::borrow::Cow::Owned(p2) => Cow::Owned(p2),
    };

    let mut stream = sess.client.clone().stream(&prompt).await?;

    let mut output = Vec::new();
    loop {
        // Poll the next item from the model stream. We must inspect *both* Ok and Err
        // cases so that transient stream failures (e.g., dropped SSE connection before
        // `response.completed`) bubble up and trigger the caller's retry logic.
        let event = stream.next().await;
        let Some(event) = event else {
            // Channel closed without yielding a final Completed event or explicit error.
            // Treat as a disconnected stream so the caller can retry.
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };

        let event = match event {
            Ok(ev) => ev,
            Err(e) => {
                // Propagate the underlying stream error to the caller (run_turn), which
                // will apply the configured `stream_max_retries` policy.
                return Err(e);
            }
        };

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(item) => {
                let response =
                    handle_response_item(sess, turn_diff_tracker, sub_id, item.clone()).await?;

                output.push(ProcessedResponseItem { item, response });
            }
            ResponseEvent::Completed {
                response_id: _,
                token_usage,
            } => {
                if let Some(token_usage) = token_usage {
                    // Remember last completed token usage for baseline in post-prune updates
                    {
                        let mut st = sess.state.lock().unwrap();
                        st.last_completed_token_usage = Some(token_usage.clone());
                    }
                    sess.tx_event
                        .send(Event {
                            id: sub_id.to_string(),
                            msg: EventMsg::TokenCount(token_usage),
                        })
                        .await
                        .ok();
                }
                
                let unified_diff = turn_diff_tracker.get_unified_diff();
                if let Ok(Some(unified_diff)) = unified_diff {
                    let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
                    let event = Event {
                        id: sub_id.to_string(),
                        msg,
                    };
                    let _ = sess.tx_event.send(event).await;
                }

                return Ok(output);
            }
            ResponseEvent::OutputTextDelta { delta, item_id } => {
                // Don't append to history during streaming - only send UI events.
                // The complete message will be added to history when OutputItemDone arrives.
                // This ensures items are recorded in the correct chronological order.

                // Use the item_id if present, otherwise fall back to sub_id
                let event_id = item_id.unwrap_or_else(|| sub_id.to_string());
                let event = Event {
                    id: event_id,
                    msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { delta }),
                };
                sess.tx_event.send(event).await.ok();
            }
            ResponseEvent::ReasoningSummaryDelta { delta, item_id } => {
                // Use the item_id if present, otherwise fall back to sub_id
                let event_id = item_id.unwrap_or_else(|| sub_id.to_string());
                let event = Event {
                    id: event_id,
                    msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent { delta }),
                };
                sess.tx_event.send(event).await.ok();
            }
            ResponseEvent::ReasoningSummaryPartAdded => {
                let event = Event {
                    id: sub_id.to_string(),
                    msg: EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {}),
                };
                sess.tx_event.send(event).await.ok();
            }
            ResponseEvent::ReasoningContentDelta { delta, item_id } => {
                if sess.show_raw_agent_reasoning {
                    // Use the item_id if present, otherwise fall back to sub_id
                    let event_id = item_id.unwrap_or_else(|| sub_id.to_string());
                    let event = Event {
                        id: event_id,
                        msg: EventMsg::AgentReasoningRawContentDelta(
                            AgentReasoningRawContentDeltaEvent { delta },
                        ),
                    };
                    sess.tx_event.send(event).await.ok();
                }
            }
        }
    }
}

/// Ensure the formatted request fits in the model's context window.
/// If it exceeds the budget, replace older history with a compact summary and keep
/// a small tail of recent messages. Does not mutate persistent history; only the prompt.
fn preflight_compact_if_needed<'a>(sess: &Session, prompt: &'a Prompt) -> std::borrow::Cow<'a, Prompt> {
    // Compute effective budget
    let window_tokens = sess.client.get_model_context_window().unwrap_or(128_000);
    let reserve_output = sess.client.get_model_max_output_tokens().unwrap_or(1_024);
    let safety_margin_tokens: u64 = 2_000;
    let effective_window = window_tokens.saturating_sub(reserve_output).saturating_sub(safety_margin_tokens);
    let hard_limit = effective_window;

    // Fast path – already fits
    let mut formatted = prompt.get_formatted_input();
    let mut est = estimate_tokens_for_items(&formatted) as u64;
    let mem_cfg = sess.client.get_memory_config();
    let thresholds = if mem_cfg.compact_threshold_pct.is_empty() { vec![75,85,95] } else { mem_cfg.compact_threshold_pct.clone() };
    let target_pct: u64 = mem_cfg.compact_target_pct as u64;
    let used_pct = ((est as f64) / (effective_window as f64) * 100.0) as u64;
    if est <= hard_limit && used_pct < thresholds.iter().min().copied().unwrap_or(75) as u64 {
        return std::borrow::Cow::Borrowed(prompt);
    }

    // Work on a clone we can modify
    let mut p = prompt.clone();

    // If the first item is a memory/code injection block, keep it separate so we can drop it as a last resort.
    let (mut injection_prefix, mut rest): (Vec<ResponseItem>, Vec<ResponseItem>) = match p.input.split_first() {
        Some((ResponseItem::Message { content, .. }, tail)) if content.iter().any(|c| matches!(c, ContentItem::InputText { text } if text.starts_with("[memory:"))) => {
            (vec![p.input[0].clone()], tail.to_vec())
        }
        _ => (Vec::new(), p.input.clone()),
    };

    // Protect recent volleys: derive a small count from keep_last_messages (message-based).
    let mut protect_tail_volleys: usize = (mem_cfg.keep_last_messages / 2).clamp(1, 5);
    let repo_key = crate::util::repo_key(&sess.cwd);
    let mut summary_max_chars: usize = mem_cfg.summary_max_chars_per_volley.max(200);
    let mut injection_dropped = false;
    let max_summaries = mem_cfg.max_summaries_per_request.max(1);
    let mut summaries_inserted = 0usize;

    // Loop with safety bound
    for _ in 0..64 {
        // Build volley candidates on the current rest
        let volleys = segment_into_volleys(&rest);
        if volleys.is_empty() { break; }

        // Exclude the last N volleys from compaction
        let protect_start_item_idx = if protect_tail_volleys >= volleys.len() { 0 } else { volleys[volleys.len() - protect_tail_volleys].start };
        let mut cands = filter_compaction_candidates(&rest, &volleys)
            .into_iter()
            .filter(|r| r.start < protect_start_item_idx)
            .collect::<Vec<_>>();

        if cands.is_empty() {
            // Reduce protection first, then shrink summary, then drop injection
            if protect_tail_volleys > 1 { protect_tail_volleys -= 1; continue; }
            if summary_max_chars > 200 { summary_max_chars = ((summary_max_chars as f32) * 0.7) as usize; summary_max_chars = summary_max_chars.max(200); continue; }
            if !injection_dropped && !injection_prefix.is_empty() {
                injection_prefix.clear();
                injection_dropped = true;
                // Rebuild p.input to reflect injection removal
                p.input = rest.clone();
                formatted = p.get_formatted_input();
                est = estimate_tokens_for_items(&formatted) as u64;
                if est <= hard_limit { break; }
                continue;
            }
            break;
        }

        // Summarize the oldest candidate volley
        let r = cands.remove(0);
        let slice = &rest[r.start..r.end];
        let summarizer = CompactSummarizer::new(summary_max_chars);
        if let Some(summary) = summarizer.summarize(slice) {
            let header = format!("[memory:context v1 | repo={repo_key}]");
            let text = format!("{header}\n{}\n{}", summary.title, summary.text);
            let summary_item = ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText { text }],
            };
            // Replace the slice with a single summary item
            let mut new_rest: Vec<ResponseItem> = Vec::with_capacity(rest.len() - (r.end - r.start) + 1);
            new_rest.extend_from_slice(&rest[..r.start]);
            new_rest.push(summary_item);
            new_rest.extend_from_slice(&rest[r.end..]);
            rest = new_rest;

            // Rebuild prompt input
            if injection_prefix.is_empty() { p.input = rest.clone(); } else { p.input = [injection_prefix.clone(), rest.clone()].concat(); }

            // Re‑estimate
            formatted = p.get_formatted_input();
            est = estimate_tokens_for_items(&formatted) as u64;
            summaries_inserted = summaries_inserted.saturating_add(1);
            let used_pct = ((est as f64) / (effective_window as f64) * 100.0) as u64;
            let target_tokens = (target_pct.min(99) as f64 / 100.0 * effective_window as f64) as u64;
            if est <= target_tokens || used_pct < thresholds.iter().min().copied().unwrap_or(75) as u64 { break; }
            if summaries_inserted >= max_summaries { break; }
            continue;
        } else {
            // If summarizer failed, drop the slice as a last resort to avoid infinite loop
            let mut new_rest: Vec<ResponseItem> = Vec::with_capacity(rest.len() - (r.end - r.start));
            new_rest.extend_from_slice(&rest[..r.start]);
            new_rest.extend_from_slice(&rest[r.end..]);
            rest = new_rest;
            if injection_prefix.is_empty() { p.input = rest.clone(); } else { p.input = [injection_prefix.clone(), rest.clone()].concat(); }
            formatted = p.get_formatted_input();
            est = estimate_tokens_for_items(&formatted) as u64;
            let used_pct = ((est as f64) / (effective_window as f64) * 100.0) as u64;
            let target_tokens = (target_pct.min(99) as f64 / 100.0 * effective_window as f64) as u64;
            if est <= target_tokens || used_pct < thresholds.iter().min().copied().unwrap_or(75) as u64 { break; }
            continue;
        }
    }

    std::borrow::Cow::Owned(p)
}

async fn run_compact_agent(
    sess: Arc<Session>,
    sub_id: String,
    input: Vec<InputItem>,
    compact_instructions: String,
) {
    let start_event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskStarted,
    };
    if sess.tx_event.send(start_event).await.is_err() {
        return;
    }

    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input);
    let turn_input: Vec<ResponseItem> =
        sess.turn_input_with_history(vec![initial_input_for_turn.clone().into()]);

    let max_retries = sess.client.get_provider().stream_max_retries();
    let mut retries = 0;

    loop {
        // Build status items (screenshots, system status) fresh for each attempt
        let status_items = build_turn_status_items(&sess).await;

        let prompt = Prompt {
            input: turn_input.clone(),
            user_instructions: None,
            store: !sess.disable_response_storage,
            environment_context: None,
            tools: Vec::new(),
            base_instructions_override: Some(compact_instructions.clone()),
            status_items, // Include status items with this request
        };

        let attempt_result = drain_to_completed(&sess, &sub_id, &prompt).await;

        match attempt_result {
            Ok(()) => {
                // Record status items to conversation history after successful turn
                if !prompt.status_items.is_empty() {
                    sess.record_conversation_items(&prompt.status_items).await;
                }
                break;
            }
            Err(CodexErr::Interrupted) => return,
            Err(e) => {
                if retries < max_retries {
                    retries += 1;
                    let delay = backoff(retries);
                    sess.notify_background_event(
                        &sub_id,
                        format!(
                            "stream error: {e}; retrying {retries}/{max_retries} in {delay:?}…"
                        ),
                    )
                    .await;
                    tokio::time::sleep(delay).await;
                    continue;
                } else {
                    let event = Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: e.to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                    return;
                }
            }
        }
    }

    sess.remove_agent(&sub_id);
    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Compact agent completed".to_string(),
        }),
    };
    sess.send_event(event).await;
    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskComplete(TaskCompleteEvent {
            last_agent_message: None,
        }),
    };
    sess.send_event(event).await;

    let mut state = sess.state.lock().unwrap();
    state.history.keep_last_messages(1);
}

async fn handle_response_item(
    sess: &Session,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: &str,
    item: ResponseItem,
) -> CodexResult<Option<ResponseInputItem>> {
    debug!(?item, "Output item");
    let output = match item {
        ResponseItem::Message { content, id, .. } => {
            // Use the item_id if present, otherwise fall back to sub_id
            let event_id = id.unwrap_or_else(|| sub_id.to_string());
            for item in content {
                if let ContentItem::OutputText { text } = item {
                    let event = Event {
                        id: event_id.clone(),
                        msg: EventMsg::AgentMessage(AgentMessageEvent { message: text }),
                    };
                    sess.tx_event.send(event).await.ok();
                }
            }
            None
        }
        ResponseItem::Reasoning {
            id,
            summary,
            content,
            encrypted_content: _,
        } => {
            // Use the item_id if present and not empty, otherwise fall back to sub_id
            let event_id = if !id.is_empty() {
                id.clone()
            } else {
                sub_id.to_string()
            };
            for item in summary {
                let text = match item {
                    ReasoningItemReasoningSummary::SummaryText { text } => text,
                };
                let event = Event {
                    id: event_id.clone(),
                    msg: EventMsg::AgentReasoning(AgentReasoningEvent { text }),
                };
                sess.tx_event.send(event).await.ok();
            }
            if sess.show_raw_agent_reasoning && content.is_some() {
                let content = content.unwrap();
                for item in content {
                    let text = match item {
                        ReasoningItemContent::ReasoningText { text } => text,
                        ReasoningItemContent::Text { text } => text,
                    };
                    let event = Event {
                        id: event_id.clone(),
                        msg: EventMsg::AgentReasoningRawContent(AgentReasoningRawContentEvent {
                            text,
                        }),
                    };
                    sess.tx_event.send(event).await.ok();
                }
            }
            None
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => {
            info!("FunctionCall: {arguments}");
            Some(
                handle_function_call(
                    sess,
                    turn_diff_tracker,
                    sub_id.to_string(),
                    name,
                    arguments,
                    call_id,
                )
                .await,
            )
        }
        ResponseItem::LocalShellCall {
            id,
            call_id,
            status: _,
            action,
        } => {
            let LocalShellAction::Exec(action) = action;
            tracing::info!("LocalShellCall: {action:?}");
            let params = ShellToolCallParams {
                command: action.command,
                workdir: action.working_directory,
                timeout_ms: action.timeout_ms,
                with_escalated_permissions: None,
                justification: None,
            };
            let effective_call_id = match (call_id, id) {
                (Some(call_id), _) => call_id,
                (None, Some(id)) => id,
                (None, None) => {
                    error!("LocalShellCall without call_id or id");
                    return Ok(Some(ResponseInputItem::FunctionCallOutput {
                        call_id: "".to_string(),
                        output: FunctionCallOutputPayload {
                            content: "LocalShellCall without call_id or id".to_string(),
                            success: None,
                        },
                    }));
                }
            };

            let exec_params = to_exec_params(params, sess);
            Some(
                handle_container_exec_with_params(
                    exec_params,
                    sess,
                    turn_diff_tracker,
                    sub_id.to_string(),
                    effective_call_id,
                )
                .await,
            )
        }
        ResponseItem::FunctionCallOutput { .. } => {
            debug!("unexpected FunctionCallOutput from stream");
            None
        }
        ResponseItem::Other => None,
    };
    Ok(output)
}

// Helper utilities for agent output/progress management
fn ensure_agent_dir(cwd: &Path, agent_id: &str) -> Result<PathBuf, String> {
    let dir = cwd.join(".code").join("agents").join(agent_id);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create agent dir {}: {}", dir.display(), e))?;
    Ok(dir)
}

fn write_agent_file(dir: &Path, filename: &str, content: &str) -> Result<PathBuf, String> {
    let path = dir.join(filename);
    std::fs::write(&path, content)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    Ok(path)
}

fn preview_first_n_lines(s: &str, n: usize) -> (String, usize) {
    let mut lines = s.lines();
    let mut collected: Vec<&str> = Vec::new();
    for _ in 0..n {
        if let Some(l) = lines.next() {
            collected.push(l);
        } else {
            break;
        }
    }
    (collected.join("\n"), s.lines().count())
}

async fn handle_function_call(
    sess: &Session,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    name: String,
    arguments: String,
    call_id: String,
) -> ResponseInputItem {
    match name.as_str() {
        "container.exec" | "shell" => {
            let params = match parse_container_exec_arguments(arguments, sess, &call_id) {
                Ok(params) => params,
                Err(output) => {
                    return *output;
                }
            };
            handle_container_exec_with_params(params, sess, turn_diff_tracker, sub_id, call_id)
                .await
        }
        "update_plan" => handle_update_plan(sess, arguments, sub_id, call_id).await,
        // agent_* tools
        "agent_run" => handle_run_agent(sess, arguments, sub_id, call_id).await,
        "agent_check" => handle_check_agent_status(sess, arguments, sub_id, call_id).await,
        "agent_result" => handle_get_agent_result(sess, arguments, sub_id, call_id).await,
        "agent_cancel" => handle_cancel_agent(sess, arguments, sub_id, call_id).await,
        "agent_wait" => handle_wait_for_agent(sess, arguments, sub_id, call_id).await,
        "agent_list" => handle_list_agents(sess, arguments, sub_id, call_id).await,
        // browser_* tools
        "browser_open" => handle_browser_open(sess, arguments, sub_id, call_id).await,
        "browser_close" => handle_browser_close(sess, sub_id, call_id).await,
        "browser_status" => handle_browser_status(sess, sub_id, call_id).await,
        "browser_click" => handle_browser_click(sess, arguments, sub_id, call_id).await,
        "browser_move" => handle_browser_move(sess, arguments, sub_id, call_id).await,
        "browser_type" => handle_browser_type(sess, arguments, sub_id, call_id).await,
        "browser_key" => handle_browser_key(sess, arguments, sub_id, call_id).await,
        "browser_javascript" => handle_browser_javascript(sess, arguments, sub_id, call_id).await,
        "browser_scroll" => handle_browser_scroll(sess, arguments, sub_id, call_id).await,
        "browser_history" => handle_browser_history(sess, arguments, sub_id, call_id).await,
        "browser_console" => handle_browser_console(sess, arguments, sub_id, call_id).await,
        "browser_inspect" => handle_browser_inspect(sess, arguments, sub_id, call_id).await,
        "browser_cdp" => handle_browser_cdp(sess, arguments, sub_id, call_id).await,
        "browser_cleanup" => handle_browser_cleanup(sess, sub_id, call_id).await,
        _ => {
            match sess.mcp_connection_manager.parse_tool_name(&name) {
                Some((server, tool_name)) => {
                    // TODO(mbolin): Determine appropriate timeout for tool call.
                    let timeout = None;
                    handle_mcp_tool_call(
                        sess, &sub_id, call_id, server, tool_name, arguments, timeout,
                    )
                    .await
                }
                None => {
                    // Unknown function: reply with structured failure so the model can adapt.
                    ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("unsupported call: {name}"),
                            success: None,
                        },
                    }
                }
            }
        }
    }
}

async fn handle_browser_cleanup(
    sess: &Session,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let sess_clone = sess;
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_cleanup".to_string(),
        Some(serde_json::json!({})),
        || async move {
            if let Some(browser_manager) = get_browser_manager_for_session(sess_clone).await {
                match browser_manager.cleanup().await {
                    Ok(_) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload { content: "Browser cleanup completed".to_string(), success: Some(true) },
                    },
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload { content: format!("Cleanup failed: {}", e), success: Some(false) },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload { content: "Browser is not initialized. Use browser_open to start the browser.".to_string(), success: Some(false) },
                }
            }
        }
    ).await
}

fn to_exec_params(params: ShellToolCallParams, sess: &Session) -> ExecParams {
    ExecParams {
        command: params.command,
        cwd: sess.resolve_path(params.workdir.clone()),
        timeout_ms: params.timeout_ms,
        env: create_env(&sess.shell_environment_policy),
        with_escalated_permissions: params.with_escalated_permissions,
        justification: params.justification,
    }
}

fn parse_container_exec_arguments(
    arguments: String,
    sess: &Session,
    call_id: &str,
) -> Result<ExecParams, Box<ResponseInputItem>> {
    // parse command
    match serde_json::from_str::<ShellToolCallParams>(&arguments) {
        Ok(shell_tool_call_params) => Ok(to_exec_params(shell_tool_call_params, sess)),
        Err(e) => {
            // allow model to re-sample
            let output = ResponseInputItem::FunctionCallOutput {
                call_id: call_id.to_string(),
                output: FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: None,
                },
            };
            Err(Box::new(output))
        }
    }
}

pub struct ExecInvokeArgs<'a> {
    pub params: ExecParams,
    pub sandbox_type: SandboxType,
    pub sandbox_policy: &'a SandboxPolicy,
    pub codex_linux_sandbox_exe: &'a Option<PathBuf>,
    pub stdout_stream: Option<StdoutStream>,
}

fn maybe_run_with_user_profile(params: ExecParams, sess: &Session) -> ExecParams {
    if sess.shell_environment_policy.use_profile {
        let maybe_command = sess
            .user_shell
            .format_default_shell_invocation(params.command.clone());
        if let Some(command) = maybe_command {
            return ExecParams { command, ..params };
        }
    }
    params
}

async fn handle_run_agent(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_run".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<RunAgentParams>(&arguments_clone) {
        Ok(params) => {
            let mut manager = AGENT_MANAGER.write().await;

            // Handle model parameter (can be string or array)
            let models = match params.model {
                Some(serde_json::Value::String(model)) => vec![model],
                Some(serde_json::Value::Array(models)) => models
                    .into_iter()
                    .filter_map(|m| m.as_str().map(String::from))
                    .collect(),
                _ => vec!["codex".to_string()], // Default model
            };

            let batch_id = if models.len() > 1 {
                Some(Uuid::new_v4().to_string())
            } else {
                None
            };

            let mut agent_ids = Vec::new();
            for model in models {
                // Check if this model is configured and enabled
                let agent_config = sess.agents.iter().find(|a| {
                    a.name.to_lowercase() == model.to_lowercase()
                        || a.command.to_lowercase() == model.to_lowercase()
                });

                if let Some(config) = agent_config {
                    if !config.enabled {
                        continue; // Skip disabled agents
                    }

                    // Override read_only if agent is configured as read-only
                    let read_only = config.read_only || params.read_only.unwrap_or(false);

                    let agent_id = manager
                        .create_agent_with_config(
                            model,
                            params.task.clone(),
                            params.context.clone(),
                            params.output.clone(),
                            params.files.clone().unwrap_or_default(),
                            read_only,
                            batch_id.clone(),
                            config.clone(),
                        )
                        .await;
                    agent_ids.push(agent_id);
                } else {
                    // Use default configuration for unknown agents
                    let agent_id = manager
                        .create_agent(
                            model,
                            params.task.clone(),
                            params.context.clone(),
                            params.output.clone(),
                            params.files.clone().unwrap_or_default(),
                            params.read_only.unwrap_or(false),
                            batch_id.clone(),
                        )
                        .await;
                    agent_ids.push(agent_id);
                }
            }

            // Send agent status update event
            drop(manager); // Release the write lock first
            if agent_ids.len() > 0 {
                send_agent_status_update(sess).await;
            }

            let response = if let Some(batch_id) = batch_id {
                serde_json::json!({
                    "batch_id": batch_id,
                    "agent_ids": agent_ids,
                    "status": "started",
                    "message": format!("Started {} agents", agent_ids.len())
                })
            } else {
                serde_json::json!({
                    "agent_id": agent_ids[0],
                    "status": "started",
                    "message": "Agent started successfully"
                })
            };

            ResponseInputItem::FunctionCallOutput {
                call_id: call_id_clone,
                output: FunctionCallOutputPayload {
                    content: response.to_string(),
                    success: Some(true),
                },
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid agent_run arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_check_agent_status(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_check".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<CheckAgentStatusParams>(&arguments_clone) {
        Ok(params) => {
            let manager = AGENT_MANAGER.read().await;

            if let Some(agent) = manager.get_agent(&params.agent_id) {
                // Limit progress in the response; write full progress to file if large
                let max_progress_lines = 50usize;
                let total_progress = agent.progress.len();
                let progress_preview: Vec<String> = if total_progress > max_progress_lines {
                    agent
                        .progress
                        .iter()
                        .skip(total_progress - max_progress_lines)
                        .cloned()
                        .collect()
                } else {
                    agent.progress.clone()
                };

                let mut progress_file: Option<String> = None;
                if total_progress > max_progress_lines {
                    let cwd = sess.get_cwd().to_path_buf();
                    drop(manager);
                    let dir = match ensure_agent_dir(&cwd, &agent.id) {
                        Ok(d) => d,
                        Err(e) => {
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to prepare agent progress file: {}", e),
                                    success: Some(false),
                                },
                            };
                        }
                    };
                    // Re-acquire manager to get fresh progress after potential delay
                    let manager = AGENT_MANAGER.read().await;
                    if let Some(agent) = manager.get_agent(&params.agent_id) {
                        let joined = agent.progress.join("\n");
                        match write_agent_file(&dir, "progress.log", &joined) {
                            Ok(p) => progress_file = Some(p.display().to_string()),
                            Err(e) => {
                                return ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone,
                                    output: FunctionCallOutputPayload {
                                        content: format!("Failed to write progress file: {}", e),
                                        success: Some(false),
                                    },
                                };
                            }
                        }
                    }
                } else {
                    drop(manager);
                }

                let response = serde_json::json!({
                    "agent_id": params.agent_id,
                    "status": agent.status,
                    "model": agent.model,
                    "created_at": agent.created_at,
                    "started_at": agent.started_at,
                    "completed_at": agent.completed_at,
                    "progress_preview": progress_preview,
                    "progress_total": total_progress,
                    "progress_file": progress_file,
                    "error": agent.error,
                    "worktree_path": agent.worktree_path,
                    "branch_name": agent.branch_name,
                });

                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: response.to_string(),
                        success: Some(true),
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: format!("Agent not found: {}", params.agent_id),
                        success: Some(false),
                    },
                }
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid agent_check arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_get_agent_result(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_result".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<GetAgentResultParams>(&arguments_clone) {
        Ok(params) => {
            let manager = AGENT_MANAGER.read().await;

            if let Some(agent) = manager.get_agent(&params.agent_id) {
                let cwd = sess.get_cwd().to_path_buf();
                let dir = match ensure_agent_dir(&cwd, &params.agent_id) {
                    Ok(d) => d,
                    Err(e) => {
                        return ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone,
                            output: FunctionCallOutputPayload {
                                content: format!("Failed to prepare agent output dir: {}", e),
                                success: Some(false),
                            },
                        };
                    }
                };

                match agent.status {
                    AgentStatus::Completed => {
                        let output_text = agent.result.unwrap_or_default();
                        let (preview, total_lines) = preview_first_n_lines(&output_text, 500);
                        let file_path = match write_agent_file(&dir, "result.txt", &output_text) {
                            Ok(p) => p.display().to_string(),
                            Err(e) => format!("Failed to write result file: {}", e),
                        };
                        let response = serde_json::json!({
                            "agent_id": params.agent_id,
                            "status": agent.status,
                            "output_preview": preview,
                            "output_total_lines": total_lines,
                            "output_file": file_path,
                        });
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone,
                            output: FunctionCallOutputPayload {
                                content: response.to_string(),
                                success: Some(true),
                            },
                        }
                    }
                    AgentStatus::Failed => {
                        let error_text = agent.error.unwrap_or_else(|| "Unknown error".to_string());
                        let (preview, total_lines) = preview_first_n_lines(&error_text, 500);
                        let file_path = match write_agent_file(&dir, "error.txt", &error_text) {
                            Ok(p) => p.display().to_string(),
                            Err(e) => format!("Failed to write error file: {}", e),
                        };
                        let response = serde_json::json!({
                            "agent_id": params.agent_id,
                            "status": agent.status,
                            "error_preview": preview,
                            "error_total_lines": total_lines,
                            "error_file": file_path,
                        });
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone,
                            output: FunctionCallOutputPayload {
                                content: response.to_string(),
                                success: Some(false),
                            },
                        }
                    }
                    _ => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!(
                                "Agent is still {}: cannot get result yet",
                                serde_json::to_string(&agent.status)
                                    .unwrap_or_else(|_| "running".to_string())
                            ),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: format!("Agent not found: {}", params.agent_id),
                        success: Some(false),
                    },
                }
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid agent_result arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_cancel_agent(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_cancel".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<CancelAgentParams>(&arguments_clone) {
        Ok(params) => {
            let mut manager = AGENT_MANAGER.write().await;

            if let Some(agent_id) = params.agent_id {
                if manager.cancel_agent(&agent_id).await {
                    ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Agent {} cancelled", agent_id),
                            success: Some(true),
                        },
                    }
                } else {
                    ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to cancel agent {}", agent_id),
                            success: Some(false),
                        },
                    }
                }
            } else if let Some(batch_id) = params.batch_id {
                let count = manager.cancel_batch(&batch_id).await;
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: format!("Cancelled {} agents in batch {}", count, batch_id),
                        success: Some(true),
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Either agent_id or batch_id must be provided".to_string(),
                        success: Some(false),
                    },
                }
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid agent_cancel arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_wait_for_agent(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_wait".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<WaitForAgentParams>(&arguments_clone) {
        Ok(params) => {
            let timeout =
                std::time::Duration::from_secs(params.timeout_seconds.unwrap_or(300).min(600));
            let start = std::time::Instant::now();

            loop {
                if start.elapsed() > timeout {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: "Timeout waiting for agent completion".to_string(),
                            success: Some(false),
                        },
                    };
                }

                let manager = AGENT_MANAGER.read().await;

                if let Some(agent_id) = &params.agent_id {
                    if let Some(agent) = manager.get_agent(agent_id) {
                        if matches!(
                            agent.status,
                            AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
                        ) {
                            // Include output/error preview and file path
                            let cwd = sess.get_cwd().to_path_buf();
                            let dir = ensure_agent_dir(&cwd, &agent.id).unwrap_or_else(|_| cwd.clone());
                            let (preview_key, file_key, preview, file_path, total_lines) = match agent.status {
                                AgentStatus::Completed => {
                                    let text = agent.result.clone().unwrap_or_default();
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "result.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write result file: {}", e));
                                    ("output_preview", "output_file", p, fp, total)
                                }
                                AgentStatus::Failed => {
                                    let text = agent.error.clone().unwrap_or_else(|| "Unknown error".to_string());
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "error.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write error file: {}", e));
                                    ("error_preview", "error_file", p, fp, total)
                                }
                                AgentStatus::Cancelled => {
                                    let text = "Agent cancelled".to_string();
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "status.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write status file: {}", e));
                                    ("status_preview", "status_file", p, fp, total)
                                }
                                _ => unreachable!(),
                            };

                            let mut response = serde_json::json!({
                                "agent_id": agent.id,
                                "status": agent.status,
                                "wait_time_seconds": start.elapsed().as_secs(),
                                "total_lines": total_lines,
                            });
                            if let Some(obj) = response.as_object_mut() {
                                obj.insert(preview_key.to_string(), serde_json::Value::String(preview));
                                obj.insert(file_key.to_string(), serde_json::Value::String(file_path));
                            }
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: response.to_string(),
                                    success: Some(true),
                                },
                            };
                        }
                    }
                } else if let Some(batch_id) = &params.batch_id {
                    let agents = manager.list_agents(None, Some(batch_id.clone()), false);

                    // Separate terminal vs non-terminal agents
                    let mut completed_agents: Vec<_> = agents
                        .iter()
                        .filter(|t| {
                            matches!(
                                t.status,
                                AgentStatus::Completed
                                    | AgentStatus::Failed
                                    | AgentStatus::Cancelled
                            )
                        })
                        .cloned()
                        .collect();
                    let any_in_progress = agents.iter().any(|a| {
                        matches!(a.status, AgentStatus::Pending | AgentStatus::Running)
                    });

                    if params.return_all.unwrap_or(false) {
                        // Wait for ALL agents in the batch to reach a terminal state
                        if !any_in_progress {
                            let response = serde_json::json!({
                                "batch_id": batch_id,
                                "completed_agents": completed_agents.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
                                "wait_time_seconds": start.elapsed().as_secs(),
                            });
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: response.to_string(),
                                    success: Some(true),
                                },
                            };
                        }
                    } else {
                        // Sequential behavior: return the next unseen completed agent if available
                        let mut state = sess.state.lock().unwrap();
                        let seen = state
                            .seen_completed_agents_by_batch
                            .entry(batch_id.clone())
                            .or_default();

                        // Find the first completed agent that we haven't returned yet
                        if let Some(unseen) = completed_agents
                            .iter()
                            .find(|a| !seen.contains(&a.id))
                            .cloned()
                        {
                            // Record as seen and return immediately
                            seen.insert(unseen.id.clone());
                            drop(state);

                            // Include output/error preview for the unseen completed agent
                            let cwd = sess.get_cwd().to_path_buf();
                            let dir = ensure_agent_dir(&cwd, &unseen.id).unwrap_or_else(|_| cwd.clone());
                            let (preview_key, file_key, preview, file_path, total_lines) = match unseen.status {
                                AgentStatus::Completed => {
                                    let text = unseen.result.clone().unwrap_or_default();
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "result.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write result file: {}", e));
                                    ("output_preview", "output_file", p, fp, total)
                                }
                                AgentStatus::Failed => {
                                    let text = unseen.error.clone().unwrap_or_else(|| "Unknown error".to_string());
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "error.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write error file: {}", e));
                                    ("error_preview", "error_file", p, fp, total)
                                }
                                AgentStatus::Cancelled => {
                                    let text = "Agent cancelled".to_string();
                                    let (p, total) = preview_first_n_lines(&text, 500);
                                    let fp = write_agent_file(&dir, "status.txt", &text)
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|e| format!("Failed to write status file: {}", e));
                                    ("status_preview", "status_file", p, fp, total)
                                }
                                _ => unreachable!(),
                            };

                            let mut response = serde_json::json!({
                                "agent_id": unseen.id,
                                "status": unseen.status,
                                "wait_time_seconds": start.elapsed().as_secs(),
                                "total_lines": total_lines,
                            });
                            if let Some(obj) = response.as_object_mut() {
                                obj.insert(preview_key.to_string(), serde_json::Value::String(preview));
                                obj.insert(file_key.to_string(), serde_json::Value::String(file_path));
                            }
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: response.to_string(),
                                    success: Some(true),
                                },
                            };
                        }

                        // If all agents in the batch are terminal and all have been seen, return immediately
                        if !any_in_progress && !completed_agents.is_empty() {
                            // Mark all as seen to keep state consistent
                            for a in &completed_agents {
                                seen.insert(a.id.clone());
                            }
                            drop(state);

                            let response = serde_json::json!({
                                "batch_id": batch_id,
                                "status": "no_agents_remaining",
                                "wait_time_seconds": start.elapsed().as_secs(),
                            });
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: response.to_string(),
                                    success: Some(true),
                                },
                            };
                        }
                    }
                }

                drop(manager);
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid wait_for_agent arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_list_agents(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params_for_event = serde_json::from_str(&arguments).ok();
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();
    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "agent_list".to_string(),
        params_for_event,
        || async move {
    match serde_json::from_str::<ListAgentsParams>(&arguments_clone) {
        Ok(params) => {
            let manager = AGENT_MANAGER.read().await;

            let status_filter =
                params
                    .status_filter
                    .and_then(|s| match s.to_lowercase().as_str() {
                        "pending" => Some(AgentStatus::Pending),
                        "running" => Some(AgentStatus::Running),
                        "completed" => Some(AgentStatus::Completed),
                        "failed" => Some(AgentStatus::Failed),
                        "cancelled" => Some(AgentStatus::Cancelled),
                        _ => None,
                    });

            let agents = manager.list_agents(
                status_filter,
                params.batch_id,
                params.recent_only.unwrap_or(false),
            );

            // Count running agents for status update
            let running_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Running)
                .count();
            if running_count > 0 {
                let status_msg = format!(
                    "🤖 {} agent{} currently running",
                    running_count,
                    if running_count != 1 { "s" } else { "" }
                );
                let event = Event {
                    id: "agent-status".to_string(),
                    msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                        message: status_msg,
                    }),
                };
                let _ = sess.tx_event.send(event).await;
            }

            // Add status counts to summary
            let pending_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Pending)
                .count();
            let running_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Running)
                .count();
            let completed_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Completed)
                .count();
            let failed_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Failed)
                .count();
            let cancelled_count = agents
                .iter()
                .filter(|a| a.status == AgentStatus::Cancelled)
                .count();

            let summary = serde_json::json!({
                "total_agents": agents.len(),
                "status_counts": {
                    "pending": pending_count,
                    "running": running_count,
                    "completed": completed_count,
                    "failed": failed_count,
                    "cancelled": cancelled_count,
                },
                "agents": agents.iter().map(|t| {
                    serde_json::json!({
                        "id": t.id,
                        "model": t.model,
                        "status": t.status,
                        "created_at": t.created_at,
                        "batch_id": t.batch_id,
                        "worktree_path": t.worktree_path,
                        "branch_name": t.branch_name,
                    })
                }).collect::<Vec<_>>(),
            });

            ResponseInputItem::FunctionCallOutput {
                call_id: call_id_clone,
                output: FunctionCallOutputPayload {
                    content: summary.to_string(),
                    success: Some(true),
                },
            }
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: format!("Invalid list_agents arguments: {}", e),
                success: None,
            },
        },
    }
        },
    ).await
}

async fn handle_container_exec_with_params(
    params: ExecParams,
    sess: &Session,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    // check if this was a patch, and apply it if so
    let apply_patch_exec = match maybe_parse_apply_patch_verified(&params.command, &params.cwd) {
        MaybeApplyPatchVerified::Body(changes) => {
            match apply_patch::apply_patch(sess, &sub_id, &call_id, changes).await {
                InternalApplyPatchInvocation::Output(item) => return item,
                InternalApplyPatchInvocation::DelegateToExec(apply_patch_exec) => {
                    Some(apply_patch_exec)
                }
            }
        }
        MaybeApplyPatchVerified::CorrectnessError(parse_error) => {
            // It looks like an invocation of `apply_patch`, but we
            // could not resolve it into a patch that would apply
            // cleanly. Return to model for resample.
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("error: {parse_error:#}"),
                    success: None,
                },
            };
        }
        MaybeApplyPatchVerified::ShellParseError(error) => {
            trace!("Failed to parse shell command, {error:?}");
            None
        }
        MaybeApplyPatchVerified::NotApplyPatch => None,
    };

    let (params, safety, command_for_display) = match &apply_patch_exec {
        Some(ApplyPatchExec {
            action: ApplyPatchAction { patch, cwd, .. },
            user_explicitly_approved_this_action,
        }) => {
            let path_to_codex = std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            let Some(path_to_codex) = path_to_codex else {
                return ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content: "failed to determine path to codex executable".to_string(),
                        success: None,
                    },
                };
            };

            let params = ExecParams {
                command: vec![
                    path_to_codex,
                    CODEX_APPLY_PATCH_ARG1.to_string(),
                    patch.clone(),
                ],
                cwd: cwd.clone(),
                timeout_ms: params.timeout_ms,
                env: HashMap::new(),
                with_escalated_permissions: params.with_escalated_permissions,
                justification: params.justification.clone(),
            };
            let safety = if *user_explicitly_approved_this_action {
                SafetyCheck::AutoApprove {
                    sandbox_type: SandboxType::None,
                }
            } else {
                assess_safety_for_untrusted_command(
                    sess.approval_policy,
                    &sess.sandbox_policy,
                    params.with_escalated_permissions.unwrap_or(false),
                )
            };
            (
                params,
                safety,
                vec!["apply_patch".to_string(), patch.clone()],
            )
        }
        None => {
            let safety = {
                let state = sess.state.lock().unwrap();
                assess_command_safety(
                    &params.command,
                    sess.approval_policy,
                    &sess.sandbox_policy,
                    &state.approved_commands,
                    params.with_escalated_permissions.unwrap_or(false),
                )
            };
            let command_for_display = params.command.clone();
            (params, safety, command_for_display)
        }
    };

    let sandbox_type = match safety {
        SafetyCheck::AutoApprove { sandbox_type } => sandbox_type,
        SafetyCheck::AskUser => {
            let rx_approve = sess
                .request_command_approval(
                    sub_id.clone(),
                    call_id.clone(),
                    params.command.clone(),
                    params.cwd.clone(),
                    params.justification.clone(),
                )
                .await;
            match rx_approve.await.unwrap_or_default() {
                ReviewDecision::Approved => (),
                ReviewDecision::ApprovedForSession => {
                    sess.add_approved_command(params.command.clone());
                }
                ReviewDecision::Denied | ReviewDecision::Abort => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: "exec command rejected by user".to_string(),
                            success: None,
                        },
                    };
                }
            }
            // No sandboxing is applied because the user has given
            // explicit approval. Often, we end up in this case because
            // the command cannot be run in a sandbox, such as
            // installing a new dependency that requires network access.
            SandboxType::None
        }
        SafetyCheck::Reject { reason } => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("exec command rejected: {reason}"),
                    success: None,
                },
            };
        }
    };

    let exec_command_context = ExecCommandContext {
        sub_id: sub_id.clone(),
        call_id: call_id.clone(),
        command_for_display: command_for_display.clone(),
        cwd: params.cwd.clone(),
        apply_patch: apply_patch_exec.map(
            |ApplyPatchExec {
                 action,
                 user_explicitly_approved_this_action,
             }| ApplyPatchCommandContext {
                user_explicitly_approved_this_action,
                changes: convert_apply_patch_to_protocol(&action),
            },
        ),
    };

    let params = maybe_run_with_user_profile(params, sess);
    let output_result = sess
        .run_exec_with_events(
            turn_diff_tracker,
            exec_command_context.clone(),
            ExecInvokeArgs {
                params: params.clone(),
                sandbox_type,
                sandbox_policy: &sess.sandbox_policy,
                codex_linux_sandbox_exe: &sess.codex_linux_sandbox_exe,
                stdout_stream: Some(StdoutStream {
                    sub_id: sub_id.clone(),
                    call_id: call_id.clone(),
                    tx_event: sess.tx_event.clone(),
                }),
            },
        )
        .await;

    match output_result {
        Ok(output) => {
            let ExecToolCallOutput { exit_code, .. } = &output;

            let is_success = *exit_code == 0;
            let content = format_exec_output(output);
            ResponseInputItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: FunctionCallOutputPayload {
                    content,
                    success: Some(is_success),
                },
            }
        }
        Err(CodexErr::Sandbox(error)) => {
            handle_sandbox_error(
                turn_diff_tracker,
                params,
                exec_command_context,
                error,
                sandbox_type,
                sess,
            )
            .await
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output: FunctionCallOutputPayload {
                content: format!("execution error: {e}"),
                success: None,
            },
        },
    }
}

async fn handle_sandbox_error(
    turn_diff_tracker: &mut TurnDiffTracker,
    params: ExecParams,
    exec_command_context: ExecCommandContext,
    error: SandboxErr,
    sandbox_type: SandboxType,
    sess: &Session,
) -> ResponseInputItem {
    let call_id = exec_command_context.call_id.clone();
    let sub_id = exec_command_context.sub_id.clone();
    let cwd = exec_command_context.cwd.clone();

    // Early out if either the user never wants to be asked for approval, or
    // we're letting the model manage escalation requests. Otherwise, continue
    match sess.approval_policy {
        AskForApproval::Never | AskForApproval::OnRequest => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!(
                        "failed in sandbox {sandbox_type:?} with execution error: {error}"
                    ),
                    success: Some(false),
                },
            };
        }
        AskForApproval::UnlessTrusted | AskForApproval::OnFailure => (),
    }

    // similarly, if the command timed out, we can simply return this failure to the model
    if matches!(error, SandboxErr::Timeout) {
        return ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: format!(
                    "command timed out after {} milliseconds",
                    params.timeout_duration().as_millis()
                ),
                success: Some(false),
            },
        };
    }

    // Note that when `error` is `SandboxErr::Denied`, it could be a false
    // positive. That is, it may have exited with a non-zero exit code, not
    // because the sandbox denied it, but because that is its expected behavior,
    // i.e., a grep command that did not match anything. Ideally we would
    // include additional metadata on the command to indicate whether non-zero
    // exit codes merit a retry.

    // For now, we categorically ask the user to retry without sandbox and
    // emit the raw error as a background event.
    sess.notify_background_event(&sub_id, format!("Execution failed: {error}"))
        .await;

    let rx_approve = sess
        .request_command_approval(
            sub_id.clone(),
            call_id.clone(),
            params.command.clone(),
            cwd.clone(),
            Some("command failed; retry without sandbox?".to_string()),
        )
        .await;

    match rx_approve.await.unwrap_or_default() {
        ReviewDecision::Approved | ReviewDecision::ApprovedForSession => {
            // Persist this command as pre‑approved for the
            // remainder of the session so future
            // executions skip the sandbox directly.
            // TODO(ragona): Isn't this a bug? It always saves the command in an | fork?
            sess.add_approved_command(params.command.clone());
            // Inform UI we are retrying without sandbox.
            sess.notify_background_event(&sub_id, "retrying command without sandbox")
                .await;

            // This is an escalated retry; the policy will not be
            // examined and the sandbox has been set to `None`.
            let retry_output_result = sess
                .run_exec_with_events(
                    turn_diff_tracker,
                    exec_command_context.clone(),
                    ExecInvokeArgs {
                        params,
                        sandbox_type: SandboxType::None,
                        sandbox_policy: &sess.sandbox_policy,
                        codex_linux_sandbox_exe: &sess.codex_linux_sandbox_exe,
                        stdout_stream: Some(StdoutStream {
                            sub_id: sub_id.clone(),
                            call_id: call_id.clone(),
                            tx_event: sess.tx_event.clone(),
                        }),
                    },
                )
                .await;

            match retry_output_result {
                Ok(retry_output) => {
                    let ExecToolCallOutput { exit_code, .. } = &retry_output;

                    let is_success = *exit_code == 0;
                    let content = format_exec_output(retry_output);

                    ResponseInputItem::FunctionCallOutput {
                        call_id: call_id.clone(),
                        output: FunctionCallOutputPayload {
                            content,
                            success: Some(is_success),
                        },
                    }
                }
                Err(e) => ResponseInputItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    output: FunctionCallOutputPayload {
                        content: format!("retry failed: {e}"),
                        success: None,
                    },
                },
            }
        }
        ReviewDecision::Denied | ReviewDecision::Abort => {
            // Fall through to original failure handling.
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: "exec command rejected by user".to_string(),
                    success: None,
                },
            }
        }
    }
}

/// Exec output is a pre-serialized JSON payload
fn format_exec_output(exec_output: ExecToolCallOutput) -> String {
    let ExecToolCallOutput {
        exit_code,
        stdout,
        stderr,
        duration,
    } = exec_output;

    #[derive(Serialize)]
    struct ExecMetadata {
        exit_code: i32,
        duration_seconds: f32,
    }

    #[derive(Serialize)]
    struct ExecOutput<'a> {
        output: &'a str,
        metadata: ExecMetadata,
    }

    // round to 1 decimal place
    let duration_seconds = ((duration.as_secs_f32()) * 10.0).round() / 10.0;

    let is_success = exit_code == 0;
    let output = if is_success { stdout } else { stderr };

    let mut formatted_output = output.text;
    if let Some(truncated_after_lines) = output.truncated_after_lines {
        formatted_output.push_str(&format!(
            "\n\n[Output truncated after {truncated_after_lines} lines: too many lines or bytes.]",
        ));
    }

    let payload = ExecOutput {
        output: &formatted_output,
        metadata: ExecMetadata {
            exit_code,
            duration_seconds,
        },
    };

    #[expect(clippy::expect_used)]
    serde_json::to_string(&payload).expect("serialize ExecOutput")
}

fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    responses.iter().rev().find_map(|item| {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "assistant" {
                content.iter().rev().find_map(|ci| {
                    if let ContentItem::OutputText { text } = ci {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        } else {
            None
        }
    })
}

async fn drain_to_completed(sess: &Session, sub_id: &str, prompt: &Prompt) -> CodexResult<()> {
    let mut stream = sess.client.clone().stream(prompt).await?;
    loop {
        let maybe_event = stream.next().await;
        let Some(event) = maybe_event else {
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => {
                // Record only to in-memory conversation history; avoid state snapshot.
                let mut state = sess.state.lock().unwrap();
                state.history.record_items(std::slice::from_ref(&item));
            }
            Ok(ResponseEvent::Completed {
                response_id: _,
                token_usage,
            }) => {
                let token_usage = match token_usage {
                    Some(usage) => usage,
                    None => {
                        return Err(CodexErr::Stream(
                            "token_usage was None in ResponseEvent::Completed".into(),
                            None,
                        ));
                    }
                };
                // Remember last completed token usage for baseline in post-prune updates
                {
                    let mut st = sess.state.lock().unwrap();
                    st.last_completed_token_usage = Some(token_usage.clone());
                }
                sess.tx_event
                    .send(Event {
                        id: sub_id.to_string(),
                        msg: EventMsg::TokenCount(token_usage),
                    })
                    .await
                    .ok();
                return Ok(());
            }
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Capture a screenshot from the browser and store it for the next model request
async fn capture_browser_screenshot(_sess: &Session) -> Result<(PathBuf, String), String> {
    let browser_manager = codex_browser::global::get_browser_manager()
        .await
        .ok_or_else(|| "No browser manager available".to_string())?;

    if !browser_manager.is_enabled().await {
        return Err("Browser manager is not enabled".to_string());
    }

    // Get current URL first
    let url = browser_manager
        .get_current_url()
        .await
        .unwrap_or_else(|| "Browser".to_string());
    tracing::debug!("Attempting to capture screenshot at URL: {}", url);

    match browser_manager.capture_screenshot().await {
        Ok(screenshots) => {
            if let Some(first_screenshot) = screenshots.first() {
                tracing::info!(
                    "Captured browser screenshot: {} at URL: {}",
                    first_screenshot.display(),
                    url
                );
                Ok((first_screenshot.clone(), url))
            } else {
                let msg = format!("Screenshot capture returned empty results at URL: {}", url);
                tracing::warn!("{}", msg);
                Err(msg)
            }
        }
        Err(e) => {
            let msg = format!("Failed to capture screenshot at {}: {}", url, e);
            tracing::warn!("{}", msg);
            Err(msg)
        }
    }
}

/// Send agent status update event to the TUI
async fn send_agent_status_update(sess: &Session) {
    let manager = AGENT_MANAGER.read().await;

    // Collect all active agents (not completed/failed/cancelled)
    let agents: Vec<crate::protocol::AgentInfo> = manager
        .get_all_agents()
        .filter(|agent| {
            !matches!(
                agent.status,
                AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled
            )
        })
        .map(|agent| crate::protocol::AgentInfo {
            id: agent.id.clone(),
            name: agent.model.clone(), // Use model name as the display name
            status: match agent.status {
                AgentStatus::Pending => "pending".to_string(),
                AgentStatus::Running => "running".to_string(),
                AgentStatus::Completed => "completed".to_string(),
                AgentStatus::Failed => "failed".to_string(),
                AgentStatus::Cancelled => "cancelled".to_string(),
            },
            model: Some(agent.model.clone()),
        })
        .collect();

    let event = Event {
        id: "agent_status".to_string(),
        msg: EventMsg::AgentStatusUpdate(AgentStatusUpdateEvent {
            agents,
            context: None,
            task: None,
        }),
    };

    // Send event asynchronously
    let tx_event = sess.tx_event.clone();
    tokio::spawn(async move {
        if let Err(e) = tx_event.send(event).await {
            tracing::error!("Failed to send agent status update event: {}", e);
        }
    });
}

/// Add a screenshot to pending screenshots for the next model request
fn add_pending_screenshot(sess: &Session, screenshot_path: PathBuf, url: String) {
    // Do not queue screenshots for next turn anymore; we inject fresh per-turn.
    tracing::info!("Captured screenshot; updating UI and using per-turn injection");

    // Also send an immediate event to update the TUI display
    let event = Event {
        id: "browser_screenshot".to_string(),
        msg: EventMsg::BrowserScreenshotUpdate(BrowserScreenshotUpdateEvent {
            screenshot_path,
            url,
        }),
    };

    // Send event asynchronously to avoid blocking
    let tx_event = sess.tx_event.clone();
    tokio::spawn(async move {
        if let Err(e) = tx_event.send(event).await {
            tracing::error!("Failed to send browser screenshot update event: {}", e);
        }
    });
}

/// Consume pending screenshots and return them as ResponseInputItems
#[allow(dead_code)]
fn consume_pending_screenshots(sess: &Session) -> Vec<ResponseInputItem> {
    let mut pending = sess.pending_browser_screenshots.lock().unwrap();
    let screenshots = pending.drain(..).collect::<Vec<_>>();

    screenshots
        .into_iter()
        .map(|path| {
            let metadata = format!(
                "[EPHEMERAL:browser_screenshot] Browser screenshot at {}",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
            );

            // Read the screenshot file and create an ephemeral image
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let mime = mime_guess::from_path(&path)
                        .first()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| "image/png".to_string());
                    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);

                    ResponseInputItem::Message {
                        role: "user".to_string(),
                        content: vec![
                            ContentItem::InputText { text: metadata },
                            ContentItem::InputImage {
                                image_url: format!("data:{mime};base64,{encoded}"),
                                detail: Some("high".to_string()),
                            },
                        ],
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to read screenshot {}: {}", path.display(), e);
                    ResponseInputItem::Message {
                        role: "user".to_string(),
                        content: vec![ContentItem::InputText {
                            text: format!("Failed to load browser screenshot: {}", e),
                        }],
                    }
                }
            }
        })
        .collect()
}

/// Helper function to wrap custom tool calls with events
async fn execute_custom_tool<F, Fut>(
    sess: &Session,
    sub_id: &str,
    call_id: String,
    tool_name: String,
    parameters: Option<serde_json::Value>,
    tool_fn: F,
) -> ResponseInputItem
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ResponseInputItem>,
{
    use crate::protocol::{CustomToolCallBeginEvent, CustomToolCallEndEvent};
    use std::time::Instant;

    // Send begin event
    let begin_event = Event {
        id: sub_id.to_string(),
        msg: EventMsg::CustomToolCallBegin(CustomToolCallBeginEvent {
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
            parameters: parameters.clone(),
        }),
    };
    sess.send_event(begin_event).await;

    // Execute the tool
    let start = Instant::now();
    let result = tool_fn().await;
    let duration = start.elapsed();

    // Extract success/failure from result. Prefer explicit success flag when available.
    let (success, message) = match &result {
        ResponseInputItem::FunctionCallOutput { output, .. } => {
            let content = &output.content;
            let success_flag = output.success;
            (success_flag.unwrap_or(true), content.clone())
        }
        _ => (true, String::from("Tool completed")),
    };

    // Send end event
    let end_event = Event {
        id: sub_id.to_string(),
        msg: EventMsg::CustomToolCallEnd(CustomToolCallEndEvent {
            call_id: call_id.clone(),
            tool_name,
            parameters,
            duration,
            result: if success { Ok(message) } else { Err(message) },
        }),
    };
    sess.send_event(end_event).await;

    result
}

async fn handle_browser_open(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    // Parse arguments as JSON for the event
    let params = serde_json::from_str(&arguments).ok();

    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_open".to_string(),
        params,
        || async move {
            // Parse the URL from arguments
            let args: Result<Value, _> = serde_json::from_str(&arguments_clone);

            match args {
                Ok(json) => {
                    let url = json
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("about:blank");

                    // Use the global browser manager (create if needed)
                    let browser_manager = {
                        let existing_global = codex_browser::global::get_browser_manager().await;
                        if let Some(existing) = existing_global {
                            tracing::info!("Using existing global browser manager");
                            Some(existing)
                        } else {
                            tracing::info!("Creating new browser manager");
                            let new_manager =
                                codex_browser::global::get_or_create_browser_manager().await;
                            // Enable the browser
                            new_manager.set_enabled_sync(true);
                            Some(new_manager)
                        }
                    };

                    if let Some(browser_manager) = browser_manager {
                        // Clear any lingering node highlight from previous commands
                        let _ = browser_manager
                            .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                            .await;
                        // Navigate to the URL with detailed timing logs
                        let step_start = std::time::Instant::now();
                        tracing::info!("[browser_open] begin goto: {}", url);
                        match browser_manager.goto(url).await {
                            Ok(_) => {
                                tracing::info!(
                                    "[browser_open] goto success: {} in {:?}",
                                    url,
                                    step_start.elapsed()
                                );
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("Browser opened to: {}", url),
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!(
                                        "Failed to navigate browser to {}: {}",
                                        url, e
                                    ),
                                    success: Some(false),
                                },
                            },
                        }
                    } else {
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone.clone(),
                            output: FunctionCallOutputPayload {
                                content: "Failed to initialize browser manager.".to_string(),
                                success: Some(false),
                            },
                        }
                    }
                }
                Err(e) => ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: format!("Failed to parse browser_open arguments: {}", e),
                        success: Some(false),
                    },
                },
            }
        },
    )
    .await
}

/// Get the browser manager for the session (always uses global)
async fn get_browser_manager_for_session(
    _sess: &Session,
) -> Option<Arc<codex_browser::BrowserManager>> {
    // Always use the global browser manager
    codex_browser::global::get_browser_manager().await
}

async fn handle_browser_close(
    sess: &Session,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let sess_clone = sess;
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_close".to_string(),
        None,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                // Clear any lingering highlight before closing
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                match browser_manager.stop().await {
                    Ok(_) => {
                        // Clear the browser manager from global
                        codex_browser::global::clear_browser_manager().await;
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone.clone(),
                            output: FunctionCallOutputPayload {
                                content: "Browser closed. Screenshot capture disabled.".to_string(),
                                success: Some(true),
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone.clone(),
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to close browser: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Browser is not currently open.".to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_status(
    sess: &Session,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let sess_clone = sess;
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_status".to_string(),
        None,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let status = browser_manager.get_status().await;
                let status_msg = if status.enabled {
                    if let Some(url) = status.current_url {
                        format!("Browser status: Enabled, currently at {}", url)
                    } else {
                        "Browser status: Enabled, no page loaded".to_string()
                    }
                } else {
                    "Browser status: Disabled".to_string()
                };

                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone.clone(),
                    output: FunctionCallOutputPayload {
                        content: status_msg,
                        success: Some(true),
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content:
                            "Browser is not initialized. Use browser_open to start the browser."
                                .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_click(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str::<serde_json::Value>(&arguments).ok();
    let sess_clone = sess;
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_click".to_string(),
        params.clone(),
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;

            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                // Determine click type: default 'click', or 'mousedown'/'mouseup'
                let click_type = params
                    .as_ref()
                    .and_then(|v| v.get("type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("click")
                    .to_lowercase();

                // Optional absolute coordinates
                let (mut target_x, mut target_y) = (None, None);
                if let Some(p) = params.as_ref() {
                    if let Some(vx) = p.get("x").and_then(|v| v.as_f64()) {
                        target_x = Some(vx);
                    }
                    if let Some(vy) = p.get("y").and_then(|v| v.as_f64()) {
                        target_y = Some(vy);
                    }
                }

                // If x or y provided, resolve missing coord from current position, then move
                if target_x.is_some() || target_y.is_some() {
                    // get current cursor for missing values
                    match browser_manager.get_cursor_position().await {
                        Ok((cx, cy)) => {
                            let x = target_x.unwrap_or(cx);
                            let y = target_y.unwrap_or(cy);
                            if let Err(e) = browser_manager.move_mouse(x, y).await {
                                return ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("Failed to move before click: {}", e),
                                        success: Some(false),
                                    },
                                };
                            }
                        }
                        Err(e) => {
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to get current cursor position: {}", e),
                                    success: Some(false),
                                },
                            };
                        }
                    }
                }

                // Perform the action at current (possibly moved) position
                let action_result = match click_type.as_str() {
                    "mousedown" => match browser_manager.mouse_down_at_current().await {
                        Ok((x, y)) => Ok((x, y, "Mouse down".to_string())),
                        Err(e) => Err(e),
                    },
                    "mouseup" => match browser_manager.mouse_up_at_current().await {
                        Ok((x, y)) => Ok((x, y, "Mouse up".to_string())),
                        Err(e) => Err(e),
                    },
                    "click" | _ => match browser_manager.click_at_current().await {
                        Ok((x, y)) => Ok((x, y, "Clicked".to_string())),
                        Err(e) => Err(e),
                    },
                };

                match action_result {
                    Ok((x, y, label)) => {
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone.clone(),
                            output: FunctionCallOutputPayload {
                                content: format!("{} at ({}, {})", label, x, y),
                                success: Some(true),
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone.clone(),
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to perform mouse action: {}", e),
                            success: Some(false),
                        },
                    },
                }
    } else {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: "Browser is not initialized. Use browser_open to start the browser."
                    .to_string(),
                success: Some(false),
            },
        }
    }
        },
    )
    .await
}

async fn handle_browser_move(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_move".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;

            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        // Check if we have relative movement (dx, dy) or absolute (x, y)
                        let has_dx = json.get("dx").is_some();
                        let has_dy = json.get("dy").is_some();
                        let has_x = json.get("x").is_some();
                        let has_y = json.get("y").is_some();

                        let result = if has_dx || has_dy {
                            // Relative movement
                            let dx = json.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let dy = json.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            browser_manager.move_mouse_relative(dx, dy).await
                        } else if has_x || has_y {
                            // Absolute movement
                            let x = json.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let y = json.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            browser_manager.move_mouse(x, y).await.map(|_| (x, y))
                        } else {
                            // No parameters provided, just return current position
                            browser_manager.get_cursor_position().await
                        };

                        match result {
                            Ok((x, y)) => {
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("Moved mouse position to ({}, {})", x, y),
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to move mouse: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone.clone(),
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_move arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Browser is not initialized. Use browser_open to start the browser."
                            .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_type(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_type".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let text = json.get("text").and_then(|v| v.as_str()).unwrap_or("");

                        match browser_manager.type_text(text).await {
                            Ok(_) => {
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("Typed: {}", text),
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to type text: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone.clone(),
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_type arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content:
                            "Browser is not initialized. Use browser_open to start the browser."
                                .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_key(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_key".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let key = json.get("key").and_then(|v| v.as_str()).unwrap_or("");

                        match browser_manager.press_key(key).await {
                            Ok(_) => {
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("Pressed key: {}", key),
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to press key: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_key arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content:
                            "Browser is not initialized. Use browser_open to start the browser."
                                .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_javascript(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_javascript".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let code = json.get("code").and_then(|v| v.as_str()).unwrap_or("");

                        match browser_manager.execute_javascript(code).await {
                            Ok(result) => {
                                // Log the JavaScript execution result
                                tracing::info!("JavaScript execution returned: {:?}", result);

                                // Format the result for the LLM
                                let formatted_result = if let Some(obj) = result.as_object() {
                                    // Check if it's our wrapped result format
                                    if let (Some(success), Some(value)) =
                                        (obj.get("success"), obj.get("value"))
                                    {
                                        let logs = obj.get("logs").and_then(|v| v.as_array());
                                        let mut output = String::new();

                                        if let Some(logs) = logs {
                                            if !logs.is_empty() {
                                                output.push_str("Console logs:\n");
                                                for log in logs {
                                                    if let Some(log_str) = log.as_str() {
                                                        output
                                                            .push_str(&format!("  {}\n", log_str));
                                                    }
                                                }
                                                output.push_str("\n");
                                            }
                                        }

                                        if success.as_bool().unwrap_or(false) {
                                            output.push_str("Result: ");
                                            output.push_str(
                                                &serde_json::to_string_pretty(value)
                                                    .unwrap_or_else(|_| "null".to_string()),
                                            );
                                        } else if let Some(error) = obj.get("error") {
                                            output.push_str("Error: ");
                                            output.push_str(&error.to_string());
                                        }

                                        output
                                    } else {
                                        // Fallback to raw JSON if not in expected format
                                        serde_json::to_string_pretty(&result)
                                            .unwrap_or_else(|_| "null".to_string())
                                    }
                                } else {
                                    // Not an object, return as-is
                                    serde_json::to_string_pretty(&result)
                                        .unwrap_or_else(|_| "null".to_string())
                                };

                                tracing::info!("Returning to LLM: {}", formatted_result);

                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: formatted_result,
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to execute JavaScript: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_javascript arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content:
                            "Browser is not initialized. Use browser_open to start the browser."
                                .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_scroll(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_scroll".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let dx = json.get("dx").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let dy = json.get("dy").and_then(|v| v.as_f64()).unwrap_or(0.0);

                        match browser_manager.scroll_by(dx, dy).await {
                    Ok(_) => {
                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone.clone(),
                            output: FunctionCallOutputPayload {
                                content: format!("Scrolled by ({}, {})", dx, dy),
                                success: Some(true),
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone.clone(),
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to scroll: {}", e),
                            success: Some(false),
                        },
                    },
                }
            }
            Err(e) => ResponseInputItem::FunctionCallOutput {
                call_id: call_id_clone,
                output: FunctionCallOutputPayload {
                    content: format!("Failed to parse browser_scroll arguments: {}", e),
                    success: Some(false),
                },
            },
        }
    } else {
        ResponseInputItem::FunctionCallOutput {
            call_id: call_id_clone,
            output: FunctionCallOutputPayload {
                content: "Browser is not initialized. Use browser_open to start the browser.".to_string(),
                success: Some(false),
            },
        }
    }
        },
    )
    .await
}

async fn handle_browser_console(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_console".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                let lines = match args {
                    Ok(json) => json.get("lines").and_then(|v| v.as_u64()).map(|n| n as usize),
                    Err(_) => None,
                };

                match browser_manager.get_console_logs(lines).await {
                    Ok(logs) => {
                        // Format the logs for display
                        let formatted = if let Some(logs_array) = logs.as_array() {
                            if logs_array.is_empty() {
                                "No console logs captured.".to_string()
                            } else {
                                let mut output = String::new();
                                output.push_str("Console logs:\n");
                                for log in logs_array {
                                    if let Some(log_obj) = log.as_object() {
                                        let timestamp = log_obj.get("timestamp")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let level = log_obj.get("level")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("log");
                                        let message = log_obj.get("message")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        
                                        output.push_str(&format!("[{}] [{}] {}\n", timestamp, level.to_uppercase(), message));
                                    }
                                }
                                output
                            }
                        } else {
                            "No console logs captured.".to_string()
                        };

                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone,
                            output: FunctionCallOutputPayload {
                                content: formatted,
                                success: Some(true),
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to get console logs: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Browser is not enabled. Use browser_open to enable it first.".to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_cdp(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_cdp".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let _ = browser_manager
                    .execute_cdp("Overlay.hideHighlight", serde_json::json!({}))
                    .await;
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let method = json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let params = json.get("params").cloned().unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                        let target = json
                            .get("target")
                            .and_then(|v| v.as_str())
                            .unwrap_or("page");

                        if method.is_empty() {
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: "Missing required field: method".to_string(),
                                    success: Some(false),
                                },
                            };
                        }

                        let exec_res = if target == "browser" {
                            browser_manager.execute_cdp_browser(&method, params).await
                        } else {
                            browser_manager.execute_cdp(&method, params).await
                        };

                        match exec_res {
                            Ok(result) => {
                                let pretty = serde_json::to_string_pretty(&result)
                                    .unwrap_or_else(|_| "<non-serializable result>".to_string());
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone,
                                    output: FunctionCallOutputPayload {
                                        content: pretty,
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to execute CDP command: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_cdp arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Browser is not initialized. Use browser_open to start the browser.".to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_inspect(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    use serde_json::json;
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_inspect".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        // Determine target element: by id, by coords, or by cursor
                        let id_attr = json.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let mut x = json.get("x").and_then(|v| v.as_f64());
                        let mut y = json.get("y").and_then(|v| v.as_f64());

                        if (x.is_none() || y.is_none()) && id_attr.is_none() {
                            // No coords provided; use current cursor
                            if let Ok((cx, cy)) = browser_manager.get_cursor_position().await {
                                x = Some(cx);
                                y = Some(cy);
                            }
                        }

                        // Resolve nodeId
                        let node_id_value = if let Some(id_attr) = id_attr.clone() {
                            // Use DOM.getDocument -> DOM.querySelector with selector `#id`
                            let doc = browser_manager
                                .execute_cdp("DOM.getDocument", json!({}))
                                .await
                                .map_err(|e| e);
                            let root_id = match doc {
                                Ok(v) => v.get("root").and_then(|r| r.get("nodeId")).and_then(|n| n.as_u64()),
                                Err(_) => None,
                            };
                            if let Some(root_node_id) = root_id {
                                let sel = format!("#{}", id_attr);
                                let q = browser_manager
                                    .execute_cdp(
                                        "DOM.querySelector",
                                        json!({"nodeId": root_node_id, "selector": sel}),
                                    )
                                    .await;
                                match q {
                                    Ok(v) => v.get("nodeId").cloned(),
                                    Err(_) => None,
                                }
                            } else {
                                None
                            }
                        } else if let (Some(x), Some(y)) = (x, y) {
                            // Use DOM.getNodeForLocation
                            let res = browser_manager
                                .execute_cdp(
                                    "DOM.getNodeForLocation",
                                    json!({
                                        "x": x,
                                        "y": y,
                                        "includeUserAgentShadowDOM": true
                                    }),
                                )
                                .await;
                            match res {
                                Ok(v) => {
                                    // Prefer nodeId; if absent, push backendNodeId
                                    if let Some(n) = v.get("nodeId").cloned() {
                                        Some(n)
                                    } else if let Some(backend) = v.get("backendNodeId").and_then(|b| b.as_u64()) {
                                        let pushed = browser_manager
                                            .execute_cdp(
                                                "DOM.pushNodesByBackendIdsToFrontend",
                                                json!({ "backendNodeIds": [backend] }),
                                            )
                                            .await
                                            .ok();
                                        pushed
                                            .and_then(|pv| pv.get("nodeIds").and_then(|arr| arr.as_array().cloned()))
                                            .and_then(|arr| arr.first().cloned())
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => None,
                            }
                        } else {
                            None
                        };

                        let node_id = match node_id_value.and_then(|v| v.as_u64()) {
                            Some(id) => id,
                            None => {
                                return ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone,
                                    output: FunctionCallOutputPayload {
                                        content: "Failed to resolve target node for inspection".to_string(),
                                        success: Some(false),
                                    },
                                };
                            }
                        };

                        // Enable CSS domain to get matched rules
                        let _ = browser_manager.execute_cdp("CSS.enable", json!({})).await;

                        // Gather details
                        let attrs = browser_manager
                            .execute_cdp("DOM.getAttributes", json!({"nodeId": node_id}))
                            .await
                            .unwrap_or_else(|_| json!({}));
                        let outer = browser_manager
                            .execute_cdp("DOM.getOuterHTML", json!({"nodeId": node_id}))
                            .await
                            .unwrap_or_else(|_| json!({}));
                        let box_model = browser_manager
                            .execute_cdp("DOM.getBoxModel", json!({"nodeId": node_id}))
                            .await
                            .unwrap_or_else(|_| json!({}));
                        let styles = browser_manager
                            .execute_cdp("CSS.getMatchedStylesForNode", json!({"nodeId": node_id}))
                            .await
                            .unwrap_or_else(|_| json!({}));

                        // Highlight the inspected node using Overlay domain (no screenshot capture here)
                        let _ = browser_manager.execute_cdp("Overlay.enable", json!({})).await;
                        let highlight_config = json!({
                            "showInfo": true,
                            "showStyles": false,
                            "showRulers": false,
                            "contentColor": {"r": 111, "g": 168, "b": 220, "a": 0.20},
                            "paddingColor": {"r": 147, "g": 196, "b": 125, "a": 0.55},
                            "borderColor": {"r": 255, "g": 229, "b": 153, "a": 0.60},
                            "marginColor": {"r": 246, "g": 178, "b": 107, "a": 0.60}
                        });
                        let _ = browser_manager.execute_cdp(
                            "Overlay.highlightNode",
                            json!({ "nodeId": node_id, "highlightConfig": highlight_config })
                        ).await;
                        // Do not hide here; keep highlight until the next browser command.

                        // Format output
                        let mut out = String::new();
                        if let (Some(ix), Some(iy)) = (x, y) {
                            out.push_str(&format!("Target: coordinates ({}, {})\n", ix, iy));
                        }
                        if let Some(id_attr) = id_attr {
                            out.push_str(&format!("Target: id '#{}'\n", id_attr));
                        }
                        out.push_str(&format!("NodeId: {}\n", node_id));

                        // Attributes
                        if let Some(arr) = attrs.get("attributes").and_then(|v| v.as_array()) {
                            out.push_str("Attributes:\n");
                            let mut it = arr.iter();
                            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                                out.push_str(&format!("  {}=\"{}\"\n", k.as_str().unwrap_or(""), v.as_str().unwrap_or("")));
                            }
                        }

                        // Outer HTML
                        if let Some(html) = outer.get("outerHTML").and_then(|v| v.as_str()) {
                            let one = html.replace('\n', " ");
                            let snippet: String = one.chars().take(800).collect();
                            out.push_str("\nOuterHTML (truncated):\n");
                            out.push_str(&snippet);
                            if one.len() > snippet.len() { out.push_str("…"); }
                            out.push('\n');
                        }

                        // Box Model summary
                        if box_model.get("model").is_some() {
                            out.push_str("\nBoxModel: available (content/padding/border/margin)\n");
                        }

                        // Matched styles summary
                        if let Some(rules) = styles.get("matchedCSSRules").and_then(|v| v.as_array()) {
                            out.push_str(&format!("Matched CSS rules: {}\n", rules.len()));
                        }

                        // No inline screenshot capture; result reflects DOM details only.

                        ResponseInputItem::FunctionCallOutput {
                            call_id: call_id_clone,
                            output: FunctionCallOutputPayload { content: out, success: Some(true) },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_inspect arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content: "Browser is not initialized. Use browser_open to start the browser.".to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}

async fn handle_browser_history(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    let params = serde_json::from_str(&arguments).ok();
    let sess_clone = sess;
    let arguments_clone = arguments.clone();
    let call_id_clone = call_id.clone();

    execute_custom_tool(
        sess,
        &sub_id,
        call_id,
        "browser_history".to_string(),
        params,
        || async move {
            let browser_manager = get_browser_manager_for_session(sess_clone).await;
            if let Some(browser_manager) = browser_manager {
                let args: Result<Value, _> = serde_json::from_str(&arguments_clone);
                match args {
                    Ok(json) => {
                        let direction =
                            json.get("direction").and_then(|v| v.as_str()).unwrap_or("");

                        if direction != "back" && direction != "forward" {
                            return ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone,
                                output: FunctionCallOutputPayload {
                                    content: format!(
                                        "Unsupported direction: {} (expected 'back' or 'forward')",
                                        direction
                                    ),
                                    success: Some(false),
                                },
                            };
                        }

                        let action_res = if direction == "back" {
                            browser_manager.history_back().await
                        } else {
                            browser_manager.history_forward().await
                        };

                        match action_res {
                            Ok(_) => {
                                ResponseInputItem::FunctionCallOutput {
                                    call_id: call_id_clone.clone(),
                                    output: FunctionCallOutputPayload {
                                        content: format!("History {} triggered", direction),
                                        success: Some(true),
                                    },
                                }
                            }
                            Err(e) => ResponseInputItem::FunctionCallOutput {
                                call_id: call_id_clone.clone(),
                                output: FunctionCallOutputPayload {
                                    content: format!("Failed to navigate history: {}", e),
                                    success: Some(false),
                                },
                            },
                        }
                    }
                    Err(e) => ResponseInputItem::FunctionCallOutput {
                        call_id: call_id_clone,
                        output: FunctionCallOutputPayload {
                            content: format!("Failed to parse browser_history arguments: {}", e),
                            success: Some(false),
                        },
                    },
                }
            } else {
                ResponseInputItem::FunctionCallOutput {
                    call_id: call_id_clone,
                    output: FunctionCallOutputPayload {
                        content:
                            "Browser is not initialized. Use browser_open to start the browser."
                                .to_string(),
                        success: Some(false),
                    },
                }
            }
        },
    )
    .await
}
