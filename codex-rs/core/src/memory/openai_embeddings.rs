use std::path::Path;

use codex_login::{AuthMode, CodexAuth};
use reqwest::Client;
use serde::Deserialize;

use crate::model_provider_info::ModelProviderInfo;
use codex_memory::embedding::{EmbeddingError, EmbeddingProvider};

/// OpenAI Embeddings client that uses Codex's existing auth flow.
///
/// - Prefers API key loaded from `~/.codex/auth.json` or `OPENAI_API_KEY`.
/// - Ignores ChatGPT OAuth tokens for embeddings.
pub struct OpenAiEmbeddingClient {
    base_url: String,
    query_string: String,
    api_key: String,
    http: Client,
}

impl OpenAiEmbeddingClient {
    /// Construct from the current provider and Codex home directory.
    pub fn from_provider(provider: &ModelProviderInfo, codex_home: &Path) -> Result<Self, EmbeddingError> {
        // Acquire API key from existing auth infrastructure.
        let auth = CodexAuth::from_codex_home(codex_home, AuthMode::ApiKey)
            .map_err(|e| std::io::Error::other(format!("load auth failed: {e}")))?
            .ok_or_else(|| std::io::Error::other("No OpenAI API key found. Run `codex login --api-key` or set OPENAI_API_KEY."))?;
        let api_key = futures::executor::block_on(auth.get_token())
            .map_err(|e| std::io::Error::other(format!("get token failed: {e}")))?;

        // Determine base URL (default to OpenAI API when not overridden).
        let mut base_url = provider
            .base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        if base_url.ends_with('/') {
            base_url.pop();
        }

        // Build query string from provider.query_params (e.g., Azure `api-version`).
        let query_string = if let Some(params) = &provider.query_params {
            if params.is_empty() {
                String::new()
            } else {
                let joined = params
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("&");
                format!("?{joined}")
            }
        } else {
            String::new()
        };

        Ok(Self { base_url, query_string, api_key, http: Client::new() })
    }

    fn embeddings_url(&self) -> String {
        format!("{}/embeddings{}", self.base_url, self.query_string)
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
}

impl EmbeddingProvider for OpenAiEmbeddingClient {
    fn embed(&self, texts: &[String], dim: usize) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() { return Ok(Vec::new()); }

        // Pick an embedding model based on requested dimension.
        // text-embedding-3-small: 1536 dims, low cost; text-embedding-3-large: 3072 dims.
        let (model, include_dimensions) = if dim <= 1536 {
            ("text-embedding-3-small", true)
        } else if dim <= 3072 {
            ("text-embedding-3-large", true)
        } else {
            return Err(EmbeddingError::InvalidDimension { expected: 3072, got: dim });
        };

        // Shape the request payload.
        #[derive(serde::Serialize)]
        struct EmbeddingRequest<'a> {
            model: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            dimensions: Option<usize>,
            input: &'a [String],
        }

        let payload = EmbeddingRequest {
            model,
            dimensions: if include_dimensions { Some(dim) } else { None },
            input: texts,
        };

        let url = self.embeddings_url();
        let resp = futures::executor::block_on(async {
            self.http
                .post(&url)
                .bearer_auth(&self.api_key)
                .header("content-type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| std::io::Error::other(format!("request failed: {e}")))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = futures::executor::block_on(resp.text()).unwrap_or_default();
            return Err(EmbeddingError::Io(std::io::Error::other(format!(
                "embedding HTTP {}: {}",
                status, body
            ))));
        }

        let parsed: EmbeddingResponse = futures::executor::block_on(resp.json())
            .map_err(|e| std::io::Error::other(format!("decode failed: {e}")))?;

        let mut out: Vec<Vec<f32>> = Vec::with_capacity(parsed.data.len());
        for d in parsed.data {
            if d.embedding.len() != dim {
                return Err(EmbeddingError::InvalidDimension { expected: dim, got: d.embedding.len() });
            }
            out.push(d.embedding);
        }
        Ok(out)
    }
}

/// Choose an embedding provider based on config. Currently supports:
/// - "openai" → OpenAI Embeddings using API key from existing auth
/// - anything else → Noop provider
pub fn select_embedding_provider(config: &crate::config::Config) -> Box<dyn EmbeddingProvider> {
    let id = config.memory.embedding.provider.trim();
    if id.eq_ignore_ascii_case("openai") {
        if let Some(p) = config.model_providers.get("openai") {
            if let Ok(client) = OpenAiEmbeddingClient::from_provider(p, &config.codex_home) {
                return Box::new(client);
            }
        }
    }
    Box::new(codex_memory::embedding::NoopEmbeddingProvider)
}

/// Returns true if an OpenAI API key is configured via `~/.codex/auth.json`
/// or the `OPENAI_API_KEY` environment variable.
pub fn has_openai_api_key(codex_home: &std::path::Path) -> bool {
    match codex_login::CodexAuth::from_codex_home(codex_home, AuthMode::ApiKey) {
        Ok(Some(a)) if a.mode == AuthMode::ApiKey => true,
        _ => false,
    }
}
