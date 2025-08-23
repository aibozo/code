use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct PolicyYaml {
    pub level: Option<String>,
    pub allowed_commands: HashMap<String, Vec<YamlEntry>>, // level -> entries
    pub allowed_hosts: HashMap<String, Vec<String>>, // level -> hosts
    pub logging: Option<Logging>,
}

#[derive(Debug, Clone, Default)]
pub struct Logging {
    pub policy_log: Option<String>,
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
                "allowed_commands" | "allowed_hosts" | "logging" => {
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
                        let item = item.trim_matches('"').to_string();
                        py.allowed_hosts.entry(level.clone()).or_default().push(item);
                    }
                }
                continue;
            }
        }
    }
    py
}
