#![allow(clippy::unwrap_used)]

use codex_core::memory::summarizer::{CompactSummarizer, Summarizer};
use codex_core::{
    ContentItem, FunctionCallOutputPayload, LocalShellAction, LocalShellExecAction,
    LocalShellStatus, ResponseItem,
};

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
fn compact_summarizer_includes_calls_outputs_and_shell() {
    let items = vec![
        user("please run echo hi"),
        ResponseItem::FunctionCall {
            id: None,
            name: "shell".into(),
            arguments: "{}".into(),
            call_id: "c1".into(),
        },
        ResponseItem::LocalShellCall {
            id: None,
            call_id: Some("c1".into()),
            status: LocalShellStatus::Completed,
            action: LocalShellAction::Exec(LocalShellExecAction {
                command: vec!["echo".into(), "hi".into()],
                timeout_ms: None,
                working_directory: None,
                env: None,
                user: None,
            }),
        },
        ResponseItem::FunctionCallOutput {
            call_id: "c1".into(),
            output: FunctionCallOutputPayload {
                content: "hi".into(),
                success: Some(true),
            },
        },
        assistant("done"),
    ];

    let s = CompactSummarizer::new(400);
    let out = s.summarize(&items).unwrap();
    assert!(out.text.contains("User: please run echo hi"));
    assert!(out.text.contains("Call: shell"));
    assert!(out.text.contains("Shell: echo hi"));
    assert!(out.text.contains("Result(ok): hi"));
    assert!(out.text.contains("Assistant: done"));
}

#[test]
fn compact_summarizer_truncates_long_shell_and_output() {
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

    let s = CompactSummarizer::new(200);
    let out = s.summarize(&items).unwrap();
    assert!(out.text.contains("Shell: echo "));
    assert!(out.text.contains("…"));
    assert!(out.text.contains("Result(err): "));
}
