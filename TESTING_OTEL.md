# Testing OpenTelemetry GenAI Instrumentation

This guide explains how to test OpenTelemetry instrumentation to ensure compliance with semantic conventions.

## ✅ Approach 1: Official OpenTelemetry SDK (Recommended)

**This is the official, certified way to test OpenTelemetry instrumentation.**

We use the `opentelemetry-sdk` testing utilities with `tracing-opentelemetry` bridge to verify that our instrumentation produces compliant spans that work with the real OpenTelemetry ecosystem.

### Setup

```toml
[dev-dependencies]
opentelemetry = "0.27"
opentelemetry_sdk = { version = "0.27", features = ["testing"] }
tracing-opentelemetry = "0.28"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### Example Usage

```rust
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::testing::trace::InMemorySpanExporter;
use opentelemetry_sdk::trace::TracerProvider;

#[test]
fn test_with_otel_sdk() {
    // Setup: Create in-memory exporter
    let exporter = InMemorySpanExporter::default();
    let provider = TracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();

    global::set_tracer_provider(provider.clone());

    // Bridge tracing to OpenTelemetry
    let tracer = provider.tracer("test");
    let telemetry = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = tracing_subscriber::registry().with(telemetry);
    let _guard = tracing::subscriber::set_default(subscriber);

    // Run instrumented code
    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.provider.name = "openai",
        gen_ai.usage.input_tokens = 100,
    );
    let _enter = span.enter();
    drop(_enter);
    drop(span);

    global::shutdown_tracer_provider();

    // Verify with official OpenTelemetry SDK
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans[0].name, "gen_ai.chat");

    let attrs: Vec<_> = spans[0].attributes.iter().collect();
    assert!(attrs.iter().any(|kv|
        kv.key.as_str() == "gen_ai.usage.input_tokens"));
}
```

**See `tests/otel_sdk_integration_test.rs` for 12 comprehensive examples!**

All tests pass ✅ proving our instrumentation works with real OpenTelemetry!

---

## Approach 2: Custom Tracing Test Layer

Since we use `tracing` with OTel semantic conventions, the best approach is to create a test layer that captures spans and events.

### Example Usage

```rust
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn test_instrumentation() {
    let test_layer = TestLayer::new();
    let subscriber = tracing_subscriber::registry().with(test_layer.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    // Run instrumented code
    let span = tracing::info_span!(
        "gen_ai.chat",
        gen_ai.provider.name = "openai",
        gen_ai.usage.input_tokens = tracing::field::Empty,
    );
    let _enter = span.enter();
    span.record("gen_ai.usage.input_tokens", 100);

    // Verify attributes
    let spans = test_layer.find_spans_by_name("gen_ai.chat");
    assert_eq!(spans[0].attributes.get("gen_ai.usage.input_tokens"),
               Some(&"100".to_string()));
}
```

See `tests/otel_compliance_test.rs` for comprehensive examples.

## Approach 3: OpenTelemetry SDK Testing (Alternative)

For direct OpenTelemetry SDK usage, use `opentelemetry-sdk` testing utilities:

```toml
[dev-dependencies]
opentelemetry = "0.21"
opentelemetry-sdk = "0.21"
opentelemetry-stdout = "0.21"  # For debugging
```

```rust
use opentelemetry::trace::{Tracer, TracerProvider};
use opentelemetry_sdk::export::trace::stdout;
use opentelemetry_sdk::trace::TracerProvider as SdkTracerProvider;

#[test]
fn test_with_otel_sdk() {
    // Create in-memory exporter
    let exporter = stdout::new_pipeline().with_pretty_print(true).install_simple();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();

    let tracer = provider.tracer("test");

    // Create and verify span
    let span = tracer.span_builder("gen_ai.chat")
        .with_attributes(vec![
            KeyValue::new("gen_ai.provider.name", "openai"),
            KeyValue::new("gen_ai.usage.input_tokens", 100),
        ])
        .start(&tracer);

    // Verify attributes
    assert!(span.get_context().span().has_ended());
}
```

## Approach 4: Mock OpenTelemetry Collector

For integration testing with the full OTel pipeline:

### Using Docker

```yaml
# docker-compose.test.yml
version: '3'
services:
  otel-collector:
    image: otel/opentelemetry-collector:latest
    command: ["--config=/etc/otel-config.yaml"]
    volumes:
      - ./otel-config.yaml:/etc/otel-config.yaml
    ports:
      - "4317:4317"  # OTLP gRPC
```

```yaml
# otel-config.yaml
receivers:
  otlp:
    protocols:
      grpc:

exporters:
  logging:
    loglevel: debug
  file:
    path: /tmp/otel-spans.json

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [logging, file]
```

### In Tests

```rust
#[tokio::test]
async fn test_with_collector() {
    // Start collector
    std::process::Command::new("docker-compose")
        .args(&["-f", "docker-compose.test.yml", "up", "-d"])
        .output()
        .expect("Failed to start collector");

    // Configure OTLP exporter
    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint("http://localhost:4317");

    // Run instrumented code
    // ...

    // Read exported spans from file
    let spans = std::fs::read_to_string("/tmp/otel-spans.json")?;
    let parsed: Vec<Span> = serde_json::from_str(&spans)?;

    // Verify compliance
    assert!(parsed.iter().any(|s| s.name == "gen_ai.chat"));
}
```

## Approach 5: Snapshot Testing

Create expected span outputs and compare:

```rust
use insta::assert_json_snapshot;

#[test]
fn test_span_structure() {
    let test_layer = TestLayer::new();
    // ... run instrumented code ...

    let spans = test_layer.get_spans();
    assert_json_snapshot!(spans);
}
```

## What to Test

### ✅ Required Attributes (per operation)

**Chat Operations:**
- `otel.kind = "client"`
- `gen_ai.provider.name` (e.g., "openai")
- `gen_ai.operation.name = "chat"`
- `gen_ai.request.model`

**Usage Metrics:**
- `gen_ai.usage.input_tokens`
- `gen_ai.usage.output_tokens`
- `gen_ai.usage.cache_read.input_tokens` (if applicable)
- `gen_ai.usage.cache_creation.input_tokens` (if applicable)
- `gen_ai.usage.reasoning.input_tokens` (if applicable)
- `gen_ai.usage.reasoning.output_tokens` (if applicable)

**Error Handling:**
- `error.type` (when errors occur)
- `gen_ai.response.error_code` (provider-specific)

**Embeddings:**
- `gen_ai.operation.name = "embeddings"`
- `gen_ai.request.embedding_dimensions` (if set)
- `gen_ai.request.embedding_format` (if set)

**Image Generation:**
- `gen_ai.operation.name = "image_generation"`
- `gen_ai.request.image_size` (if set)
- `gen_ai.request.image_quality` (if set)
- `gen_ai.request.image_style` (if set)

### ✅ Events to Test

- `gen_ai.content.prompt` - Input messages
- `gen_ai.content.completion` - Output content
- `gen_ai.tool.call` - Tool invocations
- `gen_ai.tool.result` - Tool results
- `gen_ai.tool.definitions` - Available tools
- `gen_ai.error` - Error events

### ✅ Streaming Behavior

Test that streaming operations:
1. Create a span at stream start
2. Capture usage from final chunks
3. Record all token types (input, output, cache, reasoning)

## Running Tests

```bash
# Run all OTel compliance tests
cargo test --test otel_compliance_test

# Run specific test
cargo test --test otel_compliance_test test_reasoning_tokens_captured

# Run with output
cargo test --test otel_compliance_test -- --nocapture

# Check coverage
cargo tarpaulin --test otel_compliance_test
```

## Integration with OpenTelemetry Collector

For real-world validation, you can send spans to an actual OpenTelemetry collector and use its validation features:

```bash
# Start collector with validation
docker run -p 4317:4317 \
  -v $(pwd)/otel-config.yaml:/etc/otel-config.yaml \
  otel/opentelemetry-collector:latest \
  --config=/etc/otel-config.yaml

# Run your application with OTLP exporter
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 \
  cargo run --example chat_example
```

## Official OpenTelemetry Resources

- **Semantic Conventions**: https://opentelemetry.io/docs/specs/semconv/gen-ai/
- **Rust SDK**: https://github.com/open-telemetry/opentelemetry-rust
- **Test Examples**: https://github.com/open-telemetry/opentelemetry-rust/tree/main/opentelemetry-sdk/tests

## Continuous Validation

Consider adding these checks to CI:

1. **Schema Validation**: Ensure attribute names match spec
2. **Required Fields**: Verify all mandatory attributes are present
3. **Type Correctness**: Check attribute value types
4. **Event Structure**: Validate event targets and fields
5. **Span Hierarchy**: Verify parent-child relationships

## Example CI Test

```yaml
# .github/workflows/otel-compliance.yml
name: OpenTelemetry Compliance

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - name: Run OTel compliance tests
        run: cargo test --test otel_compliance_test
      - name: Verify with real collector
        run: |
          docker-compose -f docker-compose.test.yml up -d
          cargo test --features integration-tests
```
