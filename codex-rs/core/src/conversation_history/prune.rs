use uuid::Uuid;

use crate::memory::store_jsonl::JsonlMemoryStore;
use crate::memory::summarizer::Summarizer;

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

    /// Summarize the pruned prefix and then keep only the last `keep_last`
    /// message items in `history`.
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
        if all_items.len() <= self.keep_last {
            return None;
        }

        let cutoff = all_items.len() - self.keep_last;
        let pruned_slice = &all_items[..cutoff];

        let maybe_summary = summarizer.summarize(pruned_slice);
        if let Some(summary) = &maybe_summary {
            // Best-effort append; ignore failures.
            let _ = store.append(repo_key, session_id, summary, &[]);
        }

        history.keep_last_messages(self.keep_last);
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
}
