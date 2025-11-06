//! Instrumented LLM client using Lingua with automatic Braintrust tracing
//!
//! This module provides a high-level LLM client that automatically instruments all
//! requests with Braintrust spans, capturing inputs, outputs, timing metrics, and
//! token usage.
//!
//! # Example
//!
//! ```no_run
//! use braintrust_sdk::lingua::{execute_instrumented_request, LinguaClientConfig, parse_response_text};
//! use braintrust_sdk::{Logger, LoggerOptions};
//! use lingua::universal::Message;
//! use lingua::providers::openai::generated::CreateResponseClass;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! // Initialize logger
//! let logger = Logger::new_async(LoggerOptions::new("my-project")).await?;
//!
//! // Configure client
//! let config = LinguaClientConfig::new()?;
//!
//! // Create messages
//! let messages = vec![
//!     Message::System {
//!         content: "You are a helpful assistant.".into()
//!     },
//!     Message::User {
//!         content: "What is 2+2?".into()
//!     }
//! ];
//!
//! // Set parameters
//! let mut parameters = CreateResponseClass::default();
//! parameters.model = "gpt-4".to_string();
//! parameters.temperature = Some(0.7);
//!
//! // Execute instrumented request
//! let response = execute_instrumented_request(
//!     &logger,
//!     messages,
//!     parameters,
//!     &config,
//! ).await?;
//!
//! // Parse response
//! let parsed = parse_response_text(&response)?;
//! println!("Output: {}", parsed.output_text);
//! println!("Reasoning: {}", parsed.reasoning_text);
//!
//! // Flush traces
//! logger.flush().await?;
//! # Ok(())
//! # }
//! ```

mod client;
mod error;

pub use client::{execute_instrumented_request, LinguaClientConfig};
pub use error::{LinguaError, Result};

use lingua::universal::{AssistantContent, AssistantContentPart, Message};

/// Parsed response from an LLM request
#[derive(Debug, Clone)]
pub struct ParsedResponse {
    /// The main output text from the assistant
    pub output_text: String,

    /// Any reasoning text (for models that support it, like o1)
    pub reasoning_text: String,
}

/// Parse response messages to extract output and reasoning text
///
/// This helper function extracts the main output text and any reasoning content
/// from the LLM response messages.
///
/// # Arguments
///
/// * `messages` - Response messages from the LLM
///
/// # Returns
///
/// A `ParsedResponse` containing the extracted text
///
/// # Errors
///
/// Returns an error if no output text is found in the response
pub fn parse_response_text(messages: &[Message]) -> Result<ParsedResponse> {
    // Extract output text from assistant messages
    let output_text = messages
        .iter()
        .find_map(|msg| {
            if let Message::Assistant { content, .. } = msg {
                match content {
                    AssistantContent::String(s) => Some(s.as_str()),
                    AssistantContent::Array(arr) => arr.iter().find_map(|part| {
                        if let AssistantContentPart::Text(text_content) = part {
                            Some(text_content.text.as_str())
                        } else {
                            None
                        }
                    }),
                }
            } else {
                None
            }
        })
        .ok_or_else(|| LinguaError::NoOutputText)?
        .to_string();

    // Extract reasoning text (joins all reasoning parts)
    let reasoning_text = messages
        .iter()
        .flat_map(|msg| {
            if let Message::Assistant { content, .. } = msg {
                match content {
                    AssistantContent::String(_) => vec![],
                    AssistantContent::Array(arr) => arr
                        .iter()
                        .filter_map(|part| {
                            if let AssistantContentPart::Reasoning { text, .. } = part {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>(),
                }
            } else {
                vec![]
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ParsedResponse {
        output_text,
        reasoning_text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_response() {
        let messages = vec![Message::Assistant {
            content: AssistantContent::String("Hello, world!".to_string()),
            id: None,
        }];

        let parsed = parse_response_text(&messages).unwrap();
        assert_eq!(parsed.output_text, "Hello, world!");
        assert_eq!(parsed.reasoning_text, "");
    }

    #[test]
    fn test_parse_empty_response() {
        let messages = vec![];
        let result = parse_response_text(&messages);
        assert!(result.is_err());
    }
}
