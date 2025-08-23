use std::ops::Range;

use crate::models::ContentItem;
use crate::models::ResponseItem;

/// Returns true when this item is a summary/injected memory block that should not be summarized again.
/// Heuristic: a Message whose first text block starts with "[memory:".
pub fn is_summary_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { content, .. } => content.iter().any(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.starts_with("[memory:"),
            _ => false,
        }),
        _ => false,
    }
}

/// Returns true when this item is an ephemeral status/screenshot marker message.
/// Heuristic: any text content starting with "[EPHEMERAL:" (screenshots/status are filtered elsewhere,
/// but we keep this guard for robustness).
pub fn is_ephemeral_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { content, .. } => content.iter().any(|c| match c {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.starts_with("[EPHEMERAL:"),
            _ => false,
        }),
        _ => false,
    }
}

/// Returns true when this item looks like a protected PR/design doc that should not be summarized or pruned.
/// Heuristic: Message text starts with a markdown heading containing PR/Plan/RFC/Design, or includes a
/// marker like "[DOC:PR]" (case-insensitive checks on heading keywords).
// NOTE: We considered detecting and protecting PR/design docs here, but have
// chosen not to enforce special handling. Summarization and preflight compaction
// should preserve essential direction via regular summaries, and users can re‑inject
// docs when necessary.

/// Segment a transcript into volley ranges.
/// A volley starts at a user message and includes subsequent items until the next user message.
/// Any leading items before the first user message are grouped into a prelude volley (0..first_user).
pub fn segment_into_volleys(items: &[ResponseItem]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();

    for (idx, it) in items.iter().enumerate() {
        if let ResponseItem::Message { role, .. } = it {
            if role == "user" {
                starts.push(idx);
            }
        }
    }

    if starts.is_empty() {
        if !items.is_empty() {
            out.push(0..items.len());
        }
        return out;
    }

    // Prelude volley if there are items before the first user message
    if starts[0] > 0 {
        out.push(0..starts[0]);
    }

    for (i, &s) in starts.iter().enumerate() {
        let e = if i + 1 < starts.len() { starts[i + 1] } else { items.len() };
        out.push(s..e);
    }

    out
}

/// Estimate tokens for a slice of items using a simple chars/4 heuristic.
pub fn estimated_tokens(items: &[ResponseItem]) -> usize {
    let mut chars: usize = 0;
    for it in items {
        match it {
            ResponseItem::Message { content, .. } => {
                for c in content {
                    match c {
                        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                            chars = chars.saturating_add(text.len());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (chars + 3) / 4
}

/// Filter volley ranges down to candidates suitable for summarization:
/// - skip ranges that contain only summary items
/// - skip ranges that are entirely ephemeral (defensive; should not occur in history)
pub fn filter_compaction_candidates(items: &[ResponseItem], volleys: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    'outer: for r in volleys {
        if r.start >= r.end || r.end > items.len() { continue; }
        let mut has_non_summary = false;
        let mut all_ephemeral = true;
        for it in &items[r.start..r.end] {
            if !is_ephemeral_item(it) { all_ephemeral = false; }
            if !is_summary_item(it) { has_non_summary = true; }
        }
        if !has_non_summary { continue 'outer; }
        if all_ephemeral { continue 'outer; }
        out.push(r.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ResponseItem;
    use crate::models::ContentItem;

    fn msg(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![ContentItem::OutputText { text: text.to_string() }],
        }
    }

    #[test]
    fn segments_simple_user_assistant_pairs() {
        let items = vec![
            msg("user", "u1"),
            msg("assistant", "a1"),
            msg("user", "u2"),
            msg("assistant", "a2"),
        ];
        let v = segment_into_volleys(&items);
        assert_eq!(v, vec![0..2, 2..4]);
    }

    #[test]
    fn prelude_before_first_user_is_grouped() {
        let items = vec![
            msg("assistant", "intro"),
            msg("assistant", "prep"),
            msg("user", "u1"),
            msg("assistant", "a1"),
        ];
        let v = segment_into_volleys(&items);
        assert_eq!(v, vec![0..2, 2..4]);
    }

    #[test]
    fn detects_summary_items() {
        let s = msg("user", "[memory:summary v1 | repo=/r]\n- bullet");
        let n = msg("user", "hello");
        assert!(is_summary_item(&s));
        assert!(!is_summary_item(&n));
    }

    #[test]
    fn filters_candidates_to_skip_summaries_only_ranges() {
        let items = vec![
            msg("user", "[memory:context v1 | repo=/r]\n- stuff"),
            msg("assistant", "[memory:retrieval v1 | repo=/r]\n- hint"),
            msg("user", "u1"),
            msg("assistant", "a1"),
        ];
        let volleys = segment_into_volleys(&items);
        // volleys: prelude 0..2 (both summaries), then 2..4 actual volley
        let c = filter_compaction_candidates(&items, &volleys);
        assert_eq!(c, vec![2..4]);
    }

    #[test]
    fn ephemeral_items_do_not_block_candidates() {
        let items = vec![
            msg("assistant", "[EPHEMERAL: status]"),
            msg("assistant", "processing"),
            msg("user", "real"),
            msg("assistant", "done"),
        ];
        let volleys = segment_into_volleys(&items);
        let c = filter_compaction_candidates(&items, &volleys);
        // Should include the prelude volley (0..2) and the real volley (2..4)
        assert_eq!(c, vec![0..2, 2..4]);
    }
}
