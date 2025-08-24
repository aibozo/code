#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
use std::time::Instant;

use async_channel::Sender;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::BufReader;
use tokio::process::Child;

use crate::error::CodexErr;
use crate::error::Result;
use crate::error::SandboxErr;
use crate::landlock::spawn_command_under_linux_sandbox;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecCommandOutputDeltaEvent;
use crate::protocol::ExecOutputStream;
use crate::protocol::SandboxPolicy;
use crate::seatbelt::spawn_command_under_seatbelt;
use crate::spawn::StdioPolicy;
use crate::spawn::spawn_child_async;
use crate::policy_yaml::{PolicyYaml, append_policy_log, allowed_hosts_for_level};
use crate::enforcement_advisor::advise_for_command;
#[cfg(feature = "enforcement-stub")]
use crate::enforcement_gate::{decide as enforcement_decide, GateDecision};
use serde_bytes::ByteBuf;
use once_cell::sync::OnceCell;

// Track per-session GODMODE wall-time budget start
static GODMODE_START: OnceCell<std::time::Instant> = OnceCell::new();

// Maximum we send for each stream, which is either:
// - 10KiB OR
// - 256 lines
const MAX_STREAM_OUTPUT: usize = 10 * 1024;
const MAX_STREAM_OUTPUT_LINES: usize = 256;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

// Hardcode these since it does not seem worth including the libc crate just
// for these.
const SIGKILL_CODE: i32 = 9;
const TIMEOUT_CODE: i32 = 64;

#[derive(Debug, Clone)]
pub struct ExecParams {
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: Option<u64>,
    pub env: HashMap<String, String>,
    pub with_escalated_permissions: Option<bool>,
    pub justification: Option<String>,
}

impl ExecParams {
    pub fn timeout_duration(&self) -> Duration {
        Duration::from_millis(self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SandboxType {
    None,

    /// Only available on macOS.
    MacosSeatbelt,

    /// Only available on Linux.
    LinuxSeccomp,
    /// High-privilege mode executed via an external microVM/container wrapper.
    /// Currently selected only when the session sandbox policy is
    /// `DangerFullAccess` and a wrapper is available on the host.
    MicroVm,
}

#[derive(Clone)]
pub struct StdoutStream {
    pub sub_id: String,
    pub call_id: String,
    pub tx_event: Sender<Event>,
}

pub async fn process_exec_tool_call(
    params: ExecParams,
    sandbox_type: SandboxType,
    sandbox_policy: &SandboxPolicy,
    codex_linux_sandbox_exe: &Option<PathBuf>,
    stdout_stream: Option<StdoutStream>,
) -> Result<ExecToolCallOutput> {
    let start = Instant::now();

    // Light policy warning/logging before execution (M0):
    // Read configs/policy.yaml and warn if command not in allowed list for current level.
    if let Some(py) = PolicyYaml::load_from_repo(&params.cwd) {
        if let Some(level) = py.active_level() {
            let allowed = py.allowed_for_level(level);
            if let Some(cmd0) = params.command.first() {
                // Compare base program name if path provided
                let prog = std::path::Path::new(cmd0)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| cmd0.clone());
                if !allowed.contains(&prog) && !allowed.contains(cmd0) {
                    // Send a small prefix to the command's stdout stream if available
                    if let Some(s) = &stdout_stream {
                        let msg = format!(
                            "[policy] Warning: '{}' not allowed for level {} (will proceed)\n",
                            prog, level
                        );
                        let _ = s.tx_event.try_send(Event {
                            id: s.sub_id.clone(),
                            msg: EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
                                call_id: s.call_id.clone(),
                                stream: ExecOutputStream::Stdout,
                                chunk: ByteBuf::from(msg.into_bytes()),
                            }),
                        });
                    }
                    // Append to policy log
                    let log_path = py.policy_log_path(&params.cwd);
                    let event = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "event": "warn_disallowed_command",
                        "profile": level,
                        "command": params.command,
                        "cwd": params.cwd,
                    });
                    append_policy_log(&log_path, event);
                }
            }
        }
    }

    // M2 (stub): Provide a non-blocking enforcement advisory based on the current sandbox policy.
    if let Some(s) = advise_for_command(sandbox_policy, &params.command) {
        if let Some(sout) = &stdout_stream {
            // Emit advisory as a background event so tests comparing stream payloads are stable.
            let _ = sout.tx_event.try_send(Event {
                id: sout.sub_id.clone(),
                msg: EventMsg::BackgroundEvent(crate::protocol::BackgroundEventEvent { message: s }),
            });
        }
    }

    // Optional enforcement gate (feature-gated). Deny early with clear message.
    #[cfg(feature = "enforcement-stub")]
    {
        match enforcement_decide(sandbox_policy, &params.command, &params.cwd) {
            GateDecision::Deny { message } => {
                return Err(CodexErr::Sandbox(SandboxErr::Denied(
                    1,
                    String::new(),
                    message,
                )));
            }
            GateDecision::Allow => {}
        }
    }

    let raw_output_result: std::result::Result<RawExecToolCallOutput, CodexErr> = match sandbox_type
    {
        SandboxType::None => exec(params, sandbox_policy, stdout_stream.clone()).await,
        SandboxType::MacosSeatbelt => {
            let timeout = params.timeout_duration();
            let ExecParams {
                command, cwd, env, ..
            } = params;
            let child = spawn_command_under_seatbelt(
                command,
                sandbox_policy,
                cwd,
                StdioPolicy::RedirectForShellTool,
                env,
            )
            .await?;
            consume_truncated_output(child, timeout, stdout_stream.clone()).await
        }
        SandboxType::LinuxSeccomp => {
            let timeout = params.timeout_duration();
            let ExecParams {
                command, cwd, env, ..
            } = params;

            let codex_linux_sandbox_exe = codex_linux_sandbox_exe
                .as_ref()
                .ok_or(CodexErr::LandlockSandboxExecutableNotProvided)?;
            let child = spawn_command_under_linux_sandbox(
                codex_linux_sandbox_exe,
                command,
                sandbox_policy,
                cwd,
                StdioPolicy::RedirectForShellTool,
                env,
            )
            .await?;

            consume_truncated_output(child, timeout, stdout_stream).await
        }
        SandboxType::MicroVm => {
            // GODMODE flow: prefer a microVM/container wrapper if available. This
            // keeps host free of direct exec, while still providing a high‑privilege
            // environment. We make a best‑effort attempt and fall back to host
            // execution with a loud warning if no wrapper is present.
            let timeout = params.timeout_duration();
            let ExecParams { command, cwd, env, .. } = params;

            // Enforce optional per-session wall-time budget for GODMODE
            if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                if let Some(secs) = py.godmode_wall_time_secs() {
                    let start = GODMODE_START.get_or_init(std::time::Instant::now);
                    if start.elapsed() > std::time::Duration::from_secs(secs.max(1)) {
                        let msg = format!(
                            "[godmode] time budget exceeded ({}s); refusing to run more high‑privilege commands",
                            secs
                        );
                        return Err(CodexErr::Sandbox(SandboxErr::Denied(1, String::new(), msg)));
                    }
                }
            }

            // Determine wrapper script. Prefer Firecracker stub, fall back to gVisor stub.
            let firecracker = cwd.join("sandbox").join("firecracker").join("start.sh");
            let gvisor = cwd.join("sandbox").join("gvisor").join("run.sh");

            // Helper to check executability (exists + is_file)
            let pick_wrapper = if firecracker.is_file() { Some(firecracker) }
                else if gvisor.is_file() { Some(gvisor) } else { None };

            if let Some(wrapper) = pick_wrapper {
                // Audit: record godmode execution via wrapper
                if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                    let log_path = py.policy_log_path(&cwd);
                    let event = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "event": "godmode_microvm_exec",
                        "wrapper": wrapper.to_string_lossy(),
                        "command": command,
                        "cwd": cwd,
                    });
                    append_policy_log(&log_path, event);
                }
                // Policy args to wrapper: read-only host FS, explicit writable
                // project root, and network disabled by default.
                let mut args: Vec<String> = Vec::new();
                args.push("--cwd".to_string());
                args.push(cwd.to_string_lossy().to_string());
                args.push("--writable".to_string());
                args.push(cwd.to_string_lossy().to_string());
                // Default: network off unless policy explicitly allows network
                let net_off = match sandbox_policy {
                    SandboxPolicy::DangerFullAccess => true,
                    SandboxPolicy::ReadOnly => false,
                    SandboxPolicy::WorkspaceWrite { network_access, .. } => !*network_access,
                };
                args.push("--network".to_string());
                args.push(if net_off { "off".to_string() } else { "on".to_string() });
                args.push("--".to_string());
                args.extend(command.clone());

                // Invoke wrapper with policy args and the original command after '--'.
                // We inherit no stdin and capture stdout/stderr like other tool calls.
                let child = spawn_child_async(
                    wrapper,
                    args,
                    None,
                    cwd,
                    sandbox_policy,
                    StdioPolicy::RedirectForShellTool,
                    env,
                )
                .await?;
                consume_truncated_output(child, timeout, stdout_stream).await
            } else {
                // Respect policy flag to optionally allow or reject host fallback
                if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                    if !py.godmode_allow_host_fallback() {
                        let msg = "[godmode] isolation wrapper not found and host fallback is disabled by policy (configs/policy.yaml godmode.allow_host_fallback)";
                        // Audit
                        let log_path = py.policy_log_path(&cwd);
                        append_policy_log(&log_path, serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "event": "godmode_fallback_blocked",
                            "command": command,
                            "cwd": cwd,
                        }));
                        return Err(CodexErr::Sandbox(SandboxErr::Denied(1, String::new(), msg.to_string())));
                    }
                }
                // No wrapper found – run on host but surface a clear warning and
                // log an audit event so users can verify the fallback occurred.
                if let Some(s) = &stdout_stream {
                    let warn = "[godmode] isolation wrapper not found; running on host";
                    let _ = s.tx_event.try_send(Event {
                        id: s.sub_id.clone(),
                        msg: EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
                            call_id: s.call_id.clone(),
                            stream: ExecOutputStream::Stderr,
                            chunk: ByteBuf::from(format!("{}\n", warn).into_bytes()),
                        }),
                    });
                }

                // Also append to policy log if present
                if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                    let log_path = py.policy_log_path(&cwd);
                    let event = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "event": "godmode_fallback_host_exec",
                        "command": command,
                        "cwd": cwd,
                    });
                    append_policy_log(&log_path, event);
                }

                // Best-effort exfiltration monitor: warn on outbound hosts not allowed by policy
                if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                    if let Some(level) = py.active_level() {
                        let allowed_hosts = allowed_hosts_for_level(&py, level);
                        if let Some(hosts) = detect_outbound_hosts(&command) {
                            for h in hosts {
                                if !allowed_hosts.contains(&h) {
                                    if let Some(s) = &stdout_stream {
                                        let hint = format!(
                                            "[safety] exfil warning: outbound host '{}' not in allowed_hosts — see /safety",
                                            h
                                        );
                                        let _ = s.tx_event.try_send(Event {
                                            id: s.sub_id.clone(),
                                            msg: EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
                                                call_id: s.call_id.clone(),
                                                stream: ExecOutputStream::Stderr,
                                                chunk: ByteBuf::from(format!("{}\n", hint).into_bytes()),
                                            }),
                                        });
                                    }
                                    let event = serde_json::json!({
                                        "ts": chrono::Utc::now().to_rfc3339(),
                                        "event": "exfil_warning",
                                        "host": h,
                                        "command": command,
                                        "cwd": cwd,
                                    });
                                    if let Some(py) = PolicyYaml::load_from_repo(&cwd) {
                                        let log_path = py.policy_log_path(&cwd);
                                        append_policy_log(&log_path, event);
                                    }
                                }
                            }
                        }
                    }
                }

                let (program, args) = command.split_first().ok_or_else(|| {
                    CodexErr::Io(io::Error::new(io::ErrorKind::InvalidInput, "command args are empty"))
                })?;
                let child = spawn_child_async(
                    PathBuf::from(program),
                    args.into(),
                    None,
                    cwd,
                    sandbox_policy,
                    StdioPolicy::RedirectForShellTool,
                    env,
                )
                .await?;
                consume_truncated_output(child, timeout, stdout_stream).await
            }
        }
    };
    let duration = start.elapsed();
    match raw_output_result {
        Ok(raw_output) => {
            let stdout = raw_output.stdout.from_utf8_lossy();
            let stderr = raw_output.stderr.from_utf8_lossy();

            #[cfg(target_family = "unix")]
            match raw_output.exit_status.signal() {
                Some(TIMEOUT_CODE) => return Err(CodexErr::Sandbox(SandboxErr::Timeout)),
                Some(signal) => {
                    return Err(CodexErr::Sandbox(SandboxErr::Signal(signal)));
                }
                None => {}
            }

            let exit_code = raw_output.exit_status.code().unwrap_or(-1);

            if exit_code != 0 && is_likely_sandbox_denied(sandbox_type, exit_code) {
                return Err(CodexErr::Sandbox(SandboxErr::Denied(
                    exit_code,
                    stdout.text,
                    stderr.text,
                )));
            }

            Ok(ExecToolCallOutput {
                exit_code,
                stdout,
                stderr,
                duration,
            })
        }
        Err(err) => {
            tracing::error!("exec error: {err}");
            Err(err)
        }
    }
}

/// We don't have a fully deterministic way to tell if our command failed
/// because of the sandbox - a command in the user's zshrc file might hit an
/// error, but the command itself might fail or succeed for other reasons.
/// For now, we conservatively check for 'command not found' (exit code 127),
/// and can add additional cases as necessary.
fn is_likely_sandbox_denied(sandbox_type: SandboxType, exit_code: i32) -> bool {
    if sandbox_type == SandboxType::None {
        return false;
    }

    // Quick rejects: well-known non-sandbox shell exit codes
    // 127: command not found, 2: misuse of shell builtins
    if exit_code == 127 {
        return false;
    }

    // For all other cases, we assume the sandbox is the cause
    true
}

/// Very small parser to extract obvious outbound hosts from common tools.
fn detect_outbound_hosts(cmd: &[String]) -> Option<Vec<String>> {
    if cmd.is_empty() { return None; }
    let prog = std::path::Path::new(&cmd[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&cmd[0]);
    let mut out: Vec<String> = Vec::new();
    let extract_from_url = |s: &str| -> Option<String> {
        if let Some(rest) = s.strip_prefix("http://").or_else(|| s.strip_prefix("https://")) {
            let host = rest.split('/').next().unwrap_or("");
            if !host.is_empty() { return Some(host.to_string()); }
        }
        None
    };
    match prog {
        "curl" | "wget" => {
            for a in cmd.iter().skip(1) {
                if let Some(h) = extract_from_url(a) { out.push(h); }
            }
        }
        "git" => {
            // git clone <url> or git fetch <remote>
            if cmd.len() >= 3 && (cmd[1] == "clone" || cmd[1] == "fetch") {
                let url = cmd[2].as_str();
                if let Some(h) = extract_from_url(url) { out.push(h); }
                else if let Some((_, rest)) = url.split_once('@') {
                    // git@github.com:org/repo.git
                    let h = rest.split(':').next().unwrap_or("");
                    if !h.is_empty() { out.push(h.to_string()); }
                }
            }
        }
        _ => {}
    }
    if out.is_empty() { None } else { Some(out) }
}

#[derive(Debug)]
pub struct StreamOutput<T> {
    pub text: T,
    pub truncated_after_lines: Option<u32>,
}
#[derive(Debug)]
pub struct RawExecToolCallOutput {
    pub exit_status: ExitStatus,
    pub stdout: StreamOutput<Vec<u8>>,
    pub stderr: StreamOutput<Vec<u8>>,
}

impl StreamOutput<String> {
    pub fn new(text: String) -> Self {
        Self {
            text,
            truncated_after_lines: None,
        }
    }
}

impl StreamOutput<Vec<u8>> {
    pub fn from_utf8_lossy(&self) -> StreamOutput<String> {
        StreamOutput {
            text: String::from_utf8_lossy(&self.text).to_string(),
            truncated_after_lines: self.truncated_after_lines,
        }
    }
}

#[derive(Debug)]
pub struct ExecToolCallOutput {
    pub exit_code: i32,
    pub stdout: StreamOutput<String>,
    pub stderr: StreamOutput<String>,
    pub duration: Duration,
}

async fn exec(
    params: ExecParams,
    sandbox_policy: &SandboxPolicy,
    stdout_stream: Option<StdoutStream>,
) -> Result<RawExecToolCallOutput> {
    let timeout = params.timeout_duration();
    let ExecParams {
        command, cwd, env, ..
    } = params;

    let (program, args) = command.split_first().ok_or_else(|| {
        CodexErr::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command args are empty",
        ))
    })?;
    let arg0 = None;
    let child = spawn_child_async(
        PathBuf::from(program),
        args.into(),
        arg0,
        cwd,
        sandbox_policy,
        StdioPolicy::RedirectForShellTool,
        env,
    )
    .await?;
    consume_truncated_output(child, timeout, stdout_stream).await
}

/// Consumes the output of a child process, truncating it so it is suitable for
/// use as the output of a `shell` tool call. Also enforces specified timeout.
pub(crate) async fn consume_truncated_output(
    mut child: Child,
    timeout: Duration,
    stdout_stream: Option<StdoutStream>,
) -> Result<RawExecToolCallOutput> {
    // Both stdout and stderr were configured with `Stdio::piped()`
    // above, therefore `take()` should normally return `Some`.  If it doesn't
    // we treat it as an exceptional I/O error

    let stdout_reader = child.stdout.take().ok_or_else(|| {
        CodexErr::Io(io::Error::other(
            "stdout pipe was unexpectedly not available",
        ))
    })?;
    let stderr_reader = child.stderr.take().ok_or_else(|| {
        CodexErr::Io(io::Error::other(
            "stderr pipe was unexpectedly not available",
        ))
    })?;

    let stdout_handle = tokio::spawn(read_capped(
        BufReader::new(stdout_reader),
        MAX_STREAM_OUTPUT,
        MAX_STREAM_OUTPUT_LINES,
        stdout_stream.clone(),
        false,
    ));
    let stderr_handle = tokio::spawn(read_capped(
        BufReader::new(stderr_reader),
        MAX_STREAM_OUTPUT,
        MAX_STREAM_OUTPUT_LINES,
        stdout_stream.clone(),
        true,
    ));

    let exit_status = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => {
            match result {
                Ok(Ok(exit_status)) => exit_status,
                Ok(e) => e?,
                Err(_) => {
                    // timeout
                    child.start_kill()?;
                    // Debatable whether `child.wait().await` should be called here.
                    synthetic_exit_status(128 + TIMEOUT_CODE)
                }
            }
        }
        _ = tokio::signal::ctrl_c() => {
            child.start_kill()?;
            synthetic_exit_status(128 + SIGKILL_CODE)
        }
    };

    let stdout = stdout_handle.await??;
    let stderr = stderr_handle.await??;

    Ok(RawExecToolCallOutput {
        exit_status,
        stdout,
        stderr,
    })
}

async fn read_capped<R: AsyncRead + Unpin + Send + 'static>(
    mut reader: R,
    max_output: usize,
    max_lines: usize,
    stream: Option<StdoutStream>,
    is_stderr: bool,
) -> io::Result<StreamOutput<Vec<u8>>> {
    let mut buf = Vec::with_capacity(max_output.min(8 * 1024));
    let mut tmp = [0u8; 8192];

    let mut remaining_bytes = max_output;
    let mut remaining_lines = max_lines;

    loop {
        let n = reader.read(&mut tmp).await?;
        if n == 0 {
            break;
        }

        if let Some(stream) = &stream {
            let chunk = tmp[..n].to_vec();
            let msg = EventMsg::ExecCommandOutputDelta(ExecCommandOutputDeltaEvent {
                call_id: stream.call_id.clone(),
                stream: if is_stderr {
                    ExecOutputStream::Stderr
                } else {
                    ExecOutputStream::Stdout
                },
                chunk: ByteBuf::from(chunk),
            });
            let event = Event {
                id: stream.sub_id.clone(),
                msg,
            };
            #[allow(clippy::let_unit_value)]
            let _ = stream.tx_event.send(event).await;
        }

        // Copy into the buffer only while we still have byte and line budget.
        if remaining_bytes > 0 && remaining_lines > 0 {
            let mut copy_len = 0;
            for &b in &tmp[..n] {
                if remaining_bytes == 0 || remaining_lines == 0 {
                    break;
                }
                copy_len += 1;
                remaining_bytes -= 1;
                if b == b'\n' {
                    remaining_lines -= 1;
                }
            }
            buf.extend_from_slice(&tmp[..copy_len]);
        }
        // Continue reading to EOF to avoid back-pressure, but discard once caps are hit.
    }

    let truncated = remaining_lines == 0 || remaining_bytes == 0;

    Ok(StreamOutput {
        text: buf,
        truncated_after_lines: if truncated {
            Some((max_lines - remaining_lines) as u32)
        } else {
            None
        },
    })
}

#[cfg(unix)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code)
}

#[cfg(windows)]
fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    #[expect(clippy::unwrap_used)]
    std::process::ExitStatus::from_raw(code.try_into().unwrap())
}
