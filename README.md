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

## Auto-Instrumentation

**New in 0.1.0-alpha.2**: Automatically log AI SDK calls with zero manual code!

The SDK now includes `BraintrustTracingLayer`, a tracing subscriber that automatically captures OpenTelemetry GenAI semantic convention events from instrumented AI SDKs.

### Quick Start

```rust
use braintrust_sdk_rust::{BraintrustClient, BraintrustTracingLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize Braintrust
    let braintrust = BraintrustClient::builder()
        .api_url("https://api.braintrust.dev")
        .build()
        .await?;

    let span = braintrust
        .span_builder().await?
        .project_name("my-project")
        .build();

    // Install BraintrustTracingLayer - this is all you need!
    tracing_subscriber::registry()
        .with(BraintrustTracingLayer::new(span.clone()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Now all instrumented AI SDK calls are automatically logged!
    // No manual SpanLog::builder() calls needed.

    Ok(())
}
```

### Supported SDKs

Auto-instrumentation works with any SDK that emits OpenTelemetry GenAI semantic convention events via the `tracing` crate:

- **async-openai** (with `instrumentation` feature)
- **rust-genai / genai** (with `instrumentation` feature) - covers 11+ providers:
  - OpenAI, Anthropic, Gemini, Groq, Cohere, DeepSeek, Ollama, Fireworks, Together, Xai, BigModel, Aliyun, Mimo, Nebius

### Benefits

- ✅ **Zero manual logging code** - just install the layer once
- ✅ **Consistent format** across all providers
- ✅ **Standard conventions** - follows OpenTelemetry GenAI semantic conventions
- ✅ **Works with streaming** - automatically captures streaming responses
- ✅ **Low overhead** - async logging doesn't block your application

### How It Works

1. AI SDKs emit structured `tracing` events following [OpenTelemetry GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/)
2. `BraintrustTracingLayer` subscribes to these events
3. Events are automatically converted to Braintrust `SpanLog` entries
4. Logs are sent to Braintrust in the background

### Example with async-openai

```toml
[dependencies]
braintrust-sdk-rust = "0.1.0-alpha.2"
async-openai = { version = "0.33", features = ["instrumentation", "chat-completion"] }
tracing-subscriber = "0.3"
```

```rust
use async_openai::{types::CreateChatCompletionRequestArgs, Client};
use braintrust_sdk_rust::{BraintrustClient, BraintrustTracingLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup (one-time)
    let braintrust = BraintrustClient::builder()
        .api_url("https://api.braintrust.dev")
        .build().await?;
    let span = braintrust.span_builder().await?.project_name("my-project").build();

    tracing_subscriber::registry()
        .with(BraintrustTracingLayer::new(span.clone()))
        .init();

    // Use async-openai as normal - automatic logging!
    let client = Client::new();
    let request = CreateChatCompletionRequestArgs::default()
        .model("gpt-4")
        .messages(vec![/* ... */])
        .build()?;

    let response = client.chat().create(request).await?;
    // ↑ This call was automatically logged to Braintrust!

    span.flush().await?;
    Ok(())
}
```

See the [auto-instrumentation example](examples/auto_instrumentation.rs) for more details.

## Features

- **Auto-instrumentation**: Automatically capture AI SDK calls via OpenTelemetry GenAI conventions
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
