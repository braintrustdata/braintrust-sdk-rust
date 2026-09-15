//! Auto-instrumentation example
//!
//! This example demonstrates how to use `BraintrustTracingLayer` to automatically
//! capture OpenTelemetry GenAI semantic convention events from instrumented AI SDKs.
//!
//! The full pipeline is:
//!
//! ```text
//! AI library (emitting OTel GenAI spans)
//!   → OpenTelemetry SDK
//!   → tracing-opentelemetry bridge
//!   → BraintrustTracingLayer
//!   → Braintrust
//! ```
//!
//! AI libraries that emit OpenTelemetry GenAI semantic conventions (span names
//! starting with `gen_ai.*`) will be captured automatically. This example
//! demonstrates the full pipeline using the OpenTelemetry SDK directly to show
//! what an instrumented AI library would produce.

use anyhow::Result;
use braintrust_sdk_rust::{BraintrustClient, BraintrustTracingLayer};
use opentelemetry::global;
use opentelemetry::trace::{Tracer, TracerProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_sdk::trace::{Sampler, TracerProvider};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize Braintrust (use skip_login for demo; use api_key in production)
    let braintrust = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await?;

    let span = braintrust
        .span_builder_with_credentials("demo-key", "demo-org")
        .project_name("auto-instrumentation-demo")
        .build();

    // Set up OpenTelemetry SDK with a tracer provider.
    // In production, configure an OTLP exporter to send spans to a collector.
    let provider = TracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .build();
    global::set_tracer_provider(provider.clone());
    let tracer = provider.tracer("ai-library");

    // Set up the full subscriber stack:
    //   1. BraintrustTracingLayer — captures gen_ai.* spans and logs them to Braintrust
    //   2. tracing-opentelemetry — bridges OTel spans into the tracing ecosystem
    //   3. fmt layer — prints spans to stdout for visibility
    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(BraintrustTracingLayer::new(span.clone()))
        .with(telemetry)
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Simulate an AI library calling OpenAI.
    // AI libraries that follow the OTel GenAI semantic conventions
    // (https://opentelemetry.io/docs/specs/semconv/gen-ai/) emit spans like
    // these automatically — no manual instrumentation is needed in your code.
    {
        let otel_span = provider
            .tracer("ai-library")
            .span_builder("gen_ai.chat")
            .with_attributes(vec![
                KeyValue::new("gen_ai.provider.name", "openai"),
                KeyValue::new("gen_ai.operation.name", "chat"),
                KeyValue::new("gen_ai.request.model", "gpt-4o"),
                KeyValue::new("gen_ai.request.temperature", 0.7_f64),
                KeyValue::new("gen_ai.request.max_tokens", 1024_i64),
            ])
            .start(&provider.tracer("ai-library"));

        // The AI library records response attributes after the API call returns
        use opentelemetry::trace::Span as _;
        let mut otel_span = otel_span;
        otel_span.set_attribute(KeyValue::new("gen_ai.response.model", "gpt-4o-2024-11-20"));
        otel_span.set_attribute(KeyValue::new("gen_ai.usage.input_tokens", 15_i64));
        otel_span.set_attribute(KeyValue::new("gen_ai.usage.output_tokens", 8_i64));

        // Emit prompt/completion events following the OTel GenAI event spec
        otel_span.add_event(
            "gen_ai.content.prompt",
            vec![KeyValue::new(
                "messages",
                r#"[{"role":"user","content":"What is the capital of France?"}]"#,
            )],
        );
        otel_span.add_event(
            "gen_ai.content.completion",
            vec![KeyValue::new(
                "choices",
                r#"[{"message":{"role":"assistant","content":"The capital of France is Paris."}}]"#,
            )],
        );

        // Span ends here — BraintrustTracingLayer captures it and logs to Braintrust
        otel_span.end();
    }

    // Simulate a second AI call (Anthropic Claude)
    {
        let otel_span = provider
            .tracer("ai-library")
            .span_builder("gen_ai.chat")
            .with_attributes(vec![
                KeyValue::new("gen_ai.provider.name", "anthropic"),
                KeyValue::new("gen_ai.operation.name", "chat"),
                KeyValue::new("gen_ai.request.model", "claude-sonnet-4-6"),
            ])
            .start(&provider.tracer("ai-library"));

        use opentelemetry::trace::Span as _;
        let mut otel_span = otel_span;
        otel_span.set_attribute(KeyValue::new("gen_ai.usage.input_tokens", 10_i64));
        otel_span.set_attribute(KeyValue::new("gen_ai.usage.output_tokens", 45_i64));

        otel_span.add_event(
            "gen_ai.content.prompt",
            vec![KeyValue::new("messages", "Explain recursion briefly.")],
        );
        otel_span.add_event(
            "gen_ai.content.completion",
            vec![KeyValue::new(
                "choices",
                "Recursion is a function that calls itself.",
            )],
        );

        otel_span.end();
    }

    // Shut down the OTel provider and flush Braintrust
    global::shutdown_tracer_provider();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    span.flush().await?;
    braintrust.flush().await?;

    println!("Done! Check your Braintrust project for the logged spans.");
    Ok(())
}
