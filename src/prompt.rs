//! Prompt support for loading and building versioned prompt templates.
//!
//! Prompts allow you to:
//! - Fetch versioned prompt templates from Braintrust
//! - Build/render prompts with variable substitution
//! - Use environment-based versioning (e.g., "production")
//!
//! # Example
//!
//! ```ignore
//! let prompt = client
//!     .prompt_builder_with_credentials(&api_key, &org_id)
//!     .project_name("my-project")
//!     .slug("greeting-prompt")
//!     .environment("production")
//!     .build()
//!     .await?;
//!
//! let result = prompt.build(Some(hashmap! {
//!     "name".to_string() => json!("Alice"),
//! }));
//!
//! // Use result.chat() or result.completion() with your LLM
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
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
        }
    }
}

impl std::error::Error for PromptBuilderError {}

// ============================================================================
// Prompt Data Types
// ============================================================================

/// Model parameters for prompt execution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelParams {
    /// Sampling temperature (0.0 to 2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Nucleus sampling parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Frequency penalty (-2.0 to 2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    /// Presence penalty (-2.0 to 2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Response format (e.g., "json_object" or "text").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    /// Whether to use Braintrust caching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_cache: Option<bool>,
}

/// Response format configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResponseFormat {
    /// The format type: "json_object" or "text".
    #[serde(rename = "type")]
    pub format_type: String,
}

/// A single chat message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromptChatMessage {
    /// Role of the message sender: "system", "user", "assistant", "tool".
    pub role: String,
    /// Content of the message.
    pub content: MessageContent,
    /// Optional name for the message sender.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Tool calls made by the assistant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<PromptToolCall>>,
    /// Tool call ID this message is responding to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Content of a chat message - either a string or array of content parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Simple string content.
    Text(String),
    /// Array of content parts (text, images, etc.).
    Parts(Vec<ContentPart>),
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        MessageContent::Text(s)
    }
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        MessageContent::Text(s.to_string())
    }
}

/// A content part within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    /// Text content.
    #[serde(rename = "text")]
    Text {
        /// The text content.
        text: String,
    },
    /// Image URL content.
    #[serde(rename = "image_url")]
    ImageUrl {
        /// The image URL configuration.
        image_url: ImageUrlConfig,
    },
}

/// Image URL configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlConfig {
    /// The URL of the image.
    pub url: String,
    /// Detail level: "auto", "low", or "high".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A tool call made by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptToolCall {
    /// Unique identifier for the tool call.
    pub id: String,
    /// Type of tool call (always "function" for now).
    #[serde(rename = "type")]
    pub call_type: String,
    /// Function call details.
    pub function: PromptFunctionCall,
}

/// Function call details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFunctionCall {
    /// Name of the function to call.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// Tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTool {
    /// Type of tool (always "function" for now).
    #[serde(rename = "type")]
    pub tool_type: String,
    /// Function definition.
    pub function: PromptFunctionDef,
}

/// Function definition within a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptFunctionDef {
    /// Name of the function.
    pub name: String,
    /// Description of what the function does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the function parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

/// Chat-based prompt format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ChatPrompt {
    /// The chat messages.
    pub messages: Vec<PromptChatMessage>,
    /// Available tools for function calling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<PromptTool>>,
    /// Tool choice configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
}

/// Completion-based prompt format (single text prompt).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CompletionPrompt {
    /// The prompt text.
    pub prompt: String,
}

/// The type of prompt (chat or completion).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PromptType {
    /// Chat-based prompt with messages.
    #[serde(rename = "chat")]
    Chat(ChatPrompt),
    /// Completion-based prompt with single text.
    #[serde(rename = "completion")]
    Completion(CompletionPrompt),
}

/// Complete prompt data including template and configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromptData {
    /// The prompt content (chat or completion).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptType>,
    /// Model and parameter options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<PromptOptions>,
    /// Template format: "mustache", "nunjucks", or "none".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_format: Option<String>,
}

/// Model and parameter options for a prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PromptOptions {
    /// The model to use (e.g., "gpt-4").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<ModelParams>,
}

/// Result of building a prompt with variables.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PromptBuildResult {
    /// The rendered prompt.
    prompt: RenderedPrompt,
    /// Model and parameters.
    model: Option<String>,
    params: ModelParams,
}

impl PromptBuildResult {
    /// Get the rendered prompt as a chat prompt (if applicable).
    pub fn chat(&self) -> Option<&ChatPrompt> {
        match &self.prompt {
            RenderedPrompt::Chat(chat) => Some(chat),
            RenderedPrompt::Completion(_) => None,
        }
    }

    /// Get the rendered prompt as a completion prompt (if applicable).
    pub fn completion(&self) -> Option<&CompletionPrompt> {
        match &self.prompt {
            RenderedPrompt::Chat(_) => None,
            RenderedPrompt::Completion(completion) => Some(completion),
        }
    }

    /// Get the model name.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Get the model parameters.
    pub fn params(&self) -> &ModelParams {
        &self.params
    }

    /// Check if this is a chat prompt.
    pub fn is_chat(&self) -> bool {
        matches!(self.prompt, RenderedPrompt::Chat(_))
    }

    /// Check if this is a completion prompt.
    pub fn is_completion(&self) -> bool {
        matches!(self.prompt, RenderedPrompt::Completion(_))
    }
}

/// Rendered prompt content (after variable substitution).
#[derive(Debug, Clone)]
pub enum RenderedPrompt {
    /// Chat-based rendered prompt.
    Chat(ChatPrompt),
    /// Completion-based rendered prompt.
    Completion(CompletionPrompt),
}

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
    /// Prompt template data.
    pub prompt_data: Option<PromptData>,
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
/// Use `PromptBuilder` to load a prompt, then call `build()` to render it
/// with variable substitution.
#[derive(Debug, Clone)]
pub struct Prompt {
    id: String,
    name: String,
    slug: String,
    version: String,
    project_id: String,
    data: PromptData,
    defaults: HashMap<String, Value>,
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

    /// Get the raw prompt data.
    pub fn data(&self) -> &PromptData {
        &self.data
    }

    /// Build/render the prompt with variable substitution.
    ///
    /// Variables are merged with defaults (runtime variables take precedence).
    /// Returns the rendered prompt ready for use with an LLM.
    pub fn build(&self, variables: Option<HashMap<String, Value>>) -> PromptBuildResult {
        // Merge defaults with provided variables (provided takes precedence)
        let merged_vars = if let Some(vars) = variables {
            let mut merged = self.defaults.clone();
            merged.extend(vars);
            merged
        } else {
            self.defaults.clone()
        };

        // Render the prompt based on template format
        let rendered = self.render_prompt(&merged_vars);

        // Extract model and params
        let (model, params) = if let Some(options) = &self.data.options {
            (
                options.model.clone(),
                options.params.clone().unwrap_or_default(),
            )
        } else {
            (None, ModelParams::default())
        };

        PromptBuildResult {
            prompt: rendered,
            model,
            params,
        }
    }

    /// Render the prompt with variable substitution.
    fn render_prompt(&self, variables: &HashMap<String, Value>) -> RenderedPrompt {
        let template_format = self.data.template_format.as_deref().unwrap_or("mustache");

        match &self.data.prompt {
            Some(PromptType::Chat(chat)) => {
                let rendered_messages = chat
                    .messages
                    .iter()
                    .map(|msg| self.render_message(msg, variables, template_format))
                    .collect();

                RenderedPrompt::Chat(ChatPrompt {
                    messages: rendered_messages,
                    tools: chat.tools.clone(),
                    tool_choice: chat.tool_choice.clone(),
                })
            }
            Some(PromptType::Completion(completion)) => {
                let rendered_text =
                    self.substitute_variables(&completion.prompt, variables, template_format);
                RenderedPrompt::Completion(CompletionPrompt {
                    prompt: rendered_text,
                })
            }
            None => {
                // Default to empty chat prompt
                RenderedPrompt::Chat(ChatPrompt {
                    messages: vec![],
                    tools: None,
                    tool_choice: None,
                })
            }
        }
    }

    /// Render a single chat message with variable substitution.
    fn render_message(
        &self,
        message: &PromptChatMessage,
        variables: &HashMap<String, Value>,
        template_format: &str,
    ) -> PromptChatMessage {
        let rendered_content = match &message.content {
            MessageContent::Text(text) => {
                MessageContent::Text(self.substitute_variables(text, variables, template_format))
            }
            MessageContent::Parts(parts) => {
                let rendered_parts = parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => ContentPart::Text {
                            text: self.substitute_variables(text, variables, template_format),
                        },
                        other => other.clone(),
                    })
                    .collect();
                MessageContent::Parts(rendered_parts)
            }
        };

        PromptChatMessage {
            role: message.role.clone(),
            content: rendered_content,
            name: message.name.clone(),
            tool_calls: message.tool_calls.clone(),
            tool_call_id: message.tool_call_id.clone(),
        }
    }

    /// Substitute variables in a template string.
    ///
    /// Supports simple Mustache-style {{variable}} substitution.
    fn substitute_variables(
        &self,
        template: &str,
        variables: &HashMap<String, Value>,
        template_format: &str,
    ) -> String {
        if template_format == "none" {
            return template.to_string();
        }

        // Simple Mustache-style variable substitution: {{variable}}
        let mut result = template.to_string();
        for (key, value) in variables {
            let pattern = format!("{{{{{}}}}}", key);
            let replacement = match value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => String::new(),
                _ => value.to_string(),
            };
            result = result.replace(&pattern, &replacement);
        }

        result
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
    org_id: String,
    project_name: Option<String>,
    project_id: Option<String>,
    slug: Option<String>,
    version: Option<String>,
    environment: Option<String>,
    defaults: HashMap<String, Value>,
}

impl<S: PromptFetcher> Clone for PromptBuilder<S> {
    fn clone(&self) -> Self {
        Self {
            fetcher: Arc::clone(&self.fetcher),
            token: self.token.clone(),
            org_id: self.org_id.clone(),
            project_name: self.project_name.clone(),
            project_id: self.project_id.clone(),
            slug: self.slug.clone(),
            version: self.version.clone(),
            environment: self.environment.clone(),
            defaults: self.defaults.clone(),
        }
    }
}

#[allow(private_bounds)]
impl<S: PromptFetcher + 'static> PromptBuilder<S> {
    /// Create a new PromptBuilder (internal use).
    pub(crate) fn new(
        fetcher: Arc<S>,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> Self {
        Self {
            fetcher,
            token: token.into(),
            org_id: org_id.into(),
            project_name: None,
            project_id: None,
            slug: None,
            version: None,
            environment: None,
            defaults: HashMap::new(),
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

    /// Set default variables for template substitution.
    ///
    /// These are merged with variables provided at build time
    /// (runtime variables take precedence).
    pub fn defaults(mut self, defaults: HashMap<String, Value>) -> Self {
        self.defaults = defaults;
        self
    }

    /// Add a single default variable.
    pub fn default_var(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.defaults.insert(key.into(), value.into());
        self
    }

    /// Build and fetch the prompt.
    ///
    /// Returns an error if required fields are missing or if the fetch fails.
    pub async fn build(self) -> std::result::Result<Prompt, PromptBuilderError> {
        // Validate required fields
        let slug = self.slug.ok_or(PromptBuilderError::MissingSlug)?;

        if self.project_name.is_none() && self.project_id.is_none() {
            return Err(PromptBuilderError::MissingProject);
        }

        if self.version.is_some() && self.environment.is_some() {
            return Err(PromptBuilderError::VersionAndEnvironmentConflict);
        }

        // Build the request
        let request = PromptFetchRequest {
            project_id: self.project_id.clone(),
            project_name: self.project_name.clone(),
            slug,
            version: self.version,
            environment: self.environment,
        };

        // Fetch the prompt
        let response = self
            .fetcher
            .fetch_prompt(&self.token, request)
            .await
            .map_err(|_| PromptBuilderError::MissingSlug)?; // TODO: Better error mapping

        // Build the Prompt
        Ok(Prompt {
            id: response.id,
            name: response.name,
            slug: response.slug,
            version: response.version.unwrap_or_default(),
            project_id: response.project_id,
            data: response.prompt_data.unwrap_or(PromptData {
                prompt: None,
                options: None,
                template_format: None,
            }),
            defaults: self.defaults,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_variable_substitution() {
        let prompt = Prompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            slug: "test-prompt".to_string(),
            version: "1".to_string(),
            project_id: "proj-id".to_string(),
            data: PromptData {
                prompt: Some(PromptType::Completion(CompletionPrompt {
                    prompt: "Hello, {{name}}! Welcome to {{place}}.".to_string(),
                })),
                options: None,
                template_format: Some("mustache".to_string()),
            },
            defaults: HashMap::new(),
        };

        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("Alice".to_string()));
        vars.insert("place".to_string(), Value::String("Wonderland".to_string()));

        let result = prompt.build(Some(vars));
        let completion = result.completion().expect("should be completion");
        assert_eq!(completion.prompt, "Hello, Alice! Welcome to Wonderland.");
    }

    #[test]
    fn test_chat_message_substitution() {
        let prompt = Prompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            slug: "test-prompt".to_string(),
            version: "1".to_string(),
            project_id: "proj-id".to_string(),
            data: PromptData {
                prompt: Some(PromptType::Chat(ChatPrompt {
                    messages: vec![
                        PromptChatMessage {
                            role: "system".to_string(),
                            content: MessageContent::Text(
                                "You are a helpful assistant for {{company}}.".to_string(),
                            ),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                        },
                        PromptChatMessage {
                            role: "user".to_string(),
                            content: MessageContent::Text(
                                "Hello, my name is {{user}}.".to_string(),
                            ),
                            name: None,
                            tool_calls: None,
                            tool_call_id: None,
                        },
                    ],
                    tools: None,
                    tool_choice: None,
                })),
                options: Some(PromptOptions {
                    model: Some("gpt-4".to_string()),
                    params: Some(ModelParams {
                        temperature: Some(0.7),
                        ..Default::default()
                    }),
                }),
                template_format: Some("mustache".to_string()),
            },
            defaults: HashMap::new(),
        };

        let mut vars = HashMap::new();
        vars.insert("company".to_string(), Value::String("Acme Inc".to_string()));
        vars.insert("user".to_string(), Value::String("Bob".to_string()));

        let result = prompt.build(Some(vars));

        assert!(result.is_chat());
        let chat = result.chat().expect("should be chat");
        assert_eq!(chat.messages.len(), 2);

        match &chat.messages[0].content {
            MessageContent::Text(text) => {
                assert_eq!(text, "You are a helpful assistant for Acme Inc.");
            }
            _ => panic!("expected text content"),
        }

        match &chat.messages[1].content {
            MessageContent::Text(text) => {
                assert_eq!(text, "Hello, my name is Bob.");
            }
            _ => panic!("expected text content"),
        }

        assert_eq!(result.model(), Some("gpt-4"));
        assert_eq!(result.params().temperature, Some(0.7));
    }

    #[test]
    fn test_defaults_merged_with_variables() {
        let mut defaults = HashMap::new();
        defaults.insert("name".to_string(), Value::String("Default".to_string()));
        defaults.insert("greeting".to_string(), Value::String("Hello".to_string()));

        let prompt = Prompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            slug: "test-prompt".to_string(),
            version: "1".to_string(),
            project_id: "proj-id".to_string(),
            data: PromptData {
                prompt: Some(PromptType::Completion(CompletionPrompt {
                    prompt: "{{greeting}}, {{name}}!".to_string(),
                })),
                options: None,
                template_format: Some("mustache".to_string()),
            },
            defaults,
        };

        // Override only "name", keep default "greeting"
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("Alice".to_string()));

        let result = prompt.build(Some(vars));
        let completion = result.completion().expect("should be completion");
        assert_eq!(completion.prompt, "Hello, Alice!");
    }

    #[test]
    fn test_no_substitution_when_format_none() {
        let prompt = Prompt {
            id: "test-id".to_string(),
            name: "Test Prompt".to_string(),
            slug: "test-prompt".to_string(),
            version: "1".to_string(),
            project_id: "proj-id".to_string(),
            data: PromptData {
                prompt: Some(PromptType::Completion(CompletionPrompt {
                    prompt: "Hello, {{name}}!".to_string(),
                })),
                options: None,
                template_format: Some("none".to_string()),
            },
            defaults: HashMap::new(),
        };

        let mut vars = HashMap::new();
        vars.insert("name".to_string(), Value::String("Alice".to_string()));

        let result = prompt.build(Some(vars));
        let completion = result.completion().expect("should be completion");
        // Should NOT substitute when format is "none"
        assert_eq!(completion.prompt, "Hello, {{name}}!");
    }
}
