use base64::Engine;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

use crate::AuthMode;

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Default)]
pub struct TokenData {
    /// Flat info parsed from the JWT in auth.json.
    #[serde(
        deserialize_with = "deserialize_id_token",
        serialize_with = "serialize_id_token"
    )]
    pub id_token: IdTokenInfo,

    /// This is a JWT.
    pub access_token: String,

    pub refresh_token: String,

    pub account_id: Option<String>,
}

impl TokenData {
    /// Decide whether to use the traditional API‑key path for model requests.
    ///
    /// Policy:
    /// - If the caller explicitly prefers API‑key, honor it.
    /// - If the token email is an @openai.com address, prefer ChatGPT (OAuth).
    /// - For known consumer/team plans (Free, Plus, Pro, Team), prefer ChatGPT.
    /// - For known enterprise/edu/business plans, use API‑key.
    /// - If plan is missing or unknown, prefer ChatGPT (safer default) so
    ///   primary model traffic uses OAuth even when an API key exists.
    pub(crate) fn should_use_api_key(
        &self,
        preferred_auth_method: AuthMode,
        is_openai_email: bool,
    ) -> bool {
        if preferred_auth_method == AuthMode::ApiKey {
            return true;
        }
        // If the email is an OpenAI email, use AuthMode::ChatGPT unless preferred_auth_method is AuthMode::ApiKey.
        if is_openai_email {
            return false;
        }

        match self.id_token.chatgpt_plan_type.as_ref() {
            Some(plan) => plan.is_plan_that_should_use_api_key(),
            // Missing plan info → prefer ChatGPT (OAuth) for primary model calls.
            None => false,
        }
    }

    pub fn is_openai_email(&self) -> bool {
        self.id_token
            .email
            .as_deref()
            .is_some_and(|email| email.trim().to_ascii_lowercase().ends_with("@openai.com"))
    }
}

/// Flat subset of useful claims in id_token from auth.json.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IdTokenInfo {
    pub email: Option<String>,
    /// The ChatGPT subscription plan type
    /// (e.g., "free", "plus", "pro", "business", "enterprise", "edu").
    /// (Note: ae has not verified that those are the exact values.)
    pub(crate) chatgpt_plan_type: Option<PlanType>,
    pub raw_jwt: String,
}

impl IdTokenInfo {
    pub fn get_chatgpt_plan_type(&self) -> Option<String> {
        self.chatgpt_plan_type.as_ref().map(|t| match t {
            PlanType::Known(plan) => format!("{plan:?}"),
            PlanType::Unknown(s) => s.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum PlanType {
    Known(KnownPlan),
    Unknown(String),
}

impl PlanType {
    fn is_plan_that_should_use_api_key(&self) -> bool {
        match self {
            Self::Known(known) => {
                use KnownPlan::*;
                // Enterprise/Business/Edu should use API‑key; consumer/team stays on ChatGPT.
                matches!(known, Business | Enterprise | Edu)
            }
            Self::Unknown(_) => {
                // Unknown plans: prefer ChatGPT (OAuth) to avoid forcing API if a key exists.
                false
            }
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            Self::Known(known) => format!("{known:?}").to_lowercase(),
            Self::Unknown(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum KnownPlan {
    Free,
    Plus,
    Pro,
    Team,
    Business,
    Enterprise,
    Edu,
}

#[derive(Deserialize)]
struct IdClaims {
    #[serde(default)]
    email: Option<String>,
    #[serde(rename = "https://api.openai.com/auth", default)]
    auth: Option<AuthClaims>,
}

#[derive(Deserialize)]
struct AuthClaims {
    #[serde(default)]
    chatgpt_plan_type: Option<PlanType>,
}

#[derive(Debug, Error)]
pub enum IdTokenInfoError {
    #[error("invalid ID token format")]
    InvalidFormat,
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub(crate) fn parse_id_token(id_token: &str) -> Result<IdTokenInfo, IdTokenInfoError> {
    // JWT format: header.payload.signature
    let mut parts = id_token.split('.');
    let (_header_b64, payload_b64, _sig_b64) = match (parts.next(), parts.next(), parts.next()) {
        (Some(h), Some(p), Some(s)) if !h.is_empty() && !p.is_empty() && !s.is_empty() => (h, p, s),
        _ => return Err(IdTokenInfoError::InvalidFormat),
    };

    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload_b64)?;
    let claims: IdClaims = serde_json::from_slice(&payload_bytes)?;

    Ok(IdTokenInfo {
        email: claims.email,
        chatgpt_plan_type: claims.auth.and_then(|a| a.chatgpt_plan_type),
        raw_jwt: id_token.to_string(),
    })
}

fn deserialize_id_token<'de, D>(deserializer: D) -> Result<IdTokenInfo, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse_id_token(&s).map_err(serde::de::Error::custom)
}

fn serialize_id_token<S>(id_token: &IdTokenInfo, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&id_token.raw_jwt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[test]
    fn id_token_info_parses_email_and_plan() {
        #[derive(Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }
        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": "user@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro"
            }
        });

        fn b64url_no_pad(bytes: &[u8]) -> String {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).unwrap());
        let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).unwrap());
        let signature_b64 = b64url_no_pad(b"sig");
        let fake_jwt = format!("{header_b64}.{payload_b64}.{signature_b64}");

        let info = parse_id_token(&fake_jwt).expect("should parse");
        assert_eq!(info.email.as_deref(), Some("user@example.com"));
        assert_eq!(
            info.chatgpt_plan_type,
            Some(PlanType::Known(KnownPlan::Pro))
        );
    }
}
