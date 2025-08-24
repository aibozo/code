use strum::IntoEnumIterator;
use strum_macros::AsRefStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use strum_macros::IntoStaticStr;

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Tgm,
    Checkpoint,
    Relaunch,
    Browser,
    Chrome,
    New,
    Init,
    Compact,
    Diff,
    Mention,
    Status,
    Theme,
    Reasoning,
    Verbosity,
    Prompts,
    Perf,
    Memory,
    Harness,
    Loop,
    Improve,
    Reports,
    Safety,
    Approvals,
    Accept,
    Revert,
    // Prompt-expanding commands
    Plan,
    Act,
    Solve,
    Reflect,
    Code,
    Logout,
    Quit,
    #[cfg(debug_assertions)]
    TestApproval,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::Tgm => "toggle GODMODE (/tgm on|off|status)",
            SlashCommand::Checkpoint => "create a checkpoint (M6)",
            SlashCommand::Relaunch => "restore a checkpoint (M6)",
            SlashCommand::Chrome => "connect to Chrome",
            SlashCommand::Browser => "open internal browser",
            SlashCommand::Plan => "create a comprehensive plan (multiple agents)",
            SlashCommand::Act => "implement changes via apply_patch (respect approvals)",
            SlashCommand::Solve => "solve a challenging problem (multiple agents)",
            SlashCommand::Reflect => "summarize results and next steps",
            SlashCommand::Code => "perform a coding task (multiple agents)",
            SlashCommand::Reasoning => "change reasoning effort (minimal/low/medium/high)",
            SlashCommand::Verbosity => "change text verbosity (high/medium/low)",
            SlashCommand::New => "start a new chat during a conversation",
            SlashCommand::Init => "create an AGENTS.md file with instructions for Codex",
            SlashCommand::Compact => "summarize conversation to prevent hitting the context limit",
            SlashCommand::Quit => "exit Codex",
            SlashCommand::Diff => "show git diff (including untracked files)",
            SlashCommand::Mention => "mention a file",
            SlashCommand::Status => "show current session configuration and token usage",
            SlashCommand::Theme => "switch between color themes",
            SlashCommand::Prompts => "show example prompts",
            SlashCommand::Perf => "performance tracing (on/off/show/reset)",
            SlashCommand::Memory => "configure memory (keep-last, summary)",
            SlashCommand::Harness => "run the evaluation harness",
            SlashCommand::Loop => "manage the perpetual self-improvement loop",
            SlashCommand::Improve => "run orchestrator improvement cycle (M4)",
            SlashCommand::Logout => "log out of Codex",
            SlashCommand::Reports => "show harness reports",
            SlashCommand::Safety => "show safety status and recent logs",
            SlashCommand::Approvals => "manage pending approvals (approve/deny)",
            SlashCommand::Accept => "accept current session changes",
            SlashCommand::Revert => "revert current session changes",
            #[cfg(debug_assertions)]
            SlashCommand::TestApproval => "test approval request",
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Returns true if this command should expand into a prompt for the LLM.
    pub fn is_prompt_expanding(self) -> bool {
        matches!(
            self,
            SlashCommand::Plan | SlashCommand::Act | SlashCommand::Solve | SlashCommand::Reflect | SlashCommand::Code
        )
    }

    /// Returns true if this command requires additional arguments after the command.
    pub fn requires_arguments(self) -> bool {
        matches!(
            self,
            SlashCommand::Plan | SlashCommand::Solve | SlashCommand::Code
        )
    }

    /// Expands a prompt-expanding command into a full prompt for the LLM.
    /// Returns None if the command is not a prompt-expanding command.
    pub fn expand_prompt(self, args: &str) -> Option<String> {
        if !self.is_prompt_expanding() {
            return None;
        }

        // Use the slash_commands module from core to generate the prompts
        // Note: We pass None for agents here as the TUI doesn't have access to the session config
        // The actual agents will be determined when the agent tool is invoked
        match self {
            SlashCommand::Plan => Some(codex_core::slash_commands::format_plan_command(
                args, None, None,
            )),
            // Encourage the model to use the apply_patch tool with unified diffs and respect approvals.
            SlashCommand::Act => Some({
                let task = args;
                format!(
                    concat!(
                        "Act on the following task by proposing minimal, focused code changes.\n",
                        "Use the apply_patch tool to provide a unified diff (git-style) patch.\n",
                        "Constraints:\n",
                        "- Keep diffs small, scoped, and reversible.\n",
                        "- Include file adds/updates/moves only as needed.\n",
                        "- Respect approval policy; do not bypass sandboxing.\n",
                        "- Provide only the patch via the tool; keep prose in assistant output concise.\n",
                        "- End your turn when you are confident in the proposed change set.\n",
                        "- Do not wait for time-based events or long-running loops.\n",
                        "Task:\n{}\n"
                    ),
                    task
                )
            }),
            SlashCommand::Solve => Some(codex_core::slash_commands::format_solve_command(
                args, None, None,
            )),
            SlashCommand::Reflect => Some({
                let topic = args;
                format!(
                    concat!(
                        "Reflect on the latest harness run and recent edits.\n",
                        "Summarize outcomes (passes, failures, cached stages), key changes, and propose next targets.\n",
                        "Be concise and actionable; end the turn when your reflection is complete.\n",
                        "Topic/context:\n{}\n"
                    ),
                    topic
                )
            }),
            SlashCommand::Code => Some(codex_core::slash_commands::format_code_command(
                args, None, None,
            )),
            _ => None,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter().map(|c| (c.command(), c)).collect()
}

/// Process a message that might contain a slash command.
/// Returns either the expanded prompt (for prompt-expanding commands) or the original message.
pub fn process_slash_command_message(message: &str) -> ProcessedCommand {
    let trimmed = message.trim();

    // Check if it starts with a slash
    if !trimmed.starts_with('/') {
        return ProcessedCommand::NotCommand(message.to_string());
    }

    // Parse the command and arguments
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    let command_str = &parts[0][1..]; // Remove the leading '/'
    let args = parts.get(1).map(|s| s.to_string()).unwrap_or_default();

    // Try to parse the command
    if let Ok(command) = command_str.parse::<SlashCommand>() {
        // Check if it's a prompt-expanding command
        if command.is_prompt_expanding() {
            if args.is_empty() && command.requires_arguments() {
                return ProcessedCommand::Error(format!(
                    "Error: /{} requires a task description. Usage: /{} <task>",
                    command.command(),
                    command.command()
                ));
            }

            if let Some(expanded) = command.expand_prompt(&args) {
                return ProcessedCommand::ExpandedPrompt(expanded);
            }
        }

        // It's a regular command, return it as-is
        ProcessedCommand::RegularCommand(command, args)
    } else {
        // Unknown command
        ProcessedCommand::NotCommand(message.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum ProcessedCommand {
    /// The message was expanded from a prompt-expanding slash command
    ExpandedPrompt(String),
    /// A regular slash command that should be handled by the TUI
    RegularCommand(SlashCommand, String),
    /// Not a slash command, just a regular message
    #[allow(dead_code)]
    NotCommand(String),
    /// Error processing the command
    Error(String),
}
