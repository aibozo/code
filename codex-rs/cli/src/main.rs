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
}

#[derive(Debug, Parser)]
struct MemoryLoginCommand {
    /// API key to save; if omitted prompts securely without echo.
    #[arg(long = "api-key", value_name = "API_KEY")]
    api_key: Option<String>,
}

#[derive(Debug, Parser)]
struct MemoryReindexCommand {}

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
            }
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
