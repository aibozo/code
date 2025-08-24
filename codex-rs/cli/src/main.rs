use clap::CommandFactory;
use clap::Parser;
use clap_complete::Shell;
use clap_complete::generate;
use codex_arg0::arg0_dispatch_or_else;
use codex_chatgpt::apply_command::ApplyCommand;
use codex_chatgpt::apply_command::run_apply_command;
use codex_cli::LandlockCommand;
use codex_cli::SeatbeltCommand;
use codex_cli::login::run_login_status;
use codex_cli::login::run_login_with_api_key;
use codex_cli::login::run_login_with_chatgpt;
use codex_cli::login::run_logout;
use codex_cli::proto;
use codex_common::CliConfigOverrides;
use codex_exec::Cli as ExecCli;
use codex_tui::Cli as TuiCli;
use std::path::PathBuf;

use crate::proto::ProtoCli;

/// Codex CLI
///
/// If no subcommand is specified, options will be forwarded to the interactive CLI.
#[derive(Debug, Parser)]
#[clap(
    author,
    name = "code",
    version = env!("CARGO_PKG_VERSION"),
    // If a sub‑command is given, ignore requirements of the default args.
    subcommand_negates_reqs = true,
    // The executable is sometimes invoked via a platform‑specific name like
    // `codex-x86_64-unknown-linux-musl`, but the help output should always use
    // the generic `codex` command name that users run.
    bin_name = "code"
)]
struct MultitoolCli {
    #[clap(flatten)]
    pub config_overrides: CliConfigOverrides,

    #[clap(flatten)]
    interactive: TuiCli,

    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    /// Run Codex non-interactively.
    #[clap(visible_alias = "e")]
    Exec(ExecCli),

    /// Manage login.
    Login(LoginCommand),

    /// Remove stored authentication credentials.
    Logout(LogoutCommand),

    /// Experimental: run Codex as an MCP server.
    Mcp,

    /// Run the Protocol stream via stdin/stdout
    #[clap(visible_alias = "p")]
    Proto(ProtoCli),

    /// Generate shell completion scripts.
    Completion(CompletionCommand),

    /// Internal debugging commands.
    Debug(DebugArgs),

    /// Apply the latest diff produced by Codex agent as a `git apply` to your local working tree.
    #[clap(visible_alias = "a")]
    Apply(ApplyCommand),

    /// Internal: generate TypeScript protocol bindings.
    #[clap(hide = true)]
    GenerateTs(GenerateTsCommand),

    /// Memory utilities (semantic compression helpers).
    Memory(MemoryCommand),

    /// Run the evaluation harness.
    Harness(HarnessCommand),

    /// Run an orchestrated improvement cycle (M4 supervisor).
    Improve(ImproveCommand),
}

#[derive(Debug, Parser)]
struct CompletionCommand {
    /// Shell to generate completions for
    #[clap(value_enum, default_value_t = Shell::Bash)]
    shell: Shell,
}

#[derive(Debug, Parser)]
struct DebugArgs {
    #[command(subcommand)]
    cmd: DebugCommand,
}

#[derive(Debug, clap::Subcommand)]
enum DebugCommand {
    /// Run a command under Seatbelt (macOS only).
    Seatbelt(SeatbeltCommand),

    /// Run a command under Landlock+seccomp (Linux only).
    Landlock(LandlockCommand),

    /// Inspect the computed request endpoint and instructions policy without sending a request.
    InspectRequest(InspectRequestArgs),
}

#[derive(Debug, Parser)]
struct LoginCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[arg(long = "api-key", value_name = "API_KEY")]
    api_key: Option<String>,

    #[command(subcommand)]
    action: Option<LoginSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum LoginSubcommand {
    /// Show login status.
    Status,
}

#[derive(Debug, Parser)]
struct LogoutCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
}

#[derive(Debug, Parser)]
struct GenerateTsCommand {
    /// Output directory where .ts files will be written
    #[arg(short = 'o', long = "out", value_name = "DIR")]
    out_dir: PathBuf,

    /// Optional path to the Prettier executable to format generated files
    #[arg(short = 'p', long = "prettier", value_name = "PRETTIER_BIN")]
    prettier: Option<PathBuf>,
}

#[derive(Debug, Parser)]
struct MemoryCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    action: MemorySubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum MemorySubcommand {
    /// Store an OpenAI API key for embeddings (saved to ~/.codex/auth.json).
    Login(MemoryLoginCommand),

    /// Rebuild the code index for the current project (code-kind vectors only).
    Reindex(MemoryReindexCommand),

    /// Reingest graph memory from existing runs and episodes in the repo.
    Reingest(MemoryReingestCommand),
}

#[derive(Debug, Parser)]
struct MemoryLoginCommand {
    /// API key to save; if omitted prompts securely without echo.
    #[arg(long = "api-key", value_name = "API_KEY")]
    api_key: Option<String>,
}

#[derive(Debug, Parser)]
struct MemoryReindexCommand {}

#[derive(Debug, Parser)]
struct MemoryReingestCommand {}

#[derive(Debug, Parser)]
struct HarnessCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,

    #[command(subcommand)]
    action: HarnessSubcommand,
}

#[derive(Debug, Parser)]
struct InspectRequestArgs {
    /// Auth mode to simulate: auto|chatgpt|api
    #[arg(long = "auth", value_name = "MODE")]
    auth_mode: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
enum HarnessSubcommand {
    /// Run harness stages and write a result JSON.
    Run(HarnessRunCommand),
}

#[derive(Debug, Parser)]
struct HarnessRunCommand {
    /// RNG seed for deterministic runs.
    #[arg(long = "seed", value_name = "N")]
    seed: Option<u64>,

    /// Disable stage cache to force fresh execution.
    #[arg(long = "no-cache")]
    no_cache: bool,

    /// Optional context label (e.g., branch name) to include in results.
    #[arg(long = "context", value_name = "NAME")]
    context: Option<String>,
}

#[derive(Debug, Parser)]
struct ImproveCommand {
    #[clap(skip)]
    config_overrides: CliConfigOverrides,
    /// High-level goal for the improvement cycle.
    pub goal: String,

    /// Maximum number of attempts to try before stopping.
    #[arg(long = "max-attempts", value_name = "N", default_value_t = 1)]
    max_attempts: usize,

    /// Execute without asking for approvals for each write (policy still enforced by profile).
    #[arg(long = "no-approval", default_value_t = false)]
    no_approval: bool,

    /// Optional wall-time budget in seconds for the entire improve loop.
    #[arg(long = "wall-time", value_name = "SECS")]
    wall_time_secs: Option<u64>,

    /// Optional token budget target for subagents (not enforced in MVP).
    #[arg(long = "token-budget", value_name = "TOKENS")]
    token_budget: Option<u64>,

    /// Optional concurrency cap for subagents (not used in MVP).
    #[arg(long = "concurrency", value_name = "N")]
    concurrency: Option<u32>,

    /// Optional context label for harness runs (e.g., branch, task).
    #[arg(long = "context", value_name = "NAME")]
    context: Option<String>,
}

fn main() -> anyhow::Result<()> {
    arg0_dispatch_or_else(|codex_linux_sandbox_exe| async move {
        cli_main(codex_linux_sandbox_exe).await?;
        Ok(())
    })
}

async fn cli_main(codex_linux_sandbox_exe: Option<PathBuf>) -> anyhow::Result<()> {
    let cli = MultitoolCli::parse();

    match cli.subcommand {
        None => {
            let mut tui_cli = cli.interactive;
            prepend_config_flags(&mut tui_cli.config_overrides, cli.config_overrides);
            let usage = codex_tui::run_main(tui_cli, codex_linux_sandbox_exe).await?;
            if !usage.is_zero() {
                println!("{}", codex_core::protocol::FinalOutput::from(usage));
            }
        }
        Some(Subcommand::Exec(mut exec_cli)) => {
            prepend_config_flags(&mut exec_cli.config_overrides, cli.config_overrides);
            codex_exec::run_main(exec_cli, codex_linux_sandbox_exe).await?;
        }
        Some(Subcommand::Mcp) => {
            codex_mcp_server::run_main(codex_linux_sandbox_exe, cli.config_overrides).await?;
        }
        Some(Subcommand::Login(mut login_cli)) => {
            prepend_config_flags(&mut login_cli.config_overrides, cli.config_overrides);
            match login_cli.action {
                Some(LoginSubcommand::Status) => {
                    run_login_status(login_cli.config_overrides).await;
                }
                None => {
                    if let Some(api_key) = login_cli.api_key {
                        run_login_with_api_key(login_cli.config_overrides, api_key).await;
                    } else {
                        run_login_with_chatgpt(login_cli.config_overrides).await;
                    }
                }
            }
        }
        Some(Subcommand::Logout(mut logout_cli)) => {
            prepend_config_flags(&mut logout_cli.config_overrides, cli.config_overrides);
            run_logout(logout_cli.config_overrides).await;
        }
        Some(Subcommand::Proto(mut proto_cli)) => {
            prepend_config_flags(&mut proto_cli.config_overrides, cli.config_overrides);
            proto::run_main(proto_cli).await?;
        }
        Some(Subcommand::Completion(completion_cli)) => {
            print_completion(completion_cli);
        }
        Some(Subcommand::Debug(debug_args)) => match debug_args.cmd {
            DebugCommand::Seatbelt(mut seatbelt_cli) => {
                prepend_config_flags(&mut seatbelt_cli.config_overrides, cli.config_overrides);
                codex_cli::debug_sandbox::run_command_under_seatbelt(
                    seatbelt_cli,
                    codex_linux_sandbox_exe,
                )
                .await?;
            }
            DebugCommand::Landlock(mut landlock_cli) => {
                prepend_config_flags(&mut landlock_cli.config_overrides, cli.config_overrides);
                codex_cli::debug_sandbox::run_command_under_landlock(
                    landlock_cli,
                    codex_linux_sandbox_exe,
                )
                .await?;
            }
            DebugCommand::InspectRequest(mut args) => {
                let mut tui_cli = cli.interactive; // reuse overrides container
                prepend_config_flags(&mut tui_cli.config_overrides, cli.config_overrides);
                debug_inspect_request(tui_cli.config_overrides, args).await?;
            }
        },
        Some(Subcommand::Apply(mut apply_cli)) => {
            prepend_config_flags(&mut apply_cli.config_overrides, cli.config_overrides);
            run_apply_command(apply_cli, None).await?;
        }
        Some(Subcommand::GenerateTs(gen_cli)) => {
            codex_protocol_ts::generate_ts(&gen_cli.out_dir, gen_cli.prettier.as_deref())?;
        }
        Some(Subcommand::Memory(mut mem_cli)) => {
            prepend_config_flags(&mut mem_cli.config_overrides, cli.config_overrides);
            match mem_cli.action {
                MemorySubcommand::Login(login_cmd) => {
                    memory_login(mem_cli.config_overrides, login_cmd).await?;
                }
                MemorySubcommand::Reindex(_cmd) => {
                    memory_reindex(mem_cli.config_overrides).await?;
                }
                MemorySubcommand::Reingest(_cmd) => {
                    memory_reingest(mem_cli.config_overrides).await?;
                }
            }
        }
        Some(Subcommand::Harness(mut h_cli)) => {
            prepend_config_flags(&mut h_cli.config_overrides, cli.config_overrides);
            match h_cli.action {
                HarnessSubcommand::Run(cmd) => {
                    harness_run(h_cli.config_overrides, cmd).await?;
                }
            }
        }
        Some(Subcommand::Improve(mut imp_cli)) => {
            prepend_config_flags(&mut imp_cli.config_overrides, cli.config_overrides);
            let cfg_overrides = std::mem::take(&mut imp_cli.config_overrides);
            improve_run(cfg_overrides, imp_cli).await?;
        }
    }

    Ok(())
}

async fn memory_login(
    cli_config_overrides: CliConfigOverrides,
    login_cmd: MemoryLoginCommand,
) -> anyhow::Result<()> {
    // Load config to discover CODEX_HOME.
    let config = {
        use codex_core::config::ConfigOverrides;
        let overrides = ConfigOverrides::default();
        let kv = cli_config_overrides
            .parse_overrides()
            .map_err(|e| anyhow::anyhow!(e))?;
        codex_core::config::Config::load_with_cli_overrides(kv, overrides)?
    };

    // Prompt for key if not provided.
    let api_key = match login_cmd.api_key {
        Some(k) => k,
        None => {
            let prompt = "Enter OpenAI API key (sk-...): ";
            let k = prompt_secret(prompt)?;
            if k.trim().is_empty() {
                anyhow::bail!("No API key entered");
            }
            k
        }
    };

    // Persist using existing login infrastructure.
    codex_login::login_with_api_key(&config.codex_home, &api_key)?;
    eprintln!("Saved API key to {}", config.codex_home.join("auth.json").display());
    Ok(())
}

async fn memory_reindex(cli_config_overrides: CliConfigOverrides) -> anyhow::Result<()> {
    // Load config with overrides
    let overrides = cli_config_overrides
        .parse_overrides()
        .map_err(|e| anyhow::anyhow!(e))?;
    let cfg = codex_core::config::Config::load_with_cli_overrides(
        overrides,
        codex_core::config::ConfigOverrides::default(),
    )?;

    // Ensure key present
    if !codex_core::memory::openai_embeddings::has_openai_api_key(&cfg.codex_home) {
        anyhow::bail!("OpenAI API key missing. Run: code memory login");
    }

    let repo_key = codex_core::util::repo_key(&cfg.cwd);
    let dim = cfg.memory.embedding.dim;
    let chunk_bytes = cfg.memory.code_index.chunk_bytes;

    // Run reindex (best effort)
    match codex_core::memory::code_index::rebuild_code_index(&repo_key, &cfg.codex_home, &cfg.cwd, dim, chunk_bytes) {
        Ok(_) => {
            eprintln!("Rebuilt code index for {repo_key}");
        }
        Err(e) => {
            eprintln!("Failed to rebuild code index: {e}");
        }
    }
    Ok(())
}

async fn memory_reingest(cli_config_overrides: CliConfigOverrides) -> anyhow::Result<()> {
    // Load config with overrides
    let overrides = cli_config_overrides
        .parse_overrides()
        .map_err(|e| anyhow::anyhow!(e))?;
    let cfg = codex_core::config::Config::load_with_cli_overrides(
        overrides,
        codex_core::config::ConfigOverrides::default(),
    )?;

    let (runs, eps) = match codex_memory::graph::ingest::reingest_repo(&cfg.cwd) {
        Ok(v) => v,
        Err(e) => anyhow::bail!(format!("Reingest failed: {}", e)),
    };
    println!("Reingested graph: runs {} • episodes {}", runs, eps);
    Ok(())
}

fn prompt_secret(prompt: &str) -> std::io::Result<String> {
    use std::io::{self, Write};
    print!("{}", prompt);
    io::stdout().flush()?;
    #[cfg(unix)]
    {
        use libc::{c_int, tcgetattr, tcsetattr, termios, ECHO, TCSANOW};
        unsafe {
            let fd: c_int = libc::STDIN_FILENO;
            let mut term: termios = std::mem::zeroed();
            if tcgetattr(fd, &mut term) != 0 {
                // Fallback to visible read
                let mut s = String::new();
                io::stdin().read_line(&mut s)?;
                return Ok(s.trim_end_matches(['\n','\r']).to_string());
            }
            let old = term;
            term.c_lflag &= !ECHO;
            let _ = tcsetattr(fd, TCSANOW, &term);
            let mut s = String::new();
            let res = io::stdin().read_line(&mut s);
            let _ = tcsetattr(fd, TCSANOW, &old);
            println!(""); // move to next line
            res.map(|_| s.trim_end_matches(['\n','\r']).to_string())
        }
    }
    #[cfg(not(unix))]
    {
        // Fallback: echo remains on
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        println!("");
        Ok(s.trim_end_matches(['\n','\r']).to_string())
    }
}

/// Prepend root-level overrides so they have lower precedence than
/// CLI-specific ones specified after the subcommand (if any).
fn prepend_config_flags(
    subcommand_config_overrides: &mut CliConfigOverrides,
    cli_config_overrides: CliConfigOverrides,
) {
    subcommand_config_overrides
        .raw_overrides
        .splice(0..0, cli_config_overrides.raw_overrides);
}

fn print_completion(cmd: CompletionCommand) {
    let mut app = MultitoolCli::command();
    let name = "codex";
    generate(cmd.shell, &mut app, name, &mut std::io::stdout());
}

async fn harness_run(
    cli_config_overrides: CliConfigOverrides,
    cmd: HarnessRunCommand,
) -> anyhow::Result<()> {
    // Load config to resolve cwd (repo root)
    let cfg = codex_core::config::Config::load_with_cli_overrides(
        cli_config_overrides
            .parse_overrides()
            .map_err(|e| anyhow::anyhow!(e))?,
        codex_core::config::ConfigOverrides::default(),
    )?;
    let harness = cfg.cwd.join("harness").join("run.sh");
    if !harness.is_file() {
        anyhow::bail!(format!("Harness runner not found at {}", harness.display()));
    }
    let mut command = tokio::process::Command::new(&harness);
    command.current_dir(&cfg.cwd);
    if let Some(seed) = cmd.seed {
        command.arg("--seed").arg(seed.to_string());
    }
    if cmd.no_cache {
        command.arg("--no-cache");
    }
    if let Some(ctx) = cmd.context.as_deref() {
        command.arg("--context").arg(ctx);
    }
    let status = command.status().await?;
    if !status.success() {
        anyhow::bail!(format!("Harness run failed with status {}", status));
    }
    Ok(())
}

async fn debug_inspect_request(
    cli_config_overrides: CliConfigOverrides,
    args: InspectRequestArgs,
) -> anyhow::Result<()> {
    // Load config with overrides
    let overrides = cli_config_overrides
        .parse_overrides()
        .map_err(|e| anyhow::anyhow!(e))?;
    let cfg = codex_core::config::Config::load_with_cli_overrides(
        overrides,
        codex_core::config::ConfigOverrides::default(),
    )?;

    // Determine auth mode per arg or availability
    use codex_login::AuthMode;
    let desired = args
        .auth_mode
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "auto".to_string());

    let auth = match desired.as_str() {
        "chatgpt" => codex_login::CodexAuth::from_codex_home(&cfg.codex_home, AuthMode::ChatGPT)?,
        "api" | "apikey" => codex_login::CodexAuth::from_codex_home(&cfg.codex_home, AuthMode::ApiKey)?,
        _ => {
            // auto: prefer ChatGPT when present, else API key, else none
            match codex_login::CodexAuth::from_codex_home(&cfg.codex_home, AuthMode::ChatGPT)? {
                Some(a) => Some(a),
                None => codex_login::CodexAuth::from_codex_home(&cfg.codex_home, AuthMode::ApiKey)?,
            }
        }
    };

    let auth_mode = auth.as_ref().map(|a| a.mode);
    // Reconstruct endpoint selection logic locally (mirrors core behavior)
    let default_base = if matches!(auth_mode, Some(AuthMode::ChatGPT)) {
        "https://chatgpt.com/backend-api/codex".to_string()
    } else {
        "https://api.openai.com/v1".to_string()
    };
    let base_url = match auth_mode {
        Some(AuthMode::ChatGPT) => {
            match &cfg.model_provider.base_url {
                Some(url) if url.contains("chatgpt") || url.contains("/codex") => url.clone(),
                _ => default_base.clone(),
            }
        }
        _ => cfg
            .model_provider
            .base_url
            .clone()
            .unwrap_or(default_base.clone()),
    };
    let wire_api = format!("{:?}", cfg.model_provider.wire_api).to_lowercase();
    let endpoint = match cfg.model_provider.wire_api {
        codex_core::WireApi::Responses => format!("{}/responses", base_url),
        codex_core::WireApi::Chat => format!("{}/chat/completions", base_url),
    };

    // Indicate which instructions policy would be used
    let policy = match auth_mode {
        Some(AuthMode::ChatGPT) => "chatgpt_minimal",
        Some(AuthMode::ApiKey) => "full",
        None => "none",
    };

    // Print a compact JSON diagnostic to stdout (no secrets)
    let auth_mode_s = auth_mode
        .map(|m| match m { AuthMode::ChatGPT => "chatgpt", AuthMode::ApiKey => "apikey" })
        .unwrap_or("none");

    println!(
        "{}",
        serde_json::json!({
            "auth_mode": auth_mode_s,
            "wire_api": wire_api,
            "endpoint": endpoint,
            "instructions_policy": policy,
        })
    );

    Ok(())
}

async fn improve_run(
    cli_config_overrides: CliConfigOverrides,
    cmd: ImproveCommand,
) -> anyhow::Result<()> {
    // Load config (cwd, policy) from overrides
    let cfg = codex_core::config::Config::load_with_cli_overrides(
        cli_config_overrides
            .parse_overrides()
            .map_err(|e| anyhow::anyhow!(e))?,
        codex_core::config::ConfigOverrides::default(),
    )?;

    let opts = codex_core::orchestrator::ImproveOptions {
        goal: cmd.goal,
        max_attempts: cmd.max_attempts.max(1),
        no_approval: cmd.no_approval,
        budgets: codex_core::orchestrator::Budgets {
            wall_time_secs: cmd.wall_time_secs,
            token_budget: cmd.token_budget,
            max_concurrency: cmd.concurrency,
        },
        context_label: cmd.context,
    };
    let res = codex_core::orchestrator::run_improve(&cfg, opts).await?;
    println!(
        "Improve: accepted={} attempts={} last={}/{} Δok={} Δerr={}",
        res.accepted,
        res.attempts,
        res.last_run_day.unwrap_or_else(|| "n/a".to_string()),
        res.last_run_ts.unwrap_or_else(|| "n/a".to_string()),
        res.delta_ok.map(|v| v.to_string()).unwrap_or_else(|| "n/a".to_string()),
        res.delta_error.map(|v| v.to_string()).unwrap_or_else(|| "n/a".to_string()),
    );
    Ok(())
}
