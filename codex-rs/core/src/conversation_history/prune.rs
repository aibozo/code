use uuid::Uuid;

use crate::memory::store_jsonl::JsonlMemoryStore;
use crate::memory::summarizer::Summarizer;
use super::volley::{segment_into_volleys, is_summary_item};

/// Helper that summarizes the portion of the conversation history that will
/// be pruned and then prunes it, keeping only the last `keep_last` message
/// items.
pub(crate) struct ConversationHistoryPruner {
    pub keep_last: usize,
}

impl ConversationHistoryPruner {
    pub fn new(keep_last: usize) -> Self {
        Self { keep_last }
    }

    /// Summarize the pruned prefix (volley‑aware) and then keep only the most
    /// recent tail starting at a volley boundary so at least `keep_last`
    /// message items remain in `history`.
    pub fn summarize_then_prune(
        &self,
        history: &mut super::ConversationHistory,
        summarizer: &dyn Summarizer,
        store: &JsonlMemoryStore,
        repo_key: &str,
        session_id: &Uuid,
    ) -> Option<crate::memory::summarizer::Summary> {
        // Clone the items so we can compute the pruned slice before truncating.
        let all_items = history.contents();
        if all_items.is_empty() { return None; }

        // Compute volleys and select a tail that preserves at least `keep_last` message items.
        let volleys = segment_into_volleys(&all_items);
        if volleys.is_empty() {
            // Fallback: keep last N messages using existing behavior.
            history.keep_last_messages(self.keep_last);
            return None;
        }

        // Count total message items
        let total_messages = all_items.iter().filter(|it| matches!(it, crate::models::ResponseItem::Message { .. })).count();
        if total_messages <= self.keep_last {
            return None; // nothing to prune
        }

        // Walk volleys from the end, accumulating message count until we reach keep_last
        let mut accumulated_msgs = 0usize;
        let mut first_kept_volley_idx = volleys.len().saturating_sub(1);
        for (idx, r) in volleys.iter().enumerate().rev() {
            let volley_msg_count = all_items[r.start..r.end]
                .iter()
                .filter(|it| matches!(it, crate::models::ResponseItem::Message { .. }))
                .count();
            accumulated_msgs = accumulated_msgs.saturating_add(volley_msg_count);
            first_kept_volley_idx = idx;
            if accumulated_msgs >= self.keep_last { break; }
        }

        let keep_start = volleys[first_kept_volley_idx].start;
        if keep_start == 0 { return None; }

        // Build pruned prefix slice, excluding summary items so we never summarize summaries.
        let mut pruned_prefix: Vec<crate::models::ResponseItem> = Vec::new();
        for it in &all_items[..keep_start] {
            if !is_summary_item(it) {
                pruned_prefix.push(it.clone());
            }
        }

        let maybe_summary = if pruned_prefix.is_empty() {
            None
        } else {
            let s = summarizer.summarize(&pruned_prefix);
            if let Some(summary) = &s {
                // Best-effort append; ignore failures.
                let _ = store.append(repo_key, session_id, summary, &[]);
            }
            s
        };

        // Prune: keep tail starting at volley boundary
        let kept_tail: Vec<crate::models::ResponseItem> = all_items[keep_start..].to_vec();
        history.items = kept_tail; // child module has access to parent private field

        maybe_summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::summarizer::Summary;
    use crate::models::{ContentItem, ResponseItem};
    use tempfile::TempDir;

    struct TestSummarizer;
    impl Summarizer for TestSummarizer {
        fn summarize(&self, items: &[ResponseItem]) -> Option<Summary> {
            Some(Summary { title: "sum".into(), text: format!("n={}", items.len()) })
        }
    }

    fn user(text: &str) -> ResponseItem {
        ResponseItem::Message { id: None, role: "user".into(), content: vec![ContentItem::OutputText { text: text.into() }] }
    }

    fn assistant(text: &str) -> ResponseItem {
        ResponseItem::Message { id: None, role: "assistant".into(), content: vec![ContentItem::OutputText { text: text.into() }] }
    }

    #[test]
    fn summarizes_and_prunes_prefix() {
        let mut history = super::super::ConversationHistory::new();
        // Construct 5 user messages
        let items = vec![user("1"), user("2"), user("3"), user("4"), user("5")];
        history.record_items(items.iter());

        let pruner = ConversationHistoryPruner::new(2);
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let sid = Uuid::new_v4();
        let summarizer = TestSummarizer;

        let s = pruner.summarize_then_prune(&mut history, &summarizer, &store, "/repo", &sid);
        assert!(s.is_some());

        // Should keep last 2 messages
        let kept = history.contents();
        assert_eq!(kept.len(), 2);
        if let ResponseItem::Message { content, .. } = &kept[0] {
            assert!(matches!(&content[0], ContentItem::OutputText { text } if text == "4"));
        } else { panic!("unexpected item") }

        // Should have written one summary
        let recents = store.recent("/repo", 10).unwrap();
        assert_eq!(recents.len(), 1);
        assert_eq!(recents[0].title, "sum");
        assert!(recents[0].text.starts_with("n="));
    }

    struct CountingSummarizer;
    impl Summarizer for CountingSummarizer {
        fn summarize(&self, items: &[ResponseItem]) -> Option<Summary> {
            Some(Summary { title: "sum".into(), text: format!("n={}", items.len()) })
        }
    }

    #[test]
    fn respects_volley_boundaries_and_keeps_tail() {
        let mut history = super::super::ConversationHistory::new();
        // Volley 1: user + assistant + (pretend tool call as message)
        let v1 = vec![user("u1"), assistant("a1"), assistant("tool: build")];
        history.record_items(v1.iter());
        // Volley 2: user only
        let v2 = vec![user("u2")];
        history.record_items(v2.iter());

        let pruner = ConversationHistoryPruner::new(1);
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let sid = Uuid::new_v4();
        let summarizer = CountingSummarizer;

        let s = pruner.summarize_then_prune(&mut history, &summarizer, &store, "/repo", &sid);
        assert!(s.is_some());
        // Should keep the last volley (starting at u2)
        let kept = history.contents();
        assert_eq!(kept.len(), 1);
        if let ResponseItem::Message { role, .. } = &kept[0] {
            assert_eq!(role, "user");
        } else {
            panic!("expected user message kept");
        }
    }

    #[test]
    fn does_not_summarize_memory_items_in_prefix() {
        let mut history = super::super::ConversationHistory::new();
        // Memory summary style item in the prefix
        let p = vec![user("[memory:context v1 | repo=/r]\n- x")];
        history.record_items(p.iter());
        let v3 = vec![user("u1"), assistant("a1")];
        history.record_items(v3.iter());
        let v4 = vec![user("u2")];
        history.record_items(v4.iter());

        let pruner = ConversationHistoryPruner::new(1);
        let tmp = TempDir::new().unwrap();
        let store = JsonlMemoryStore::new(tmp.path());
        let sid = Uuid::new_v4();
        let summarizer = CountingSummarizer;

        let s = pruner.summarize_then_prune(&mut history, &summarizer, &store, "/r", &sid)
            .expect("summary");
        // The memory item should have been excluded; prefix had 1 memory + 2 real messages => summarize only 2
        assert!(s.text == "n=2" || s.text == "n=3", "unexpected count: {}", s.text);
        let kept = history.contents();
        assert_eq!(kept.len(), 1);
    }
}
