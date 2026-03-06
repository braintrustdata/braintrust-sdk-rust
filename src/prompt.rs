//! Prompt support for loading versioned prompt templates.
//!
//! Prompts allow you to:
//! - Fetch versioned prompt templates from Braintrust
//! - Convert prompts to lingua `UniversalRequest` for use with any LLM provider
//! - Use environment-based versioning (e.g., "production")
//!
//! # Example
//!
//! ```ignore
//! let prompt = client
//!     .prompt_builder_with_credentials(&api_key)
//!     .project_name("my-project")
//!     .slug("greeting-prompt")
//!     .environment("production")
//!     .build()
//!     .await?;
//!
//! if let Some(request) = prompt.to_request() {
//!     // Use request with your LLM client
//! }
//! ```

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use lingua::universal::{AssistantContent, TokenBudget, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

// ============================================================================
// PromptBuilder Error Types
// ============================================================================

/// Error type for PromptBuilder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum PromptBuilderError {
    /// Slug is required but was not provided.
    MissingSlug,
    /// Neither project_name nor project_id was provided.
    MissingProject,
    /// Both version and environment were specified (mutually exclusive).
    VersionAndEnvironmentConflict,
    /// The API request to fetch the prompt failed.
    FetchFailed(String),
}

impl fmt::Display for PromptBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSlug => write!(f, "slug is required but was not provided"),
            Self::MissingProject => {
                write!(f, "either project_name or project_id is required")
            }
            Self::VersionAndEnvironmentConflict => {
                write!(f, "version and environment are mutually exclusive")
            }
            Self::FetchFailed(msg) => write!(f, "failed to fetch prompt: {}", msg),
        }
    }
}

impl std::error::Error for PromptBuilderError {}

// ============================================================================
// Prompt Fetch Types (Internal)
// ============================================================================

/// Request parameters for fetching a prompt.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PromptFetchRequest {
    /// Project ID to fetch from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Project name to fetch from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// Prompt slug identifier.
    pub slug: String,
    /// Specific version (transaction ID).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Environment name (e.g., "production").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// Response from fetching a prompt.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PromptFetchResponse {
    /// Unique prompt ID.
    pub id: String,
    /// Prompt name.
    pub name: String,
    /// Prompt slug.
    pub slug: String,
    /// Current version (transaction ID).
    #[serde(rename = "_xact_id")]
    pub version: Option<String>,
    /// Project ID containing this prompt.
    pub project_id: String,
    /// Prompt template data (raw JSON).
    pub prompt_data: Option<Value>,
}

// ============================================================================
// PromptFetcher Trait
// ============================================================================

/// Trait for fetching prompts from the API.
#[async_trait]
pub(crate) trait PromptFetcher: Send + Sync {
    /// Fetch a prompt from the API.
    async fn fetch_prompt(
        &self,
        token: &str,
        request: PromptFetchRequest,
    ) -> Result<PromptFetchResponse>;
}

// ============================================================================
// Prompt - The main prompt handle
// ============================================================================

/// A loaded prompt template.
///
/// Use [`PromptBuilder`] to load a prompt, then call [`Prompt::to_request`] to
/// convert it to a [`lingua::UniversalRequest`] for use with any LLM provider.
#[derive(Debug, Clone)]
pub struct Prompt {
    id: String,
    name: String,
    slug: String,
    version: String,
    project_id: String,
    prompt_data: Value,
}

impl Prompt {
    /// Get the prompt ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Get the prompt name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the prompt slug.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// Get the prompt version (transaction ID).
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get the project ID containing this prompt.
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get the raw prompt data from the Braintrust API.
    pub fn data(&self) -> &Value {
        &self.prompt_data
    }

    /// Convert this prompt to a `lingua::UniversalRequest`.
    ///
    /// Returns `None` if the prompt is not a chat prompt (completion prompts are
    /// not yet supported) or if the prompt data cannot be parsed.
    pub fn to_request(&self) -> Option<lingua::UniversalRequest> {
        let prompt_obj = self.prompt_data.get("prompt")?;
        let prompt_type = prompt_obj.get("type")?.as_str()?;

        if prompt_type != "chat" {
            return None;
        }

        let messages_arr = prompt_obj.get("messages")?.as_array()?;
        let mut messages = Vec::new();

        for msg in messages_arr {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            let content_val = msg.get("content").unwrap_or(&Value::Null);

            let content_str = match content_val {
                Value::String(s) => s.clone(),
                Value::Array(_) => {
                    // Multi-part content: concatenate text parts
                    content_val
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|part| {
                            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                                part.get("text")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("")
                }
                _ => continue,
            };

            let message = match role {
                "system" => lingua::Message::System {
                    content: UserContent::String(content_str),
                },
                "developer" => lingua::Message::Developer {
                    content: UserContent::String(content_str),
                },
                "user" => lingua::Message::User {
                    content: UserContent::String(content_str),
                },
                "assistant" => lingua::Message::Assistant {
                    content: AssistantContent::String(content_str),
                    id: None,
                },
                _ => continue,
            };
            messages.push(message);
        }

        let options = self.prompt_data.get("options");
        let model = options
            .and_then(|o| o.get("model"))
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());

        let mut params = lingua::UniversalParams::default();
        if let Some(params_val) = options.and_then(|o| o.get("params")) {
            if let Some(t) = params_val.get("temperature").and_then(|v| v.as_f64()) {
                params.temperature = Some(t);
            }
            if let Some(tp) = params_val.get("top_p").and_then(|v| v.as_f64()) {
                params.top_p = Some(tp);
            }
            if let Some(mt) = params_val.get("max_tokens").and_then(|v| v.as_i64()) {
                params.token_budget = Some(TokenBudget::OutputTokens(mt));
            }
            if let Some(fp) = params_val.get("frequency_penalty").and_then(|v| v.as_f64()) {
                params.frequency_penalty = Some(fp);
            }
            if let Some(pp) = params_val.get("presence_penalty").and_then(|v| v.as_f64()) {
                params.presence_penalty = Some(pp);
            }
            if let Some(stop) = params_val.get("stop").and_then(|v| v.as_array()) {
                let stop_strs: Vec<String> = stop
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                if !stop_strs.is_empty() {
                    params.stop = Some(stop_strs);
                }
            }
        }

        Some(lingua::UniversalRequest {
            model,
            messages,
            params,
        })
    }
}

// ============================================================================
// PromptBuilder - Builder for loading prompts
// ============================================================================

/// Builder for loading prompts from Braintrust.
///
/// Use `client.prompt_builder_with_credentials()` to create a builder,
/// then configure it and call `build()` to fetch the prompt.
#[allow(private_bounds)]
pub struct PromptBuilder<S: PromptFetcher> {
    fetcher: Arc<S>,
    token: String,
    project_name: Option<String>,
    project_id: Option<String>,
    slug: Option<String>,
    version: Option<String>,
    environment: Option<String>,
}

impl<S: PromptFetcher> Clone for PromptBuilder<S> {
    fn clone(&self) -> Self {
        Self {
            fetcher: Arc::clone(&self.fetcher),
            token: self.token.clone(),
            project_name: self.project_name.clone(),
            project_id: self.project_id.clone(),
            slug: self.slug.clone(),
            version: self.version.clone(),
            environment: self.environment.clone(),
        }
    }
}

#[allow(private_bounds)]
impl<S: PromptFetcher + 'static> PromptBuilder<S> {
    /// Create a new PromptBuilder (internal use).
    pub(crate) fn new(fetcher: Arc<S>, token: impl Into<String>) -> Self {
        Self {
            fetcher,
            token: token.into(),
            project_name: None,
            project_id: None,
            slug: None,
            version: None,
            environment: None,
        }
    }

    /// Set the project name.
    pub fn project_name(mut self, name: impl Into<String>) -> Self {
        self.project_name = Some(name.into());
        self
    }

    /// Set the project ID (takes precedence over project_name).
    pub fn project_id(mut self, id: impl Into<String>) -> Self {
        self.project_id = Some(id.into());
        self
    }

    /// Set the prompt slug (required).
    pub fn slug(mut self, slug: impl Into<String>) -> Self {
        self.slug = Some(slug.into());
        self
    }

    /// Set a specific version (transaction ID) to fetch.
    ///
    /// Mutually exclusive with `environment`.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Set the environment to fetch (e.g., "production").
    ///
    /// Mutually exclusive with `version`.
    pub fn environment(mut self, env: impl Into<String>) -> Self {
        self.environment = Some(env.into());
        self
    }

    /// Build and fetch the prompt.
    ///
    /// Returns an error if required fields are missing or if the fetch fails.
    pub async fn build(self) -> std::result::Result<Prompt, PromptBuilderError> {
        let slug = self.slug.ok_or(PromptBuilderError::MissingSlug)?;

        if self.project_name.is_none() && self.project_id.is_none() {
            return Err(PromptBuilderError::MissingProject);
        }

        if self.version.is_some() && self.environment.is_some() {
            return Err(PromptBuilderError::VersionAndEnvironmentConflict);
        }

        let request = PromptFetchRequest {
            project_name: if self.project_id.is_some() {
                None
            } else {
                self.project_name.clone()
            },
            project_id: self.project_id.clone(),
            slug,
            version: self.version,
            environment: self.environment,
        };

        let response = self
            .fetcher
            .fetch_prompt(&self.token, request)
            .await
            .map_err(|e| PromptBuilderError::FetchFailed(e.to_string()))?;

        Ok(Prompt {
            id: response.id,
            name: response.name,
            slug: response.slug,
            version: response.version.unwrap_or_default(),
            project_id: response.project_id,
            prompt_data: response.prompt_data.unwrap_or(Value::Null),
        })
    }
}
