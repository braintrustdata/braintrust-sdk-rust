//! OpenTelemetry SDK Integration Tests
//!
//! These tests use the official OpenTelemetry SDK to verify that our tracing
//! instrumentation correctly integrates with OpenTelemetry and produces
//! compliant spans that can be exported.

use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_sdk::export::trace::SpanData;
use opentelemetry_sdk::testing::trace::InMemorySpanExporter;
use opentelemetry_sdk::trace::{Sampler, TracerProvider};
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

/// Setup OpenTelemetry with in-memory exporter and tracing bridge
fn setup_otel_test() -> (Arc<InMemorySpanExporter>, tracing::subscriber::DefaultGuard) {
    // Create in-memory exporter to capture spans
    let exporter = InMemorySpanExporter::default();
    let exporter_arc = Arc::new(exporter);

    // Create tracer provider with the exporter
    let provider = TracerProvider::builder()
        .with_simple_exporter(exporter_arc.as_ref().clone())
        .with_sampler(Sampler::AlwaysOn)
        .build();

    // Set as global provider
    global::set_tracer_provider(provider.clone());

    // Create tracing-opentelemetry layer
    let tracer = provider.tracer("test");
    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);

    // Set up tracing subscriber
    let subscriber = Registry::default().with(telemetry);
    let guard = tracing::subscriber::set_default(subscriber);

    (exporter_arc, guard)
}

/// Get all finished spans from the exporter
fn get_finished_spans(exporter: &InMemorySpanExporter) -> Vec<SpanData> {
    exporter.get_finished_spans().unwrap()
}

#[test]
fn test_otel_span_is_created() {
    let (exporter, _guard) = setup_otel_test();

    // Create a span using tracing
    let span = tracing::info_span!(
        "gen_ai.chat",
        otel.kind = "client",
        gen_ai.provider.name = "openai",
    );
    let _enter = span.enter();

    // Force flush
    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    // Verify span was exported to OpenTelemetry
    let spans = get_finished_spans(&exporter);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "gen_ai.chat");
}

#[test]
fn test_otel_attributes_with_semantic_conventions() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        otel.kind = "client",
        gen_ai.provider.name = "openai",
        gen_ai.operation.name = "chat",
        gen_ai.request.model = "gpt-4",
        gen_ai.request.temperature = 0.7,
        gen_ai.request.max_tokens = 1000,
    );
    let _enter = span.enter();

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    assert_eq!(spans.len(), 1);

    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    // Debug: Print all attributes
    eprintln!("Captured attributes:");
    for attr in &attrs {
        eprintln!("  {} = {:?}", attr.key.as_str(), attr.value);
    }

    // Verify required attributes exist
    // Note: otel.kind might be captured as span kind, not attribute
    assert!(
        attrs.iter().any(|kv| kv.key.as_str() == "otel.kind")
            || span_data.span_kind == opentelemetry::trace::SpanKind::Client,
        "Span should have otel.kind attribute or SpanKind::Client"
    );
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.provider.name" && kv.value.as_str() == "openai"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.operation.name" && kv.value.as_str() == "chat"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.model" && kv.value.as_str() == "gpt-4"));
}

#[test]
fn test_otel_usage_tokens_recorded() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
    );
    let _enter = span.enter();

    // Record usage (simulating what the instrumentation does)
    span.record("gen_ai.usage.input_tokens", 100);
    span.record("gen_ai.usage.output_tokens", 50);

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.usage.input_tokens" && kv.value.as_str() == "100"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.usage.output_tokens" && kv.value.as_str() == "50"));
}

#[test]
fn test_otel_reasoning_tokens() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.usage.reasoning.input_tokens = tracing::field::Empty,
        gen_ai.usage.reasoning.output_tokens = tracing::field::Empty,
    );
    let _enter = span.enter();

    span.record("gen_ai.usage.reasoning.output_tokens", 1500);

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    assert!(attrs.iter().any(
        |kv| kv.key.as_str() == "gen_ai.usage.reasoning.output_tokens"
            && kv.value.as_str() == "1500"
    ));
}

#[test]
fn test_otel_cache_tokens() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
        gen_ai.usage.cache_creation.input_tokens = tracing::field::Empty,
    );
    let _enter = span.enter();

    span.record("gen_ai.usage.cache_read.input_tokens", 200);
    span.record("gen_ai.usage.cache_creation.input_tokens", 300);

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    assert!(attrs.iter().any(
        |kv| kv.key.as_str() == "gen_ai.usage.cache_read.input_tokens"
            && kv.value.as_str() == "200"
    ));
    assert!(attrs.iter().any(
        |kv| kv.key.as_str() == "gen_ai.usage.cache_creation.input_tokens"
            && kv.value.as_str() == "300"
    ));
}

#[test]
fn test_otel_error_attributes() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        error.type = tracing::field::Empty,
        gen_ai.response.error_code = tracing::field::Empty,
    );
    let _enter = span.enter();

    span.record("error.type", "rate_limit");
    span.record("gen_ai.response.error_code", "429");

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "error.type" && kv.value.as_str() == "rate_limit"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.response.error_code" && kv.value.as_str() == "429"));
}

#[test]
fn test_otel_server_attributes() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        server.address = tracing::field::Empty,
        server.port = tracing::field::Empty,
    );
    let _enter = span.enter();

    span.record("server.address", "api.openai.com");
    span.record("server.port", 443u16);

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let span_data = &spans[0];
    let attrs: Vec<KeyValue> = span_data.attributes.clone().into_iter().collect();

    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "server.address" && kv.value.as_str() == "api.openai.com"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "server.port" && kv.value.as_str() == "443"));
}

#[test]
fn test_otel_embeddings_operation() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.embeddings",
        otel.kind = "client",
        gen_ai.provider.name = "openai",
        gen_ai.operation.name = "embeddings",
        gen_ai.request.model = "text-embedding-3-small",
        gen_ai.request.embedding_dimensions = 1536,
    );
    let _enter = span.enter();

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].name, "gen_ai.embeddings");

    let attrs: Vec<KeyValue> = spans[0].attributes.clone().into_iter().collect();
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.operation.name" && kv.value.as_str() == "embeddings"));
    assert!(attrs.iter().any(
        |kv| kv.key.as_str() == "gen_ai.request.embedding_dimensions"
            && kv.value.as_str() == "1536"
    ));
}

#[test]
fn test_otel_image_generation_operation() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.image_generation",
        otel.kind = "client",
        gen_ai.provider.name = "openai",
        gen_ai.operation.name = "image_generation",
        gen_ai.request.image_size = "1024x1024",
        gen_ai.request.image_quality = "hd",
    );
    let _enter = span.enter();

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let attrs: Vec<KeyValue> = spans[0].attributes.clone().into_iter().collect();

    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.operation.name"
            && kv.value.as_str() == "image_generation"));
    assert!(attrs.iter().any(
        |kv| kv.key.as_str() == "gen_ai.request.image_size" && kv.value.as_str() == "1024x1024"
    ));
}

#[test]
fn test_otel_span_events() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!("gen_ai.chat");
    let _enter = span.enter();

    // Create events (these should be captured as OpenTelemetry span events)
    tracing::info!(
        target: "gen_ai.content.prompt",
        content = "What is 2+2?",
        "prompt"
    );

    tracing::info!(
        target: "gen_ai.content.completion",
        content = "4",
        "completion"
    );

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    assert_eq!(spans.len(), 1);

    // Verify events were captured
    let events = &spans[0].events;
    assert!(events.len() >= 2, "Should have at least 2 events");

    // Note: Event targets and attributes are captured by OpenTelemetry
    assert!(events.iter().any(|e| e.name == "prompt"));
    assert!(events.iter().any(|e| e.name == "completion"));
}

#[test]
fn test_otel_request_parameters() {
    let (exporter, _guard) = setup_otel_test();

    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.request.temperature = 0.8,
        gen_ai.request.max_tokens = 2000,
        gen_ai.request.top_p = 0.95,
        gen_ai.request.frequency_penalty = 0.5,
        gen_ai.request.presence_penalty = 0.3,
        gen_ai.request.seed = 42,
    );
    let _enter = span.enter();

    drop(_enter);
    drop(span);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    let attrs: Vec<KeyValue> = spans[0].attributes.clone().into_iter().collect();

    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.temperature"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.max_tokens"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.top_p"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.frequency_penalty"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.presence_penalty"));
    assert!(attrs
        .iter()
        .any(|kv| kv.key.as_str() == "gen_ai.request.seed"));
}

#[test]
fn test_otel_span_hierarchy() {
    let (exporter, _guard) = setup_otel_test();

    // Create parent span
    let parent = tracing::info_span!("gen_ai.chat");
    let _parent_enter = parent.enter();

    // Create child span (simulating nested operations)
    let child = tracing::info_span!("gen_ai.tool.call");
    let _child_enter = child.enter();

    drop(_child_enter);
    drop(child);
    drop(_parent_enter);
    drop(parent);
    global::shutdown_tracer_provider();

    let spans = get_finished_spans(&exporter);
    assert_eq!(spans.len(), 2, "Should have parent and child spans");

    // Find parent and child
    let parent_span = spans.iter().find(|s| s.name == "gen_ai.chat").unwrap();
    let child_span = spans.iter().find(|s| s.name == "gen_ai.tool.call").unwrap();

    // Verify parent-child relationship
    assert_eq!(
        child_span.parent_span_id,
        parent_span.span_context.span_id(),
        "Child span should reference parent span ID"
    );
}
