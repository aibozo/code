use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct PolicyYaml {
    pub level: Option<String>,
    pub allowed_commands: HashMap<String, Vec<YamlEntry>>, // level -> entries
    pub allowed_hosts: HashMap<String, Vec<YamlEntry>>, // level -> hosts (allow inherit)
    pub logging: Option<Logging>,
    pub godmode: Option<Godmode>,
    pub defaults: Option<DefaultsPolicy>,
}

#[derive(Debug, Clone, Default)]
pub struct Logging {
    pub policy_log: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Godmode {
    pub allow_host_fallback: Option<bool>,
    pub wall_time_secs: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct DefaultsPolicy {
    /// When true, do not apply built-in allowlists for commands/hosts.
    /// Ignored for GODMODE to preserve expected behavior.
    pub disable_builtin_allowlists: Option<bool>,
}

#[derive(Debug, Clone)]
pub enum YamlEntry {
    Str(String),
    // Support inheritance: { inherit: "READ_ONLY" }
    Inherit { inherit: String },
}

impl PolicyYaml {
    pub fn load_from_repo(cwd: &Path) -> Option<Self> {
        let path = cwd.join("configs").join("policy.yaml");
        let contents = std::fs::read_to_string(&path).ok()?;
        Some(parse_policy_yaml(&contents))
    }

    pub fn policy_log_path(&self, cwd: &Path) -> PathBuf {
        let rel = self
            .logging
            .as_ref()
            .and_then(|l| l.policy_log.as_ref())
            .map(|s| PathBuf::from(s))
            .unwrap_or_else(|| PathBuf::from("logs/policy.jsonl"));
        cwd.join(rel)
    }

    pub fn active_level<'a>(&'a self) -> Option<&'a str> {
        self.level.as_deref()
    }

    pub fn allowed_for_level(&self, level: &str) -> HashSet<String> {
        let mut out = HashSet::new();
        self.collect_for_level(level, &mut out, 0);
        if out.is_empty() {
            out = builtin_allowed_commands_for(level);
        }
        out
    }

    fn collect_for_level(&self, level: &str, out: &mut HashSet<String>, depth: usize) {
        if depth > 8 {
            return; // guard cycles
        }
        let Some(entries) = self.allowed_commands.get(level) else { return; };
        for entry in entries {
            match entry {
                YamlEntry::Str(s) => {
                    out.insert(s.clone());
                }
                YamlEntry::Inherit { inherit } => {
                    self.collect_for_level(inherit, out, depth + 1);
                }
            }
        }
    }

    pub fn godmode_allow_host_fallback(&self) -> bool {
        self.godmode
            .as_ref()
            .and_then(|g| g.allow_host_fallback)
            .unwrap_or(false)
    }

    pub fn godmode_wall_time_secs(&self) -> Option<u64> {
        self.godmode.as_ref().and_then(|g| g.wall_time_secs)
    }

    pub fn disable_builtin_allowlists(&self) -> bool {
        self.defaults
            .as_ref()
            .and_then(|d| d.disable_builtin_allowlists)
            .unwrap_or(false)
    }
}

/// Append a JSONL log entry to the policy log path.
pub fn append_policy_log(path: &Path, event: serde_json::Value) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(_) => return,
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")
        });
}

// Very small YAML subset parser sufficient for our policy.yaml format.
fn parse_policy_yaml(s: &str) -> PolicyYaml {
    let mut py = PolicyYaml::default();
    let mut section: Option<String> = None; // "allowed_commands" or "allowed_hosts" or "logging"
    let mut current_level: Option<String> = None;
    for raw_line in s.lines() {
        let line = raw_line.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // Root-level keys
        if !line.starts_with(' ') && line.contains(':') {
            let mut parts = line.splitn(2, ':');
            let key = parts.next().unwrap().trim();
            let val = parts.next().unwrap().trim();
            match key {
                "level" => {
                    py.level = Some(val.trim_matches('"').to_string());
                    section = None;
                    current_level = None;
                }
                "allowed_commands" | "allowed_hosts" | "logging" | "godmode" | "defaults" => {
                    section = Some(key.to_string());
                    current_level = None;
                }
                _ => {
                    // ignore unknown root keys
                    section = None;
                    current_level = None;
                }
            }
            continue;
        }

        // Inside a section, detect level headers like "  READ_ONLY:" or logging keys
        if let Some(sec) = &section {
            if line.starts_with("  ") && !line.trim_start().starts_with('-') && line.contains(':') {
                let mut parts = line.trim().splitn(2, ':');
                let key = parts.next().unwrap().trim();
                let val = parts.next().unwrap().trim();
                if sec == "allowed_commands" {
                    current_level = Some(key.to_string());
                    py.allowed_commands.entry(key.to_string()).or_default();
                } else if sec == "allowed_hosts" {
                    current_level = Some(key.to_string());
                    py.allowed_hosts.entry(key.to_string()).or_default();
                } else if sec == "logging" {
                    if key == "policy_log" {
                        py.logging.get_or_insert_with(Default::default).policy_log = Some(val.trim_matches('"').to_string());
                    }
                } else if sec == "godmode" {
                    let g = py.godmode.get_or_insert_with(Default::default);
                    match key {
                        "allow_host_fallback" => {
                            let b = val.trim() == "true" || val.trim() == "yes";
                            g.allow_host_fallback = Some(b);
                        }
                        "wall_time_secs" => {
                            if let Ok(n) = val.trim().parse::<u64>() {
                                g.wall_time_secs = Some(n);
                            }
                        }
                        _ => {}
                    }
                } else if sec == "defaults" {
                    let d = py.defaults.get_or_insert_with(Default::default);
                    match key {
                        "disable_builtin_allowlists" => {
                            let b = val.trim() == "true" || val.trim() == "yes";
                            d.disable_builtin_allowlists = Some(b);
                        }
                        _ => {}
                    }
                }
                continue;
            }

            // List entries under current level
            if line.trim_start().starts_with("- ") {
                let item = line.trim_start().trim_start_matches("- ").trim();
                if let Some(level) = &current_level {
                    if sec == "allowed_commands" {
                        if let Some(inherit) = item.strip_prefix("inherit:") {
                            let inherit = inherit.trim().trim_matches('"').to_string();
                            py.allowed_commands
                                .entry(level.clone())
                                .or_default()
                                .push(YamlEntry::Inherit { inherit });
                        } else {
                            let item = item.trim_matches('"').to_string();
                            py.allowed_commands
                                .entry(level.clone())
                                .or_default()
                                .push(YamlEntry::Str(item));
                        }
                    } else if sec == "allowed_hosts" {
                        if let Some(inherit) = item.strip_prefix("inherit:") {
                            let inherit = inherit.trim().trim_matches('"').to_string();
                            py.allowed_hosts
                                .entry(level.clone())
                                .or_default()
                                .push(YamlEntry::Inherit { inherit });
                        } else {
                            let item = item.trim_matches('"').to_string();
                            py.allowed_hosts
                                .entry(level.clone())
                                .or_default()
                                .push(YamlEntry::Str(item));
                        }
                    }
                }
                continue;
            }
        }
    }
    py
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_godmode_section_and_logging_path() {
        let yaml = r#"
level: WORKSPACE_WRITE
logging:
  policy_log: logs/policy.jsonl
godmode:
  allow_host_fallback: false
  wall_time_secs: 120
defaults:
  disable_builtin_allowlists: true
"#;
        let py = parse_policy_yaml(yaml);
        assert_eq!(py.logging.as_ref().and_then(|l| l.policy_log.as_ref()), Some(&"logs/policy.jsonl".to_string()));
        assert!(py.godmode.is_some());
        assert_eq!(py.godmode.as_ref().unwrap().allow_host_fallback, Some(false));
        assert_eq!(py.active_level(), Some("WORKSPACE_WRITE"));
        assert!(py.disable_builtin_allowlists());
    }
}

/// Return a default allowed commands set when the policy file does not specify entries
/// for the active level. Defaults are conservative and avoid mutating commands.
fn builtin_allowed_commands_for(level: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    match level {
        // READ_ONLY: common introspection and read tools
        "READ_ONLY" => {
            for c in [
                "ls", "cat", "head", "tail", "wc", "cut", "sort", "uniq", "tr",
                "printf", "echo", "stat", "file", "pwd", "true", "false",
                "rg", "fd", "find", "grep",
            ] {
                out.insert(c.to_string());
            }
        }
        // WORKSPACE_WRITE and GODMODE: leave empty to avoid unexpected friction unless user opts in
        _ => {}
    }
    out
}

/// Resolve allowed_hosts for a level with inheritance and conservative defaults.
pub fn allowed_hosts_for_level(py: &PolicyYaml, level: &str) -> HashSet<String> {
    fn collect(py: &PolicyYaml, level: &str, out: &mut HashSet<String>, depth: usize) {
        if depth > 8 { return; }
        let Some(entries) = py.allowed_hosts.get(level) else { return; };
        for entry in entries {
            match entry {
                YamlEntry::Str(s) => { out.insert(s.clone()); }
                YamlEntry::Inherit { inherit } => { collect(py, inherit, out, depth + 1); }
            }
        }
    }
    let mut out = HashSet::new();
    collect(py, level, &mut out, 0);
    if out.is_empty() {
        // Defaults: major public registries and common dev hosts (minimally)
        // Respect disable knob, but NEVER disable defaults for GODMODE
        let disable = py.disable_builtin_allowlists() && !level.eq_ignore_ascii_case("GODMODE");
        if !disable && (level == "WORKSPACE_WRITE" || level == "GODMODE") {
            for h in [
                "github.com", "api.github.com", "gitlab.com",
                "registry.npmjs.org", "pypi.org", "files.pythonhosted.org",
            ] { out.insert(h.to_string()); }
        }
    }
    out
}
