# braintrust-sdk-rust

Rust SDK for [Braintrust](https://braintrust.dev) logging and tracing.

> **Early Development Notice**: This SDK is in early development (alpha). Expect backwards-incompatible changes between releases until we reach 1.0.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
braintrust-sdk-rust = "0.1.0-alpha.1"
```

## Usage

```rust
use braintrust_sdk_rust::{BraintrustClient, BraintrustClientConfig, SpanLog};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create a client pointing to your Braintrust instance
    let client = BraintrustClient::new(
        BraintrustClientConfig::new("https://api.braintrust.dev")
    )?;

    // Create a span for logging
    let span = client
        .span_builder("your-api-token", "your-org-id")
        .project_name("my-project")
        .build();

    // Log input/output using span.log() with the builder pattern
    span.log(
        SpanLog::builder()
            .input(json!({"prompt": "Hello, world!"}))
            .output(json!({"response": "Hi there!"}))
            .build()
            .expect("Failed to build SpanLog")
    ).await;

    // Flush the span data to Braintrust
    span.flush().await?;
    client.flush().await?;

    Ok(())
}
```

## Features

- **Span-based logging**: Create spans to track LLM calls and other operations
- **Usage metrics extraction**: Built-in extractors for OpenAI and Anthropic usage metrics
- **Async-first**: Built on Tokio for high-performance async operations
- **Background submission**: Logs are submitted in the background to minimize latency

## Extracting Usage Metrics

The SDK includes helpers for extracting usage metrics from provider responses:

```rust
use braintrust_sdk_rust::{extract_openai_usage, extract_anthropic_usage};
use serde_json::json;

// Extract from OpenAI response
let openai_response = json!({
    "usage": {
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "total_tokens": 150
    }
});
let usage = extract_openai_usage(&openai_response);

// Extract from Anthropic response
let anthropic_response = json!({
    "usage": {
        "input_tokens": 100,
        "output_tokens": 50
    }
});
let usage = extract_anthropic_usage(&anthropic_response);
```

## License

This project is licensed under the [Apache License, Version 2.0](LICENSE).
