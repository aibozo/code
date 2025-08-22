use std::collections::HashMap;
use std::sync::Arc;

use codex_login::CodexAuth;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::codex::Codex;
use crate::codex::CodexSpawnOk;
use crate::codex::INITIAL_SUBMIT_ID;
use crate::codex_conversation::CodexConversation;
use crate::config::Config;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::SessionConfiguredEvent;

/// Represents a newly created Codex conversation, including the first event
/// (which is [`EventMsg::SessionConfigured`]).
pub struct NewConversation {
    pub conversation_id: Uuid,
    pub conversation: Arc<CodexConversation>,
    pub session_configured: SessionConfiguredEvent,
}

/// [`ConversationManager`] is responsible for creating conversations and
/// maintaining them in memory.
pub struct ConversationManager {
    conversations: Arc<RwLock<HashMap<Uuid, Arc<CodexConversation>>>>,
}

impl Default for ConversationManager {
    fn default() -> Self {
        Self {
            conversations: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl ConversationManager {
    pub async fn new_conversation(&self, config: Config) -> CodexResult<NewConversation> {
        // Choose auth mode based on configuration: when ChatGPT auth is
        // available (user signed in), prefer it for main model calls.
        // Embeddings continue to use API key internally.
        let auth = if config.using_chatgpt_auth {
            CodexAuth::from_codex_home(&config.codex_home, codex_login::AuthMode::ChatGPT)?
        } else {
            CodexAuth::from_codex_home(&config.codex_home, codex_login::AuthMode::ApiKey)?
        };
        self.new_conversation_with_auth(config, auth).await
    }

    /// Used for integration tests: should not be used by ordinary business
    /// logic.
    pub async fn new_conversation_with_auth(
        &self,
        config: Config,
        auth: Option<CodexAuth>,
    ) -> CodexResult<NewConversation> {
        let CodexSpawnOk {
            codex,
            session_id: conversation_id,
            init_id: _,
        } = Codex::spawn(config, auth).await?;

        // The first event must be `SessionInitialized`. Validate and forward it
        // to the caller so that they can display it in the conversation
        // history.
        let event = codex.next_event().await?;
        let session_configured = match event {
            Event {
                id,
                msg: EventMsg::SessionConfigured(session_configured),
            } if id == INITIAL_SUBMIT_ID => session_configured,
            _ => {
                return Err(CodexErr::SessionConfiguredNotFirstEvent);
            }
        };

        let conversation = Arc::new(CodexConversation::new(codex));
        self.conversations
            .write()
            .await
            .insert(conversation_id, conversation.clone());

        Ok(NewConversation {
            conversation_id,
            conversation,
            session_configured,
        })
    }

    pub async fn get_conversation(
        &self,
        conversation_id: Uuid,
    ) -> CodexResult<Arc<CodexConversation>> {
        let conversations = self.conversations.read().await;
        conversations
            .get(&conversation_id)
            .cloned()
            .ok_or_else(|| CodexErr::ConversationNotFound(conversation_id))
    }
}
