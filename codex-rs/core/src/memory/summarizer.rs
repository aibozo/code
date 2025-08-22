use crate::models::ResponseItem;
use crate::model_provider_info::ModelProviderInfo;
use codex_login::{AuthMode, CodexAuth};
use reqwest::Client;
use serde::Deserialize;

/// A concise summary of a span of conversation items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub title: String,
    pub text: String,
}

/// Interface for producing short summaries from a slice of response items.
///
/// Implementations should be resilient to failures and return `None` when a
/// summary cannot be produced. Summaries should avoid long code blocks and be
/// limited to a few hundred characters.
pub trait Summarizer: Send + Sync {
    fn summarize(&self, items: &[ResponseItem]) -> Option<Summary>;
}

/// No-op summarizer used for tests and when memory is disabled.
pub struct NoopSummarizer;

impl Summarizer for NoopSummarizer {
    fn summarize(&self, _items: &[ResponseItem]) -> Option<Summary> {
        None
    }
}

/// A lightweight, local summarizer that compresses a slice of conversation
/// items into a short, human-readable summary without calling an LLM.
///
/// This implementation prefers simplicity and determinism so it can run in
/// restricted environments. It keeps a small number of compact bullet lines
/// derived from user/assistant messages and enforces a character budget.
pub struct CompactSummarizer {
    pub max_chars: usize,
}

impl CompactSummarizer {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Summarizer for CompactSummarizer {
    fn summarize(&self, items: &[ResponseItem]) -> Option<Summary> {
        if items.is_empty() || self.max_chars == 0 {
            return None;
        }

        // Collect compact lines: prefix with role and use textual content only.
        let mut lines: Vec<String> = Vec::new();
        for it in items {
            match it {
                ResponseItem::Message { role, content, .. } => {
                    let mut text = String::new();
                    for c in content {
                        match c {
                            crate::models::ContentItem::InputText { text: t }
                            | crate::models::ContentItem::OutputText { text: t } => {
                                // Skip ephemeral markers
                                if t.starts_with("[EPHEMERAL:") { continue; }
                                if !text.is_empty() { text.push(' '); }
                                text.push_str(t.trim());
                            }
                            _ => {}
                        }
                    }
                    if text.is_empty() { continue; }
                    // Normalize whitespace and trim
                    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    let prefix = match role.as_str() {
                        "user" => "User",
                        "assistant" => "Assistant",
                        _ => continue,
                    };
                    lines.push(format!("{prefix}: {normalized}"));
                }
                ResponseItem::FunctionCall { name, .. } => {
                    lines.push(format!("Call: {}", name));
                }
                ResponseItem::FunctionCallOutput { output, .. } => {
                    let status = output.success.map(|b| if b { "ok" } else { "err" }).unwrap_or("n/a");
                    let mut excerpt: String = output.content.chars().take(60).collect();
                    if output.content.chars().count() > 60 { excerpt.push_str("…"); }
                    if excerpt.is_empty() { excerpt.push_str("<no output>"); }
                    lines.push(format!("Result({}): {}", status, excerpt));
                }
                ResponseItem::LocalShellCall { action, .. } => {
                    if let crate::models::LocalShellAction::Exec(exec) = action {
                        let mut cmd = exec.command.join(" ");
                        if cmd.chars().count() > 60 {
                            let truncated: String = cmd.chars().take(60).collect();
                            cmd = format!("{}…", truncated);
                        }
                        if cmd.is_empty() { cmd = "<empty>".into(); }
                        lines.push(format!("Shell: {}", cmd));
                    }
                }
                _ => {}
            }
        }

        if lines.is_empty() {
            return None;
        }

        // Title: first non-empty line truncated.
        let mut title = lines
            .iter()
            .find(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "Earlier conversation".to_string());
        if title.len() > 80 {
            title.truncate(80);
        }

        // Body: bullets within budget.
        let mut remaining = self.max_chars;
        let mut body = String::new();
        for line in lines {
            if remaining == 0 { break; }
            let bullet_full = format!("- {line}");
            let need = bullet_full.len() + 1; // +1 for newline
            if need <= remaining {
                body.push_str(&bullet_full);
                body.push('\n');
                remaining -= need;
            } else if remaining > 4 {
                // Truncate the last bullet to fit and indicate truncation
                let take = remaining - 4; // space for " ..."
                let truncated: String = bullet_full.chars().take(take).collect();
                body.push_str(&truncated);
                body.push_str(" ...\n");
                remaining = 0;
                break;
            } else {
                break;
            }
        }

        if body.trim().is_empty() {
            return None;
        }

        Some(Summary { title, text: body })
    }
}

/// LLM-backed summarizer using OpenAI Chat Completions with an API key.
/// Intended for short, low-cost models like `gpt-5-nano`.
pub struct OpenAiNanoSummarizer {
    http: Client,
    url: String,
    api_key: String,
    model: String,
    pub max_chars: usize,
}

impl OpenAiNanoSummarizer {
    /// Build from provider definition and codex home (to load API key).
    pub fn from_provider(
        provider: &ModelProviderInfo,
        codex_home: &std::path::Path,
        model: &str,
        max_chars: usize,
    ) -> std::io::Result<Self> {
        // Always use API key for this client.
        let auth = CodexAuth::from_codex_home(codex_home, AuthMode::ApiKey)
            .map_err(|e| std::io::Error::other(format!("auth: {e}")))?
            .ok_or_else(|| std::io::Error::other("No OpenAI API key found"))?;
        let api_key = futures::executor::block_on(auth.get_token())
            .map_err(|e| std::io::Error::other(format!("token: {e}")))?;

        // Determine base URL from provider, default to OpenAI.
        let mut base_url = provider
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        if base_url.ends_with('/') {
            base_url.pop();
        }
        // Prefer chat/completions for simplicity
        let url = format!("{}/chat/completions", base_url);

        Ok(Self {
            http: Client::new(),
            url,
            api_key,
            model: model.to_string(),
            max_chars,
        })
    }

    fn digest_items(&self, items: &[ResponseItem]) -> String {
        // Reuse the compact logic to produce a small digest as input.
        let compact = CompactSummarizer { max_chars: self.max_chars.saturating_mul(4) };
        // CompactSummarizer returns Option<Summary>; if None, fallback to simple join.
        if let Some(s) = compact.summarize(items) {
            let mut buf = String::new();
            buf.push_str(&s.title);
            buf.push('\n');
            buf.push_str(&s.text);
            return buf;
        }
        // fallback: stringify messages
        let mut lines = Vec::new();
        for it in items {
            match it {
                ResponseItem::Message { role, content, .. } => {
                    let mut t = String::new();
                    for c in content {
                        match c {
                            crate::models::ContentItem::InputText { text }
                            | crate::models::ContentItem::OutputText { text } => {
                                if !t.is_empty() { t.push(' '); }
                                t.push_str(text);
                            }
                            _ => {}
                        }
                    }
                    if !t.trim().is_empty() {
                        lines.push(format!("{}: {}", role, t.trim()));
                    }
                }
                _ => {}
            }
        }
        let mut s = lines.join("\n");
        if s.len() > 4000 { s.truncate(4000); }
        s
    }
}

impl Summarizer for OpenAiNanoSummarizer {
    fn summarize(&self, items: &[ResponseItem]) -> Option<Summary> {
        if items.is_empty() { return None; }

        // Compose prompt
        let system = format!(
            "You are an expert assistant that writes very concise conversation summaries.\\n\
             Produce: first line starting with 'Title: ' followed by a very short title.\\n\
             Then up to 6 bullets starting with '- ' covering key points.\\n\
             Keep the entire output under {} characters. No code blocks.",
            self.max_chars
        );
        let digest = self.digest_items(items);

        #[derive(serde::Serialize)]
        struct Msg<'a> { role: &'a str, content: &'a str }
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            model: &'a str,
            messages: [Msg<'a>; 2],
            temperature: f32,
        }
        let payload = Payload {
            model: &self.model,
            messages: [
                Msg { role: "system", content: &system },
                Msg { role: "user", content: &digest },
            ],
            temperature: 0.0,
        };

        #[derive(Deserialize)]
        struct Choice { message: ChoiceMsg }
        #[derive(Deserialize)]
        struct ChoiceMsg { content: String }
        #[derive(Deserialize)]
        struct Resp { choices: Vec<Choice> }

        let resp = futures::executor::block_on(async {
            self.http
                .post(&self.url)
                .bearer_auth(&self.api_key)
                .header("content-type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| std::io::Error::other(format!("request failed: {e}")))
        }).ok()?;

        if !resp.status().is_success() { return None; }
        let parsed: Resp = futures::executor::block_on(resp.json()).ok()?;
        let content = parsed.choices.get(0)?.message.content.trim().to_string();
        if content.is_empty() { return None; }

        // Parse first "Title: ..." line and remaining bullets.
        let mut lines = content.lines();
        let mut title = String::from("Earlier conversation");
        while let Some(l) = lines.next() {
            let lt = l.trim();
            if lt.is_empty() { continue; }
            if let Some(rest) = lt.strip_prefix("Title:") {
                title = rest.trim().to_string();
            } else {
                // if no explicit Title found, use first non-empty line as title
                title = lt.to_string();
            }
            break;
        }
        // Remaining lines as bullets; enforce max_chars.
        let mut body = String::new();
        let mut remaining = self.max_chars;
        for l in lines {
            let t = l.trim();
            if t.is_empty() { continue; }
            let bullet = if t.starts_with('-') { t.to_string() } else { format!("- {}", t) };
            let need = bullet.len() + 1;
            if need > remaining { break; }
            body.push_str(&bullet);
            body.push('\n');
            remaining -= need;
        }
        if body.is_empty() { body = "- Summary not available".to_string(); }

        Some(Summary { title, text: body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentItem, FunctionCallOutputPayload, LocalShellAction, LocalShellExecAction, LocalShellStatus};

    fn user(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::OutputText { text: text.into() }],
        }
    }
    fn assistant(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::OutputText { text: text.into() }],
        }
    }

    #[test]
    fn compact_includes_calls_outputs_and_shell() {
        let items = vec![
            user("please run echo hi"),
            ResponseItem::FunctionCall { id: None, name: "shell".into(), arguments: "{}".into(), call_id: "c1".into() },
            ResponseItem::LocalShellCall { id: None, call_id: Some("c1".into()), status: LocalShellStatus::Completed, action: LocalShellAction::Exec(LocalShellExecAction { command: vec!["echo".into(), "hi".into()], timeout_ms: None, working_directory: None, env: None, user: None }) },
            ResponseItem::FunctionCallOutput { call_id: "c1".into(), output: FunctionCallOutputPayload { content: "hi".into(), success: Some(true) } },
            assistant("done"),
        ];

        let s = CompactSummarizer::new(400);
        let out = s.summarize(&items).expect("summary");
        // Title may vary; verify body lines
        assert!(out.text.contains("User: please run echo hi"));
        assert!(out.text.contains("Call: shell"));
        assert!(out.text.contains("Shell: echo hi"));
        assert!(out.text.contains("Result(ok): hi"));
        assert!(out.text.contains("Assistant: done"));
    }

    #[test]
    fn truncates_long_shell_and_output() {
        let long_cmd_tail = "a".repeat(200);
        let long_output = "y".repeat(200);
        let items = vec![
            ResponseItem::LocalShellCall {
                id: None,
                call_id: Some("c2".into()),
                status: LocalShellStatus::Completed,
                action: LocalShellAction::Exec(LocalShellExecAction {
                    command: vec!["echo".into(), long_cmd_tail.clone()],
                    timeout_ms: None,
                    working_directory: None,
                    env: None,
                    user: None,
                }),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "c2".into(),
                output: FunctionCallOutputPayload {
                    content: long_output.clone(),
                    success: Some(false),
                },
            },
        ];

        let s = CompactSummarizer::new(400);
        let out = s.summarize(&items).expect("summary");
        // Ensure truncation marker present for both shell and result lines
        assert!(out.text.contains("Shell: echo "));
        assert!(out.text.contains("…"));
        assert!(out.text.contains("Result(err): "));
    }
}
