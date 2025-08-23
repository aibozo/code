//! Types used to define the fields of [`crate::config::Config`].

// Note this file should generally be restricted to simple struct/enum
// definitions that do not contain business logic.

use std::collections::HashMap;
use std::path::PathBuf;
use wildmatch::WildMatchPattern;

use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;

/// Configuration for external agent models
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct AgentConfig {
    /// Name of the agent (e.g., "claude", "gemini", "gpt-4")
    pub name: String,

    /// Command to execute the agent (e.g., "claude", "gemini")
    pub command: String,

    /// Optional arguments to pass to the agent command
    #[serde(default)]
    pub args: Vec<String>,

    /// Whether this agent can only run in read-only mode
    #[serde(default)]
    pub read_only: bool,

    /// Whether this agent is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Optional description of the agent
    #[serde(default)]
    pub description: Option<String>,

    /// Optional environment variables for the agent
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    pub command: String,

    #[serde(default)]
    pub args: Vec<String>,

    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
}

#[derive(Deserialize, Debug, Copy, Clone, PartialEq)]
pub enum UriBasedFileOpener {
    #[serde(rename = "vscode")]
    VsCode,

    #[serde(rename = "vscode-insiders")]
    VsCodeInsiders,

    #[serde(rename = "windsurf")]
    Windsurf,

    #[serde(rename = "cursor")]
    Cursor,

    /// Option to disable the URI-based file opener.
    #[serde(rename = "none")]
    None,
}

impl UriBasedFileOpener {
    pub fn get_scheme(&self) -> Option<&str> {
        match self {
            UriBasedFileOpener::VsCode => Some("vscode"),
            UriBasedFileOpener::VsCodeInsiders => Some("vscode-insiders"),
            UriBasedFileOpener::Windsurf => Some("windsurf"),
            UriBasedFileOpener::Cursor => Some("cursor"),
            UriBasedFileOpener::None => None,
        }
    }
}

/// Settings that govern if and what will be written to `~/.codex/history.jsonl`.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct History {
    /// If true, history entries will not be written to disk.
    pub persistence: HistoryPersistence,

    /// If set, the maximum size of the history file in bytes.
    /// TODO(mbolin): Not currently honored.
    pub max_bytes: Option<usize>,
}

#[derive(Deserialize, Debug, Copy, Clone, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryPersistence {
    /// Save all history entries to disk.
    #[default]
    SaveAll,
    /// Do not write history to disk.
    None,
}

/// Collection of settings that are specific to the TUI.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Tui {
    /// Theme configuration for the TUI
    #[serde(default)]
    pub theme: ThemeConfig,
    
    /// Whether to show reasoning content expanded by default (can be toggled with Ctrl+R/T)
    #[serde(default)]
    pub show_reasoning: bool,
}

/// Theme configuration for the TUI
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ThemeConfig {
    /// Name of the predefined theme to use
    #[serde(default)]
    pub name: ThemeName,

    /// Custom color overrides (optional)
    #[serde(default)]
    pub colors: ThemeColors,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            name: ThemeName::default(),
            colors: ThemeColors::default(),
        }
    }
}

/// Available predefined themes
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeName {
    // Light themes (at top)
    #[default]
    LightPhoton,
    LightPrismRainbow,
    LightVividTriad,
    LightPorcelain,
    LightSandbar,
    LightGlacier,
    // Dark themes (below)
    DarkCarbonNight,
    DarkShinobiDusk,
    DarkOledBlackPro,
    DarkAmberTerminal,
    DarkAuroraFlux,
    DarkCharcoalRainbow,
    DarkZenGarden,
    DarkPaperLightPro,
    Custom,
}

/// Theme colors that can be customized
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ThemeColors {
    // Primary colors
    pub primary: Option<String>,
    pub secondary: Option<String>,
    pub background: Option<String>,
    pub foreground: Option<String>,

    // UI elements
    pub border: Option<String>,
    pub border_focused: Option<String>,
    pub selection: Option<String>,
    pub cursor: Option<String>,

    // Status colors
    pub success: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub info: Option<String>,

    // Text colors
    pub text: Option<String>,
    pub text_dim: Option<String>,
    pub text_bright: Option<String>,

    // Syntax/special colors
    pub keyword: Option<String>,
    pub string: Option<String>,
    pub comment: Option<String>,
    pub function: Option<String>,

    // Animation colors
    pub spinner: Option<String>,
    pub progress: Option<String>,
}

/// Browser configuration for integrated screenshot capabilities.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct BrowserConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default)]
    pub viewport: Option<BrowserViewportConfig>,

    #[serde(default)]
    pub wait: Option<BrowserWaitStrategy>,

    #[serde(default)]
    pub fullpage: bool,

    #[serde(default)]
    pub segments_max: Option<usize>,

    #[serde(default)]
    pub idle_timeout_ms: Option<u64>,

    #[serde(default)]
    pub format: Option<BrowserImageFormat>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct BrowserViewportConfig {
    pub width: u32,
    pub height: u32,

    #[serde(default)]
    pub device_scale_factor: Option<f64>,

    #[serde(default)]
    pub mobile: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum BrowserWaitStrategy {
    Event(String),
    Delay { delay_ms: u64 },
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BrowserImageFormat {
    Png,
    Webp,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct SandboxWorkspaceWrite {
    #[serde(default)]
    pub writable_roots: Vec<PathBuf>,
    #[serde(default)]
    pub network_access: bool,
    #[serde(default)]
    pub exclude_tmpdir_env_var: bool,
    #[serde(default)]
    pub exclude_slash_tmp: bool,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ShellEnvironmentPolicyInherit {
    /// "Core" environment variables for the platform. On UNIX, this would
    /// include HOME, LOGNAME, PATH, SHELL, and USER, among others.
    Core,

    /// Inherits the full environment from the parent process.
    #[default]
    All,

    /// Do not inherit any environment variables from the parent process.
    None,
}

/// Policy for building the `env` when spawning a process via either the
/// `shell` or `local_shell` tool.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ShellEnvironmentPolicyToml {
    pub inherit: Option<ShellEnvironmentPolicyInherit>,

    pub ignore_default_excludes: Option<bool>,

    /// List of regular expressions.
    pub exclude: Option<Vec<String>>,

    pub r#set: Option<HashMap<String, String>>,

    /// List of regular expressions.
    pub include_only: Option<Vec<String>>,

    pub experimental_use_profile: Option<bool>,
}

pub type EnvironmentVariablePattern = WildMatchPattern<'*', '?'>;

/// Deriving the `env` based on this policy works as follows:
/// 1. Create an initial map based on the `inherit` policy.
/// 2. If `ignore_default_excludes` is false, filter the map using the default
///    exclude pattern(s), which are: `"*KEY*"` and `"*TOKEN*"`.
/// 3. If `exclude` is not empty, filter the map using the provided patterns.
/// 4. Insert any entries from `r#set` into the map.
/// 5. If non-empty, filter the map using the `include_only` patterns.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShellEnvironmentPolicy {
    /// Starting point when building the environment.
    pub inherit: ShellEnvironmentPolicyInherit,

    /// True to skip the check to exclude default environment variables that
    /// contain "KEY" or "TOKEN" in their name.
    pub ignore_default_excludes: bool,

    /// Environment variable names to exclude from the environment.
    pub exclude: Vec<EnvironmentVariablePattern>,

    /// (key, value) pairs to insert in the environment.
    pub r#set: HashMap<String, String>,

    /// Environment variable names to retain in the environment.
    pub include_only: Vec<EnvironmentVariablePattern>,

    /// If true, the shell profile will be used to run the command.
    pub use_profile: bool,
}

impl From<ShellEnvironmentPolicyToml> for ShellEnvironmentPolicy {
    fn from(toml: ShellEnvironmentPolicyToml) -> Self {
        // Default to inheriting the full environment when not specified.
        let inherit = toml.inherit.unwrap_or(ShellEnvironmentPolicyInherit::All);
        let ignore_default_excludes = toml.ignore_default_excludes.unwrap_or(false);
        let exclude = toml
            .exclude
            .unwrap_or_default()
            .into_iter()
            .map(|s| EnvironmentVariablePattern::new_case_insensitive(&s))
            .collect();
        let r#set = toml.r#set.unwrap_or_default();
        let include_only = toml
            .include_only
            .unwrap_or_default()
            .into_iter()
            .map(|s| EnvironmentVariablePattern::new_case_insensitive(&s))
            .collect();
        let use_profile = toml.experimental_use_profile.unwrap_or(false);

        Self {
            inherit,
            ignore_default_excludes,
            exclude,
            r#set,
            include_only,
            use_profile,
        }
    }
}

/// See https://platform.openai.com/docs/guides/reasoning?api-mode=responses#get-started-with-reasoning
#[derive(Debug, Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    /// Minimal reasoning. Accepts legacy value "none" for backwards compatibility.
    #[serde(alias = "none")]
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    /// Deprecated: previously disabled reasoning. Kept for internal use only.
    #[serde(skip)]
    None,
}

/// A summary of the reasoning performed by the model. This can be useful for
/// debugging and understanding the model's reasoning process.
/// See https://platform.openai.com/docs/guides/reasoning?api-mode=responses#reasoning-summaries
#[derive(Debug, Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningSummary {
    #[default]
    Auto,
    Concise,
    Detailed,
    /// Option to disable reasoning summaries.
    None,
}

/// Text verbosity level for OpenAI API responses.
/// Controls the level of detail in the model's text responses.
#[derive(Debug, Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq, Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TextVerbosity {
    Low,
    #[default]
    Medium,
    High,
}

impl From<codex_protocol::config_types::ReasoningEffort> for ReasoningEffort {
    fn from(v: codex_protocol::config_types::ReasoningEffort) -> Self {
        match v {
            codex_protocol::config_types::ReasoningEffort::Minimal => ReasoningEffort::Minimal,
            codex_protocol::config_types::ReasoningEffort::Low => ReasoningEffort::Low,
            codex_protocol::config_types::ReasoningEffort::Medium => ReasoningEffort::Medium,
            codex_protocol::config_types::ReasoningEffort::High => ReasoningEffort::High,
        }
    }
}

impl From<codex_protocol::config_types::ReasoningSummary> for ReasoningSummary {
    fn from(v: codex_protocol::config_types::ReasoningSummary) -> Self {
        match v {
            codex_protocol::config_types::ReasoningSummary::Auto => ReasoningSummary::Auto,
            codex_protocol::config_types::ReasoningSummary::Concise => ReasoningSummary::Concise,
            codex_protocol::config_types::ReasoningSummary::Detailed => ReasoningSummary::Detailed,
            codex_protocol::config_types::ReasoningSummary::None => ReasoningSummary::None,
        }
    }
}

// ============================
// Memory (semantic compression)
// ============================

/// Injection budgeting for semantic compression (summaries/notes inserted into prompts).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct MemoryInjectConfig {
    #[serde(default = "MemoryInjectConfig::default_max_items")]
    pub max_items: usize,
    #[serde(default = "MemoryInjectConfig::default_max_chars")]
    pub max_chars: usize,
}

impl MemoryInjectConfig {
    fn default_max_items() -> usize { 2 }
    fn default_max_chars() -> usize { 500 }
}

impl Default for MemoryInjectConfig {
    fn default() -> Self {
        Self {
            max_items: Self::default_max_items(),
            max_chars: Self::default_max_chars(),
        }
    }
}

/// Embedding configuration for semantic search over summaries/code hints.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct MemoryEmbeddingConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "MemoryEmbeddingConfig::default_provider")]
    pub provider: String,
    #[serde(default = "MemoryEmbeddingConfig::default_top_k")]
    pub top_k: usize,
    #[serde(default = "MemoryEmbeddingConfig::default_dim")]
    pub dim: usize,
}

impl MemoryEmbeddingConfig {
    fn default_provider() -> String { "openai".to_string() }
    fn default_top_k() -> usize { 5 }
    fn default_dim() -> usize { 3072 }
}

impl Default for MemoryEmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Self::default_provider(),
            top_k: Self::default_top_k(),
            dim: Self::default_dim(),
        }
    }
}

/// Code index configuration (optional semantic codebase index).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct MemoryCodeIndexConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "MemoryCodeIndexConfig::default_chunk_bytes")]
    pub chunk_bytes: usize,
    #[serde(default = "MemoryCodeIndexConfig::default_top_k")]
    pub top_k: usize,
}

impl MemoryCodeIndexConfig {
    fn default_chunk_bytes() -> usize { 1500 }
    fn default_top_k() -> usize { 5 }
}

impl Default for MemoryCodeIndexConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            chunk_bytes: Self::default_chunk_bytes(),
            top_k: Self::default_top_k(),
        }
    }
}

/// Top-level configuration for semantic compression features.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct MemoryConfig {
    /// Master toggle for all memory features (disabled by default).
    #[serde(default)]
    pub enabled: bool,

    /// When `true`, summarize the pruned portion of the conversation and persist.
    #[serde(default = "MemoryConfig::default_summarize_on_prune")]
    pub summarize_on_prune: bool,

    /// Prompt injection budget.
    #[serde(default)]
    pub inject: MemoryInjectConfig,

    /// Embedding-backed retrieval settings (Phase 2, optional).
    #[serde(default)]
    pub embedding: MemoryEmbeddingConfig,

    /// Code index settings (Phase 3, optional).
    #[serde(default)]
    pub code_index: MemoryCodeIndexConfig,

    /// Enable fuzzy de-duplication between code and memory retrieval sections.
    /// When true, memory bullets that highly overlap with code bullets are removed.
    /// Defaults to true.
    #[serde(default = "MemoryConfig::default_fuzzy_dedupe_enabled")]
    pub fuzzy_dedupe_enabled: bool,

    /// Jaccard similarity threshold for title-based fuzzy de-duplication (0.0–1.0).
    /// Applies only when titles are not exactly equal.
    #[serde(default = "MemoryConfig::default_fuzzy_title_jaccard")]
    pub fuzzy_dedupe_title_jaccard: f32,

    /// Jaccard similarity threshold for content-based fuzzy de-duplication (0.0–1.0).
    #[serde(default = "MemoryConfig::default_fuzzy_content_jaccard")]
    pub fuzzy_dedupe_content_jaccard: f32,

    /// Minimum prefix length for the strong containment heuristic. If one text contains the
    /// other's first N characters (after trimming), the items are considered duplicates.
    #[serde(default = "MemoryConfig::default_fuzzy_min_containment_prefix")]
    pub fuzzy_dedupe_min_containment_prefix: usize,

    /// Weight for the recency component when blending ranking scores (0.0–1.0).
    /// Final score = (1 - alpha) * similarity + alpha * recency.
    #[serde(default = "MemoryConfig::default_recency_blend_alpha")]
    pub recency_blend_alpha: f32,

    /// Half-life in days used by the recency decay: recency = exp(-ln(2) * age_days / half_life_days)
    #[serde(default = "MemoryConfig::default_recency_half_life_days")]
    pub recency_half_life_days: f32,

    /// Number of recent conversation items to keep uncompressed in history.
    /// Older items may be summarized and pruned.
    #[serde(default = "MemoryConfig::default_keep_last_messages")]
    pub keep_last_messages: usize,

    /// Character budget for the compact summary stored when pruning.
    /// This does not affect the prompt injection budget.
    #[serde(default = "MemoryConfig::default_summary_max_chars")]
    pub summary_max_chars: usize,

    /// When true, use an LLM (via OpenAI API) to produce summaries when pruning.
    /// Falls back to a local compact summarizer on failure.
    #[serde(default = "MemoryConfig::default_use_llm_summarizer")]
    pub use_llm_summarizer: bool,

    /// Model to use for LLM summaries (OpenAI).
    #[serde(default = "MemoryConfig::default_summarizer_model")]
    pub summarizer_model: String,

    /// Preflight compaction thresholds as percentages of the effective window.
    /// When the estimated formatted input exceeds the lowest threshold, preflight
    /// compaction is applied until the target percentage is reached.
    #[serde(default = "MemoryConfig::default_compact_threshold_pct")]
    pub compact_threshold_pct: Vec<u8>,

    /// Target percentage for hysteresis when compacting. Should be lower than
    /// the lowest threshold to avoid thrash.
    #[serde(default = "MemoryConfig::default_compact_target_pct")]
    pub compact_target_pct: u8,

    /// Character budget for per‑volley summaries used by preflight compaction.
    #[serde(default = "MemoryConfig::default_summary_max_chars_per_volley")]
    pub summary_max_chars_per_volley: usize,

    /// Maximum number of volley summaries to insert in a single request during
    /// preflight compaction.
    #[serde(default = "MemoryConfig::default_max_summaries_per_request")]
    pub max_summaries_per_request: usize,
}

impl MemoryConfig {
    fn default_summarize_on_prune() -> bool { true }
    fn default_fuzzy_dedupe_enabled() -> bool { true }
    fn default_fuzzy_title_jaccard() -> f32 { 0.97 }
    fn default_fuzzy_content_jaccard() -> f32 { 0.92 }
    fn default_fuzzy_min_containment_prefix() -> usize { 32 }
    fn default_recency_blend_alpha() -> f32 { 0.15 }
    fn default_recency_half_life_days() -> f32 { 7.0 }
    fn default_keep_last_messages() -> usize { 20 }
    fn default_summary_max_chars() -> usize { 400 }
    fn default_use_llm_summarizer() -> bool { true }
    fn default_summarizer_model() -> String { "gpt-5-nano".to_string() }
    fn default_compact_threshold_pct() -> Vec<u8> { vec![75, 85, 95] }
    fn default_compact_target_pct() -> u8 { 70 }
    fn default_summary_max_chars_per_volley() -> usize { 400 }
    fn default_max_summaries_per_request() -> usize { 5 }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            summarize_on_prune: Self::default_summarize_on_prune(),
            inject: MemoryInjectConfig::default(),
            embedding: MemoryEmbeddingConfig::default(),
            code_index: MemoryCodeIndexConfig::default(),
            fuzzy_dedupe_enabled: Self::default_fuzzy_dedupe_enabled(),
            fuzzy_dedupe_title_jaccard: Self::default_fuzzy_title_jaccard(),
            fuzzy_dedupe_content_jaccard: Self::default_fuzzy_content_jaccard(),
            fuzzy_dedupe_min_containment_prefix: Self::default_fuzzy_min_containment_prefix(),
            recency_blend_alpha: Self::default_recency_blend_alpha(),
            recency_half_life_days: Self::default_recency_half_life_days(),
            keep_last_messages: Self::default_keep_last_messages(),
            summary_max_chars: Self::default_summary_max_chars(),
            use_llm_summarizer: Self::default_use_llm_summarizer(),
            summarizer_model: Self::default_summarizer_model(),
            compact_threshold_pct: Self::default_compact_threshold_pct(),
            compact_target_pct: Self::default_compact_target_pct(),
            summary_max_chars_per_volley: Self::default_summary_max_chars_per_volley(),
            max_summaries_per_request: Self::default_max_summaries_per_request(),
        }
    }
}
