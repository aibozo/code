use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display as DeriveDisplay;

use crate::models::ContentItem;
use crate::models::ResponseItem;
use crate::protocol::AskForApproval;
use crate::protocol::SandboxPolicy;
use crate::shell::Shell;
use crate::policy_yaml::PolicyYaml;
use codex_protocol::config_types::SandboxMode;
use std::path::PathBuf;

/// wraps environment context message in a tag for the model to parse more easily.
pub(crate) const ENVIRONMENT_CONTEXT_START: &str = "<environment_context>";
pub(crate) const ENVIRONMENT_CONTEXT_END: &str = "</environment_context>";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, DeriveDisplay)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum NetworkAccess {
    Restricted,
    Enabled,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename = "environment_context", rename_all = "snake_case")]
pub(crate) struct EnvironmentContext {
    pub cwd: Option<PathBuf>,
    pub approval_policy: Option<AskForApproval>,
    pub sandbox_mode: Option<SandboxMode>,
    pub network_access: Option<NetworkAccess>,
    pub shell: Option<Shell>,
    /// Active policy level from configs/policy.yaml (e.g., READ_ONLY, WORKSPACE_WRITE, GODMODE)
    pub policy_level: Option<String>,
    /// Isolation wrapper availability: "firecracker", "gvisor", or "none".
    pub wrapper: Option<String>,
    /// Location where orchestration artifacts are stored.
    pub artifacts_root: Option<PathBuf>,
}

impl EnvironmentContext {
    pub fn new(
        cwd: Option<PathBuf>,
        approval_policy: Option<AskForApproval>,
        sandbox_policy: Option<SandboxPolicy>,
        shell: Option<Shell>,
    ) -> Self {
        // Derive policy level from configs/policy.yaml if available
        let policy_level = cwd
            .as_ref()
            .and_then(|p| PolicyYaml::load_from_repo(p))
            .and_then(|py| py.active_level().map(|s| s.to_string()));

        // Determine wrapper availability from project path
        let wrapper = cwd.as_ref().map(|p| {
            let firecracker = p.join("sandbox").join("firecracker").join("start.sh");
            let gvisor = p.join("sandbox").join("gvisor").join("run.sh");
            if firecracker.is_file() {
                "firecracker".to_string()
            } else if gvisor.is_file() {
                "gvisor".to_string()
            } else {
                "none".to_string()
            }
        });

        let artifacts_root = cwd.as_ref().map(|p| p.join("orchestrator").join("episodes"));

        Self {
            cwd,
            approval_policy,
            sandbox_mode: match sandbox_policy {
                Some(SandboxPolicy::DangerFullAccess) => Some(SandboxMode::DangerFullAccess),
                Some(SandboxPolicy::ReadOnly) => Some(SandboxMode::ReadOnly),
                Some(SandboxPolicy::WorkspaceWrite { .. }) => Some(SandboxMode::WorkspaceWrite),
                None => None,
            },
            network_access: match sandbox_policy {
                Some(SandboxPolicy::DangerFullAccess) => Some(NetworkAccess::Enabled),
                Some(SandboxPolicy::ReadOnly) => Some(NetworkAccess::Restricted),
                Some(SandboxPolicy::WorkspaceWrite { network_access, .. }) => {
                    if network_access {
                        Some(NetworkAccess::Enabled)
                    } else {
                        Some(NetworkAccess::Restricted)
                    }
                }
                None => None,
            },
            shell,
            policy_level,
            wrapper,
            artifacts_root,
        }
    }
}

impl EnvironmentContext {
    /// Serializes the environment context to XML. Libraries like `quick-xml`
    /// require custom macros to handle Enums with newtypes, so we just do it
    /// manually, to keep things simple. Output looks like:
    ///
    /// ```xml
    /// <environment_context>
    ///   <cwd>...</cwd>
    ///   <approval_policy>...</approval_policy>
    ///   <sandbox_mode>...</sandbox_mode>
    ///   <network_access>...</network_access>
    ///   <shell>...</shell>
    /// </environment_context>
    /// ```
    pub fn serialize_to_xml(self) -> String {
        let mut lines = vec![ENVIRONMENT_CONTEXT_START.to_string()];
        if let Some(cwd) = self.cwd {
            lines.push(format!("  <cwd>{}</cwd>", cwd.to_string_lossy()));
        }
        if let Some(approval_policy) = self.approval_policy {
            lines.push(format!(
                "  <approval_policy>{}</approval_policy>",
                approval_policy
            ));
        }
        if let Some(sandbox_mode) = self.sandbox_mode {
            lines.push(format!("  <sandbox_mode>{}</sandbox_mode>", sandbox_mode));
        }
        if let Some(network_access) = self.network_access {
            lines.push(format!(
                "  <network_access>{}</network_access>",
                network_access
            ));
        }
        if let Some(shell) = self.shell
            && let Some(shell_name) = shell.name()
        {
            lines.push(format!("  <shell>{}</shell>", shell_name));
        }
        if let Some(level) = &self.policy_level {
            lines.push(format!("  <policy_level>{}</policy_level>", level));
        }
        if let Some(wrapper) = &self.wrapper {
            lines.push(format!("  <wrapper>{}</wrapper>", wrapper));
        }
        if let Some(root) = &self.artifacts_root {
            lines.push(format!("  <artifacts_root>{}</artifacts_root>", root.display()));
        }
        lines.push(ENVIRONMENT_CONTEXT_END.to_string());
        lines.join("\n")
    }
}

impl From<EnvironmentContext> for ResponseItem {
    fn from(ec: EnvironmentContext) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: ec.serialize_to_xml(),
            }],
        }
    }
}
