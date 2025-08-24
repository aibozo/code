use std::path::PathBuf;

use crate::protocol::SandboxPolicy;

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Allow,
    Deny { message: String },
}

/// Very small, deterministic gate used when `enforcement-stub` feature is enabled.
/// It does not inspect the filesystem; only the command vector and policy.
pub fn decide(policy: &SandboxPolicy, command: &[String], _cwd: &PathBuf) -> GateDecision {
    let cmd0 = command.get(0).map(|s| base_prog(s)).unwrap_or("");
    match policy {
        SandboxPolicy::ReadOnly => {
            // Allow only read/list/search/basic shell builtins; deny common mutators.
            let mutators = [
                "git", "npm", "pnpm", "cargo", "pip", "pip3", "uv", "apt-get", "brew",
                "curl", "wget", "bash", "sh", "rm", "mv", "cp", "sed", "awk",
            ];
            if mutators.contains(&cmd0) {
                return GateDecision::Deny { message: format!("[enforcement] READ_ONLY denies command '{}'. Switch to WORKSPACE_WRITE or request approval.", cmd0) };
            }
            if cmd0 == "git" {
                let allowed = ["status", "show", "diff", "log", "ls-files", "grep"];
                let sub = command.get(1).map(|s| s.as_str()).unwrap_or("");
                if !allowed.contains(&sub) {
                    return GateDecision::Deny { message: format!("[enforcement] READ_ONLY denies 'git {}'.", sub) };
                }
            }
            GateDecision::Allow
        }
        SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            // Deny system-level tools; gate network if disabled
            let sys_tools = ["apt-get", "brew"];
            if sys_tools.contains(&cmd0) {
                return GateDecision::Deny { message: format!("[enforcement] WORKSPACE_WRITE denies system tool '{}'.", cmd0) };
            }
            if !*network_access {
                let net_cmds = ["curl", "wget"]; // common net probes
                if net_cmds.contains(&cmd0) {
                    return GateDecision::Deny { message: "[enforcement] network disabled for this profile.".to_string() };
                }
                if (cmd0 == "git" && command.iter().any(|c| c == "clone"))
                    || (cmd0 == "npm" && command.iter().any(|c| c == "install"))
                    || (cmd0 == "pnpm" && command.iter().any(|c| c == "install"))
                    || (cmd0 == "pip" && command.iter().any(|c| c == "install"))
                    || (cmd0 == "pip3" && command.iter().any(|c| c == "install"))
                    || (cmd0 == "uv" && command.iter().any(|c| c == "pip"))
                {
                    return GateDecision::Deny { message: "[enforcement] dependency install requires network; enable network or switch profile.".to_string() };
                }
            }
            GateDecision::Allow
        }
        SandboxPolicy::DangerFullAccess => GateDecision::Allow,
    }
}

fn base_prog(s: &str) -> &str {
    std::path::Path::new(s)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or(s)
}

