//! Instrumented LLM client using Lingua with automatic Braintrust tracing

use crate::{Logger, SpanType, StartSpanOptions};
use super::error::{LinguaError, Result};
use lingua::{
    providers::openai::generated::{CreateResponseClass, TheResponseObject},
    universal::{convert::TryFromLLM, Message},
};
use reqwest::Client;
use serde_json::json;
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default API URL for OpenAI
const DEFAULT_API_URL: &str = "https://api.openai.com/v1";

/// Configuration for the instrumented LLM client
#[derive(Debug, Clone)]
pub struct LinguaClientConfig {
    /// API URL (defaults to https://api.openai.com/v1)
    pub api_url: String,

    /// API key for authentication (defaults to OPENAI_API_KEY env var)
    pub api_key: String,
}

impl LinguaClientConfig {
    /// Create a new configuration with defaults from environment
    pub fn new() -> Result<Self> {
        let api_key = env::var("OPENAI_API_KEY")
            .map_err(|_| LinguaError::MissingEnvVar("OPENAI_API_KEY".to_string()))?;

        Ok(Self {
            api_url: env::var("OPENAI_API_URL")
                .unwrap_or_else(|_| DEFAULT_API_URL.to_string()),
            api_key,
        })
    }

    /// Create a configuration with custom API URL
    pub fn with_api_url(mut self, url: impl Into<String>) -> Self {
        self.api_url = url.into();
        self
    }

    /// Create a configuration with custom API key
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = key.into();
        self
    }
}

impl Default for LinguaClientConfig {
    fn default() -> Self {
        Self::new().expect("Failed to create default LinguaClientConfig")
    }
}

/// Execute an instrumented LLM request with automatic Braintrust tracing
///
/// This function wraps an LLM API call with a Braintrust span that automatically logs:
/// - Input messages
/// - Output messages
/// - Timing metrics (start, end, duration)
/// - Token usage (prompt_tokens, completion_tokens, total_tokens)
/// - Model metadata (model name, temperature, seed, etc.)
///
/// # Arguments
///
/// * `logger` - Braintrust logger for tracing
/// * `messages` - Input messages in Lingua's universal format
/// * `parameters` - OpenAI-style request parameters
/// * `config` - Client configuration (proxy URL, API key)
///
/// # Returns
///
/// Vec of response messages in Lingua's universal format
///
/// # Example
///
/// ```no_run
/// use braintrust_sdk::lingua::{execute_instrumented_request, LinguaClientConfig};
/// use braintrust_sdk::{Logger, LoggerOptions};
/// use lingua::universal::Message;
/// use lingua::providers::openai::generated::CreateResponseClass;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let logger = Logger::new_async(LoggerOptions::new("my-project")).await?;
/// let config = LinguaClientConfig::new()?;
///
/// let messages = vec![
///     Message::User {
///         content: "What is 2+2?".into()
///     }
/// ];
///
/// let mut parameters = CreateResponseClass::default();
/// parameters.model = "gpt-4".to_string();
///
/// let response = execute_instrumented_request(
///     &logger,
///     messages,
///     parameters,
///     &config,
/// ).await?;
/// # Ok(())
/// # }
/// ```
pub async fn execute_instrumented_request(
    logger: &Logger,
    messages: Vec<Message>,
    mut parameters: CreateResponseClass,
    config: &LinguaClientConfig,
) -> Result<Vec<Message>> {
    // Start a span for this LLM call
    let span = logger.start_span(Some(
        StartSpanOptions::new()
            .with_name("llm_request")
            .with_type(SpanType::Llm),
    ));

    // Record start time
    let start_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    // Log input messages
    let input_json = serde_json::to_value(&messages)?;

    // Extract temperature from request (model will come from response)
    let temperature = parameters.temperature;

    // Execute the request and handle errors
    let result = execute_request_impl(&messages, &mut parameters, config).await;

    // Record end time
    let end_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let duration = end_time - start_time;

    match result {
        Ok((response_messages, usage, model)) => {
            // Log successful response
            let output_json = serde_json::to_value(&response_messages)?;

            // Build metadata
            let mut metadata = serde_json::Map::new();
            metadata.insert("model".to_string(), json!(model));
            if let Some(temp) = temperature {
                metadata.insert("temperature".to_string(), json!(temp));
            }

            // Build metrics
            let mut metrics = serde_json::Map::new();
            metrics.insert("start".to_string(), json!(start_time));
            metrics.insert("end".to_string(), json!(end_time));
            metrics.insert("duration".to_string(), json!(duration));

            // Add token usage if available
            if let Some(usage_data) = usage {
                metrics.insert("prompt_tokens".to_string(), json!(usage_data.input_tokens));
                metrics.insert("completion_tokens".to_string(), json!(usage_data.output_tokens));
                metrics.insert("tokens".to_string(), json!(usage_data.total_tokens));
            }

            // Log everything to the span
            span.log(json!({
                "input": input_json,
                "output": output_json,
                "metadata": metadata,
                "metrics": metrics,
            }))?;

            span.end()?;
            Ok(response_messages)
        }
        Err(e) => {
            // Log error
            span.log(json!({
                "input": input_json,
                "error": e.to_string(),
                "metrics": json!({
                    "start": start_time,
                    "end": end_time,
                    "duration": duration,
                }),
            }))?;

            span.end()?;
            Err(e)
        }
    }
}

/// Internal implementation of the LLM request
async fn execute_request_impl(
    messages: &[Message],
    parameters: &mut CreateResponseClass,
    config: &LinguaClientConfig,
) -> Result<(Vec<Message>, Option<lingua::providers::openai::generated::ResponseUsage>, String)> {
    // Convert universal messages to OpenAI InputItem format
    let request_messages: Vec<lingua::providers::openai::generated::InputItem> =
        TryFromLLM::try_from(messages.to_vec())
            .map_err(|e| LinguaError::MessageConversion(format!("{:?}", e)))?;

    // Set the input messages
    parameters.input = Some(
        lingua::providers::openai::generated::Instructions::InputItemArray(request_messages),
    );

    // Serialize parameters to JSON for the request
    let parameters_json = serde_json::to_value(parameters)?;

    // Make HTTP request to responses API
    let client = Client::new();
    let response = client
        .post(format!("{}/responses", config.api_url))
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&parameters_json)
        .send()
        .await?;

    // Check for HTTP errors
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_else(|_| String::from("Unknown error"));
        return Err(LinguaError::LlmRequestError {
            status_code: status,
            body,
        });
    }

    // Parse response
    let result_json: TheResponseObject = response.json().await?;

    // Extract usage information and model
    let usage = result_json.usage.clone();
    let model = result_json.model.clone();

    // Convert response through two-step process: OutputItem -> InputItem -> Message
    let input_items: Vec<lingua::providers::openai::generated::InputItem> =
        TryFromLLM::try_from(result_json.output)
            .map_err(|e| LinguaError::ResponseParsing(format!("{:?}", e)))?;

    let result_messages: Vec<Message> = TryFromLLM::try_from(input_items)
        .map_err(|e| LinguaError::ResponseParsing(format!("{:?}", e)))?;

    Ok((result_messages, usage, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config = LinguaClientConfig {
            api_url: "http://test:8000".to_string(),
            api_key: "test-key".to_string(),
        };

        assert_eq!(config.api_url, "http://test:8000");
        assert_eq!(config.api_key, "test-key");
    }

    #[test]
    fn test_config_builder() {
        let config = LinguaClientConfig {
            api_url: DEFAULT_API_URL.to_string(),
            api_key: "key".to_string(),
        }
        .with_api_url("http://custom:9000")
        .with_api_key("custom-key");

        assert_eq!(config.api_url, "http://custom:9000");
        assert_eq!(config.api_key, "custom-key");
    }
}
