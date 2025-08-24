use crate::protocol::SandboxPolicy;

/// Return a human-readable advisory for the given command under the current sandbox policy.
/// Purely diagnostic — does not block execution.
pub fn advise_for_command(policy: &SandboxPolicy, command: &[String]) -> Option<String> {
    let cmd0 = command.get(0).map(|s| s.as_str()).unwrap_or("");
    match policy {
        SandboxPolicy::ReadOnly => {
            let tools = ["cargo", "npm", "pnpm", "uv", "pip", "pip3", "python", "python3", "node", "bash", "sh"];
            let looks_tool = tools.iter().any(|t| cmd0.ends_with(t))
                || command.iter().any(|c| c == "install" || c == "build");
            if looks_tool {
                return Some("[enforcement] READ_ONLY may block build/install; consider WORKSPACE_WRITE or request approval".to_string());
            }
        }
        SandboxPolicy::WorkspaceWrite { network_access, .. } => {
            if !*network_access {
                let net_cmds = ["curl", "wget", "git", "pip", "pip3", "npm", "pnpm", "uv"]; 
                if net_cmds.iter().any(|t| cmd0.ends_with(t)) || command.iter().any(|c| c == "clone") {
                    return Some("[enforcement] network disabled; enable network or switch profile".to_string());
                }
            }
        }
        SandboxPolicy::DangerFullAccess => {
            return Some("[enforcement] GODMODE active; actions run with full host access".to_string());
        }
    }
    None
}

