mod context;
mod data;
mod visitor;

pub use context::GenAISpanContext;
use data::GenAISpanData;
use visitor::FieldVisitor;

use crate::logger::BraintrustClient;
use crate::span::{SpanHandle, SpanLog};
use std::collections::HashMap;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// A tracing subscriber layer that captures OpenTelemetry GenAI semantic convention events
/// and automatically logs them to Braintrust.
///
/// # Example
///
/// ```rust,no_run
/// use braintrust_sdk_rust::{BraintrustClient, BraintrustTracingLayer};
/// use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let braintrust = BraintrustClient::builder()
///         .api_url("https://api.braintrust.dev")
///         .skip_login(true)
///         .build()
///         .await?;
///
///     let span = braintrust
///         .span_builder_with_credentials("api-key", "org-id")
///         .project_name("my-project")
///         .build();
///
///     // Install the tracing layer — all instrumented AI SDK calls are now logged!
///     tracing_subscriber::registry()
///         .with(BraintrustTracingLayer::new(span.clone()))
///         .with(tracing_subscriber::fmt::layer())
///         .init();
///
///     // All instrumented AI SDK calls are now automatically logged!
///     Ok(())
/// }
/// ```
pub struct BraintrustTracingLayer {
    span: SpanHandle<BraintrustClient>,
}

impl BraintrustTracingLayer {
    /// Create a new BraintrustTracingLayer that logs to the given span.
    pub fn new(span: SpanHandle<BraintrustClient>) -> Self {
        Self { span }
    }
}

impl<S> Layer<S> for BraintrustTracingLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let metadata = attrs.metadata();

        // Only track GenAI spans (spans that start with gen_ai.)
        if !is_genai_span(metadata) {
            return;
        }

        // Extract fields from span attributes
        let mut visitor = FieldVisitor::new();
        attrs.record(&mut visitor);

        // Create storage for this span and populate with initial request data
        let mut data = GenAISpanData::new();
        for (key, value) in visitor.fields {
            data.record_field(&key, value);
        }

        // Store in span extensions so we can access it later
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(data);
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        // Handle span.record() calls for response fields
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                let mut visitor = FieldVisitor::new();
                values.record(&mut visitor);

                // Record all fields into our data structure
                for (key, value) in visitor.fields {
                    data.record_field(&key, value);
                }
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let target = metadata.target();

        // Handle prompt events
        if target == "gen_ai.content.prompt" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    // Extract prompt content from various field names
                    if let Some(messages) = visitor.fields.get("messages") {
                        if let Some(s) = messages.as_str() {
                            data.add_prompt(s.to_string());
                        } else {
                            data.add_prompt(messages.to_string());
                        }
                    } else if let Some(prompt) = visitor.fields.get("prompt") {
                        if let Some(s) = prompt.as_str() {
                            data.add_prompt(s.to_string());
                        } else {
                            data.add_prompt(prompt.to_string());
                        }
                    }
                }
            }
        }

        // Handle completion events
        if target == "gen_ai.content.completion" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    // Extract completion content from various field names
                    if let Some(choices) = visitor.fields.get("choices") {
                        if let Some(s) = choices.as_str() {
                            data.add_completion(s.to_string());
                        } else {
                            data.add_completion(choices.to_string());
                        }
                    } else if let Some(content) = visitor.fields.get("content") {
                        if let Some(s) = content.as_str() {
                            data.add_completion(s.to_string());
                        } else {
                            data.add_completion(content.to_string());
                        }
                    }
                }
            }
        }

        // Handle streaming chunks
        if target == "gen_ai.content.completion.chunk" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    if let Some(delta) = visitor.fields.get("delta") {
                        if let Some(s) = delta.as_str() {
                            data.add_chunk(s.to_string());
                        } else {
                            data.add_chunk(delta.to_string());
                        }
                    } else if let Some(chunk) = visitor.fields.get("chunk") {
                        if let Some(s) = chunk.as_str() {
                            data.add_chunk(s.to_string());
                        } else {
                            data.add_chunk(chunk.to_string());
                        }
                    }
                }
            }
        }

        // Handle tool definitions
        if target == "gen_ai.tool.definitions" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    if let Some(definitions) = visitor.fields.get("definitions") {
                        data.additional_fields
                            .insert("tool_definitions".to_string(), definitions.clone());
                    }
                }
            }
        }

        // Handle tool calls
        if target == "gen_ai.tool.call" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    let tool_call = serde_json::json!({
                        "call_id": visitor.fields.get("call_id"),
                        "name": visitor.fields.get("name"),
                        "arguments": visitor.fields.get("arguments"),
                    });

                    let tool_calls = data
                        .additional_fields
                        .entry("tool_calls".to_string())
                        .or_insert(serde_json::json!([]));

                    if let Some(arr) = tool_calls.as_array_mut() {
                        arr.push(tool_call);
                    }
                }
            }
        }

        // Handle tool results
        if target == "gen_ai.tool.result" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    let tool_result = serde_json::json!({
                        "tool_call_id": visitor.fields.get("tool_call_id"),
                        "result": visitor.fields.get("result"),
                    });

                    let tool_results = data
                        .additional_fields
                        .entry("tool_results".to_string())
                        .or_insert(serde_json::json!([]));

                    if let Some(arr) = tool_results.as_array_mut() {
                        arr.push(tool_result);
                    }
                }
            }
        }

        // Handle error events
        if target == "gen_ai.error" {
            let mut visitor = FieldVisitor::new();
            event.record(&mut visitor);

            if let Some(span) = ctx.event_span(event) {
                let mut extensions = span.extensions_mut();
                if let Some(data) = extensions.get_mut::<GenAISpanData>() {
                    if let Some(error) = visitor.fields.get("error") {
                        data.additional_fields
                            .insert("error_details".to_string(), error.clone());
                    }
                }
            }
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, S>) {
        // When span closes, send accumulated data to Braintrust
        if let Some(span) = ctx.span(&id) {
            let extensions = span.extensions();
            if let Some(data) = extensions.get::<GenAISpanData>() {
                // Skip empty spans (non-GenAI spans that passed through)
                if data.is_empty() {
                    return;
                }

                let data = data.clone();
                let braintrust_span = self.span.clone();

                // Log asynchronously to avoid blocking the tracing pipeline.
                // Check for a runtime first to avoid a panic if none is installed.
                let Ok(handle) = tokio::runtime::Handle::try_current() else {
                    tracing::warn!(
                        "BraintrustTracingLayer: no Tokio runtime available, span will not be logged"
                    );
                    return;
                };
                handle.spawn(async move {
                    // Build the span log
                    let name = match (
                        data.provider_name.as_deref(),
                        data.operation_name.as_deref(),
                    ) {
                        (Some(provider), Some(op)) => format!("{}.{}", provider, op),
                        (Some(provider), None) => provider.to_string(),
                        (None, Some(op)) => op.to_string(),
                        (None, None) => "genai.call".to_string(),
                    };
                    let mut log_builder = SpanLog::builder().name(name);

                    // Add input (prompt)
                    if !data.prompt.is_empty() {
                        let input = if data.prompt.len() == 1 {
                            // Single prompt - use as-is
                            serde_json::json!(data.prompt[0])
                        } else {
                            // Multiple prompts - use array
                            serde_json::json!(data.prompt)
                        };
                        log_builder = log_builder.input(input);
                    }

                    // Add output (completion or chunks)
                    if !data.completion.is_empty() {
                        let output = if data.completion.len() == 1 {
                            serde_json::json!(data.completion[0])
                        } else {
                            serde_json::json!(data.completion)
                        };
                        log_builder = log_builder.output(output);
                    } else if !data.chunks.is_empty() {
                        // For streaming, combine chunks
                        log_builder = log_builder.output(serde_json::json!({
                            "chunks": data.chunks
                        }));
                    }

                    // Add metadata
                    let mut metadata = serde_json::Map::new();
                    if let Some(provider) = data.provider_name {
                        metadata.insert("provider".to_string(), serde_json::json!(provider));
                    }
                    if let Some(model) = data.model {
                        metadata.insert("model".to_string(), serde_json::json!(model));
                    }
                    if let Some(temperature) = data.temperature {
                        metadata.insert("temperature".to_string(), serde_json::json!(temperature));
                    }
                    if let Some(max_tokens) = data.max_tokens {
                        metadata.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
                    }
                    if let Some(top_p) = data.top_p {
                        metadata.insert("top_p".to_string(), serde_json::json!(top_p));
                    }
                    if let Some(choice_count) = data.choice_count {
                        metadata
                            .insert("choice_count".to_string(), serde_json::json!(choice_count));
                    }
                    if !data.stop_sequences.is_empty() {
                        metadata.insert(
                            "stop_sequences".to_string(),
                            serde_json::json!(data.stop_sequences),
                        );
                    }
                    if let Some(seed) = data.seed {
                        metadata.insert("seed".to_string(), serde_json::json!(seed));
                    }
                    if let Some(logprobs) = data.logprobs {
                        metadata.insert("logprobs".to_string(), serde_json::json!(logprobs));
                    }
                    if let Some(ref logit_bias) = data.logit_bias {
                        metadata.insert("logit_bias".to_string(), serde_json::json!(logit_bias));
                    }
                    if let Some(ref format) = data.response_format {
                        metadata.insert("response_format".to_string(), serde_json::json!(format));
                    }
                    if let Some(ref effort) = data.reasoning_effort {
                        metadata.insert("reasoning_effort".to_string(), serde_json::json!(effort));
                    }
                    if let Some(penalty) = data.presence_penalty {
                        metadata.insert("presence_penalty".to_string(), serde_json::json!(penalty));
                    }
                    if let Some(penalty) = data.frequency_penalty {
                        metadata
                            .insert("frequency_penalty".to_string(), serde_json::json!(penalty));
                    }
                    if let Some(response_id) = data.response_id {
                        metadata.insert("response_id".to_string(), serde_json::json!(response_id));
                    }
                    if let Some(response_model) = data.response_model {
                        metadata.insert(
                            "response_model".to_string(),
                            serde_json::json!(response_model),
                        );
                    }
                    if !data.finish_reasons.is_empty() {
                        metadata.insert(
                            "finish_reasons".to_string(),
                            serde_json::json!(data.finish_reasons),
                        );
                    }
                    if let Some(ref error_type) = data.error_type {
                        metadata.insert("error_type".to_string(), serde_json::json!(error_type));
                    }
                    if let Some(ref error_code) = data.error_code {
                        metadata.insert("error_code".to_string(), serde_json::json!(error_code));
                    }
                    if let Some(ref address) = data.server_address {
                        metadata.insert("server_address".to_string(), serde_json::json!(address));
                    }
                    if let Some(port) = data.server_port {
                        metadata.insert("server_port".to_string(), serde_json::json!(port));
                    }
                    if let Some(ref agent_id) = data.agent_id {
                        metadata.insert("agent_id".to_string(), serde_json::json!(agent_id));
                    }
                    if let Some(ref agent_name) = data.agent_name {
                        metadata.insert("agent_name".to_string(), serde_json::json!(agent_name));
                    }
                    if let Some(ref workflow_id) = data.workflow_id {
                        metadata.insert("workflow_id".to_string(), serde_json::json!(workflow_id));
                    }
                    if let Some(step) = data.workflow_step {
                        metadata.insert("workflow_step".to_string(), serde_json::json!(step));
                    }
                    if let Some(dims) = data.embedding_dimensions {
                        metadata
                            .insert("embedding_dimensions".to_string(), serde_json::json!(dims));
                    }
                    if let Some(ref format) = data.embedding_format {
                        metadata.insert("embedding_format".to_string(), serde_json::json!(format));
                    }
                    if let Some(ref size) = data.image_size {
                        metadata.insert("image_size".to_string(), serde_json::json!(size));
                    }
                    if let Some(ref quality) = data.image_quality {
                        metadata.insert("image_quality".to_string(), serde_json::json!(quality));
                    }
                    if let Some(ref style) = data.image_style {
                        metadata.insert("image_style".to_string(), serde_json::json!(style));
                    }

                    // Add any additional fields captured
                    for (key, value) in data.additional_fields {
                        metadata.insert(key, value);
                    }

                    if !metadata.is_empty() {
                        log_builder = log_builder.metadata(metadata);
                    }

                    // Add metrics (usage) - using spec-compliant names
                    let mut metrics = HashMap::new();
                    if let Some(tokens) = data.input_tokens {
                        metrics.insert("input_tokens".to_string(), tokens as f64);
                    }
                    if let Some(tokens) = data.output_tokens {
                        metrics.insert("output_tokens".to_string(), tokens as f64);
                    }
                    if let Some(tokens) = data.cache_creation_input_tokens {
                        metrics.insert("cache_creation_input_tokens".to_string(), tokens as f64);
                    }
                    if let Some(tokens) = data.cache_read_input_tokens {
                        metrics.insert("cache_read_input_tokens".to_string(), tokens as f64);
                    }
                    if let Some(tokens) = data.reasoning_input_tokens {
                        metrics.insert("reasoning_input_tokens".to_string(), tokens as f64);
                    }
                    if let Some(tokens) = data.reasoning_output_tokens {
                        metrics.insert("reasoning_output_tokens".to_string(), tokens as f64);
                    }
                    // Calculate and add total_tokens for convenience (derived field)
                    if let (Some(input), Some(output)) = (data.input_tokens, data.output_tokens) {
                        metrics.insert("total_tokens".to_string(), (input + output) as f64);
                    }
                    if !metrics.is_empty() {
                        log_builder = log_builder.metrics(metrics);
                    }

                    // Send to Braintrust
                    match log_builder.build() {
                        Ok(log) => braintrust_span.log(log).await,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "BraintrustTracingLayer: failed to build span log"
                            );
                        }
                    }
                });
            }
        }
    }
}

/// Check if a span or event is a GenAI span based on OpenTelemetry conventions
fn is_genai_span(metadata: &Metadata) -> bool {
    metadata.name().starts_with("gen_ai.") || metadata.target().starts_with("gen_ai.")
}

// Note: Unit tests for is_genai_span are covered by integration tests in tests/tracing_layer_tests.rs
