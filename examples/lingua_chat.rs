//! Example demonstrating instrumented LLM calls with Lingua and Braintrust
//!
//! This example shows how to:
//! 1. Initialize the Braintrust logger
//! 2. Make LLM requests using the instrumented Lingua client
//! 3. Automatically capture traces, metrics, and token usage
//! 4. Parse and display responses
//!
//! Prerequisites:
//! - Set BRAINTRUST_API_KEY environment variable
//! - Optionally set BRAINTRUST_PROXY_URL (defaults to http://localhost:8000/v1/proxy)
//! - Ensure Braintrust proxy is running

use braintrust_sdk::lingua::{
    execute_instrumented_request, parse_response_text, LinguaClientConfig,
};
use braintrust_sdk::{Logger, LoggerOptions};
use lingua::providers::openai::generated::CreateResponseClass;
use lingua::universal::{Message, UserContent};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("🚀 Starting Instrumented Lingua Chat Example");
    println!();

    // Initialize Braintrust logger
    let logger = Logger::new_async(LoggerOptions::new("lingua-chat-example")).await?;
    println!("✅ Logger initialized for project: {}", logger.project_id());
    println!();

    // Configure Lingua client
    let config = LinguaClientConfig::new()?;
    println!("✅ Lingua client configured");
    println!("   API URL: {}", config.api_url);
    println!();

    // Example 1: Simple Q&A
    println!("📝 Example 1: Simple Question");
    println!("─────────────────────────────");

    let messages = vec![
        Message::System {
            content: UserContent::String("You are a helpful math tutor.".to_string()),
        },
        Message::User {
            content: UserContent::String("What is 15 * 23?".to_string()),
        },
    ];

    let parameters = CreateResponseClass {
        metadata: None,
        prompt_cache_key: None,
        safety_identifier: None,
        service_tier: None,
        temperature: None,
        top_logprobs: None,
        top_p: None,
        user: None,
        background: None,
        max_output_tokens: None,
        max_tool_calls: None,
        model: Some("gpt-5-nano".to_string()),
        previous_response_id: None,
        prompt: None,
        reasoning: None,
        text: None,
        tool_choice: None,
        tools: None,
        truncation: None,
        conversation: None,
        include: None,
        input: None,
        instructions: None,
        parallel_tool_calls: None,
        store: None,
        stream: None,
        stream_options: None,
    };

    println!("Sending request to model: {}", parameters.model.as_ref().unwrap_or(&"unknown".to_string()));
    let response = execute_instrumented_request(&logger, messages, parameters, &config).await?;

    let parsed = parse_response_text(&response)?;
    println!("Response: {}", parsed.output_text);
    if !parsed.reasoning_text.is_empty() {
        println!("Reasoning: {}", parsed.reasoning_text);
    }
    println!();

    // Example 2: Multi-turn conversation
    println!("📝 Example 2: Multi-turn Conversation");
    println!("─────────────────────────────────────");

    let conversation = vec![
        Message::System {
            content: UserContent::String("You are a creative writing assistant.".to_string()),
        },
        Message::User {
            content: UserContent::String("Tell me a very short story about a robot learning to paint.".to_string()),
        },
    ];

    let parameters2 = CreateResponseClass {
        metadata: None,
        prompt_cache_key: None,
        safety_identifier: None,
        service_tier: None,
        temperature: None,
        top_logprobs: None,
        top_p: None,
        user: None,
        background: None,
        max_output_tokens: None,
        max_tool_calls: None,
        model: Some("gpt-5-nano".to_string()),
        previous_response_id: None,
        prompt: None,
        reasoning: None,
        text: None,
        tool_choice: None,
        tools: None,
        truncation: None,
        conversation: None,
        include: None,
        input: None,
        instructions: None,
        parallel_tool_calls: None,
        store: None,
        stream: None,
        stream_options: None,
    };

    println!("Sending creative writing request...");
    let response2 =
        execute_instrumented_request(&logger, conversation, parameters2, &config).await?;

    let parsed2 = parse_response_text(&response2)?;
    println!("Story: {}", parsed2.output_text);
    println!();

    // Example 3: With reasoning (if using o1 model)
    println!("📝 Example 3: Complex Problem (with reasoning if available)");
    println!("──────────────────────────────────────────────────────────");

    let complex_messages = vec![
        Message::System {
            content: UserContent::String("You are a helpful problem-solving assistant.".to_string()),
        },
        Message::User {
            content: UserContent::String("If I have 3 apples and buy 2 more, then give half away, how many do I have?".to_string()),
        },
    ];

    let parameters3 = CreateResponseClass {
        metadata: None,
        prompt_cache_key: None,
        safety_identifier: None,
        service_tier: None,
        temperature: None,
        top_logprobs: None,
        top_p: None,
        user: None,
        background: None,
        max_output_tokens: None,
        max_tool_calls: None,
        model: Some("gpt-5-nano".to_string()),
        previous_response_id: None,
        prompt: None,
        reasoning: None,
        text: None,
        tool_choice: None,
        tools: None,
        truncation: None,
        conversation: None,
        include: None,
        input: None,
        instructions: None,
        parallel_tool_calls: None,
        store: None,
        stream: None,
        stream_options: None,
    };

    println!("Sending problem-solving request...");
    let response3 =
        execute_instrumented_request(&logger, complex_messages, parameters3, &config).await?;

    let parsed3 = parse_response_text(&response3)?;
    println!("Answer: {}", parsed3.output_text);
    if !parsed3.reasoning_text.is_empty() {
        println!("Model's Reasoning Process:");
        println!("{}", parsed3.reasoning_text);
    }
    println!();

    // Flush all traces to Braintrust
    logger.flush().await?;
    println!("✅ All traces flushed to Braintrust");
    println!();

    println!("🎉 Example completed successfully!");
    println!("   View your traces at: https://www.braintrust.dev");
    println!();
    println!("   Each LLM call was automatically traced with:");
    println!("   - Input and output messages");
    println!("   - Timing metrics (start, end, duration)");
    println!("   - Token usage (prompt, completion, total)");
    println!("   - Model metadata (name, temperature, max_tokens)");

    Ok(())
}
