use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::codex::Session;
use crate::models::FunctionCallOutputPayload;
use crate::models::ResponseInputItem;
use crate::openai_tools::{JsonSchema, OpenAiTool, ResponsesApiTool};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Research,
    Plan,
    Code,
    Test,
    Summarize,
}

impl Phase {
    pub fn next(self) -> Phase {
        match self {
            Phase::Research => Phase::Plan,
            Phase::Plan => Phase::Code,
            Phase::Code => Phase::Test,
            Phase::Test => Phase::Summarize,
            Phase::Summarize => Phase::Research,
        }
    }
}

pub fn finish_tool_for_phase(phase: Phase) -> OpenAiTool {
    let (name, desc) = match phase {
        Phase::Research => ("research_finish", "Exit Research phase with a concise plain-text summary; writes research.md"),
        Phase::Plan => ("plan_finish", "Exit Plan phase with a concise plain-text plan; writes plan.md"),
        Phase::Code => ("code_finish", "Exit Code phase with a plain-text status; writes changes.md"),
        Phase::Test => ("test_finish", "Exit Test phase with a plain-text summary; writes test.md"),
        Phase::Summarize => ("summary_finish", "Exit Summarize phase with executive summary; writes summary.md"),
    };

    let mut properties = BTreeMap::new();
    properties.insert(
        "text".to_string(),
        JsonSchema::String {
            description: Some("Plain-text content for the phase artifact".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: name.to_string(),
        description: desc.to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["text".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn validate_plain_text(text: &str) -> Result<(), &'static str> {
    let words = text.split_whitespace().count();
    if words < 8 {
        return Err("Text too short; provide at least 8 words");
    }
    // Reject obvious JSON blocks
    if text.contains('{') && text.contains('}') {
        return Err("Avoid JSON blocks; provide plain text prose");
    }
    Ok(())
}

fn ensure_episode_dir(sess: &Session) -> Result<(String, PathBuf), String> { sess.phase_ensure_episode_dir() }

pub async fn handle_finish_for_phase(
    sess: &Session,
    phase: Phase,
    call_id: String,
    arguments: String,
) -> ResponseInputItem {
    // Parse arguments as JSON { text: string }
    let text = match serde_json::from_str::<serde_json::Value>(&arguments)
        .ok()
        .and_then(|v| v.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
    {
        Some(t) => t,
        None => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: "Missing required 'text' string parameter".to_string(),
                    success: Some(false),
                },
            }
        }
    };
    if let Err(msg) = validate_plain_text(&text) {
        return ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload { content: msg.to_string(), success: Some(false) },
        };
    }

    // Ensure episode dir and compute target filename
    let (_ts, ep_dir) = match ensure_episode_dir(sess) { Ok(v) => v, Err(e) => {
        return ResponseInputItem::FunctionCallOutput { call_id, output: FunctionCallOutputPayload { content: e, success: Some(false) } };
    }};

    let filename = match phase {
        Phase::Research => "research.md",
        Phase::Plan => "plan.md",
        Phase::Code => "changes.md",
        Phase::Test => "test.md",
        Phase::Summarize => "summary.md",
    };
    let path = ep_dir.join(filename);
    if let Err(e) = std::fs::write(&path, text) {
        return ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: format!("Failed to write {}: {}", path.display(), e),
                success: Some(false),
            },
        };
    }

    // Advance phase (wrap after summary -> research), and after summary best-effort ingest
    // Advance phase (keeps episode dir)
    sess.phase_advance();

    // Best-effort graph ingestion after summary
    if matches!(phase, Phase::Summarize) {
        let home = sess.get_cwd().to_path_buf();
        let _ = (|| -> std::io::Result<()> {
            let g = codex_memory::graph::FileGraph::new(&home)?;
            let _ = codex_memory::graph::ingest::ingest_episode_dir(&g, &ep_dir);
            Ok(())
        })();
    }

    ResponseInputItem::FunctionCallOutput {
        call_id,
        output: FunctionCallOutputPayload {
            content: format!("Wrote {} and advanced phase", filename),
            success: Some(true),
        },
    }
}

// Public helpers to read/update the phase
pub fn get_or_init_phase(sess: &Session) -> Phase { sess.phase_get_or_init() }
