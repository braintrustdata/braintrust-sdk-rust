# Braintrust SDK Rust - Implementation Plan

## Overview

Create a production-ready Rust SDK for Braintrust with idiomatic Rust patterns, focusing on the tracer/logger component for hierarchical tracing of LLM applications.

## Architecture Summary

The Braintrust SDK is a **hierarchical tracing system for LLM applications** that logs structured events to the Braintrust platform for evaluation and monitoring.

### Core Components

1. **Span System** - Units of work with unique IDs forming a tree hierarchy
2. **Logger/Tracer** - Top-level entry point for creating root spans
3. **BraintrustState** - Global connection manager handling auth and metadata
4. **Event Queue** - Batches and sends events to backend efficiently
5. **SpanComponentsV3** - Binary span ID encoding/decoding

### Key Behaviors

- **Event Merging**: First log replaces data, subsequent logs merge field-by-field
- **Parent Resolution**: Async context automatically links parent-child relationships
- **Event Batching**: 100 events or 6MB per batch, sent in parallel with retries
- **Binary Encoding**: Variable-length UUID format for efficient span IDs

## Phase 1: Tracer Foundation (Initial Focus)

### 1.1 Project Setup

- [ ] Create Cargo workspace with `braintrust-sdk` crate
- [ ] Set up dependencies:
  - `tokio` for async runtime
  - `serde` and `serde_json` for serialization
  - `reqwest` for HTTP client
  - `uuid` for span IDs
  - `chrono` for timestamps
  - `thiserror` for error handling
  - `tokio::task_local` for async context
- [ ] Configure linting (clippy) and formatting (rustfmt)
- [ ] Set up CI/CD pipeline

### 1.2 Core Data Types

Define the fundamental types for the tracer:

```rust
// Span ID components (V3 binary encoding)
pub struct SpanId {
    row_ids: SpanRowIds,
    span_id: Uuid,
    root_span_id: Uuid,
}

pub struct SpanRowIds {
    project_id: Option<Uuid>,
    experiment_id: Option<Uuid>,
    log_id: ObjectType<ProjectId | ExperimentId>,
}

// Span types
pub enum SpanType {
    Task,
    Llm,
    Function,
    Eval,
    Score,
    Tool,
}

// Event data structures
pub struct SpanEvent {
    id: String,
    span_id: String,
    root_span_id: String,
    project_id: Option<String>,
    experiment_id: Option<String>,
    input: Option<Value>,
    output: Option<Value>,
    expected: Option<Value>,
    metadata: Option<Value>,
    metrics: Option<Value>,
    context: Option<SpanContext>,
    created: Option<DateTime<Utc>>,
    // ... other fields
}
```

### 1.3 Span Trait and Implementation

The core abstraction for tracing:

```rust
pub trait Span: Send + Sync {
    /// Log data to this span (merges with existing data)
    fn log(&self, event: SpanEvent);

    /// Start a child span
    fn start_span(&self, options: StartSpanOptions) -> Arc<dyn Span>;

    /// Execute a function and log its result
    async fn traced<F, T>(&self, func: F, options: StartSpanOptions) -> T
    where
        F: FnOnce() -> T + Send;

    /// End the span and flush events
    async fn end(&self) -> Result<(), BraintrustError>;

    /// Get span ID
    fn id(&self) -> &str;

    /// Export span data for inspection
    fn export(&self) -> SpanEvent;
}
```

### 1.4 Logger/Tracer Entry Point

Top-level API for creating root spans:

```rust
pub struct Logger {
    state: Arc<BraintrustState>,
    span_attributes: SpanAttributes,
}

impl Logger {
    pub fn new(options: LoggerOptions) -> Self;

    pub fn start_span(&self, options: StartSpanOptions) -> Arc<dyn Span>;

    pub async fn traced<F, T>(&self, func: F, options: StartSpanOptions) -> T
    where
        F: FnOnce() -> T + Send;

    pub async fn flush(&self) -> Result<(), BraintrustError>;
}

// Convenience function for default logger
pub fn init_logger(options: LoggerOptions) -> Logger;
```

### 1.5 Async Context Management

Use task-local storage to track current span:

```rust
tokio::task_local! {
    static CURRENT_SPAN: Arc<dyn Span>;
}

/// Get the current span from async context
pub fn current_span() -> Option<Arc<dyn Span>>;

/// Run a function with a span as the current context
pub async fn with_span<F, T>(span: Arc<dyn Span>, f: F) -> T
where
    F: Future<Output = T>;
```

### 1.6 Event Queue and Batching

Background task for efficient event transmission:

```rust
pub struct EventQueue {
    queue: mpsc::UnboundedSender<SpanEvent>,
    batch_size: usize,  // Default 100 events
    max_batch_bytes: usize,  // Default 6MB
}

impl EventQueue {
    pub fn new(client: HttpClient) -> Self;

    /// Enqueue an event for sending
    pub fn enqueue(&self, event: SpanEvent);

    /// Start background processing task
    pub async fn start(&self);

    /// Flush all pending events
    pub async fn flush(&self) -> Result<(), BraintrustError>;
}
```

### 1.7 HTTP Client and Authentication

Handle communication with Braintrust backend:

```rust
pub struct HttpClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl HttpClient {
    pub fn new(api_key: String, base_url: Option<String>) -> Self;

    pub async fn send_batch(&self, events: Vec<SpanEvent>) -> Result<(), BraintrustError>;

    pub async fn login(&self) -> Result<LoginResponse, BraintrustError>;
}
```

### 1.8 BraintrustState - Global State Manager

Centralized state management:

```rust
pub struct BraintrustState {
    client: HttpClient,
    queue: EventQueue,
    org_id: Option<String>,
    org_name: Option<String>,
    login_token: Option<String>,
    project_info: HashMap<String, ProjectInfo>,
}

impl BraintrustState {
    pub fn new(options: BraintrustOptions) -> Arc<Self>;

    pub async fn login(&self) -> Result<(), BraintrustError>;

    pub fn log_event(&self, event: SpanEvent);

    pub async fn flush(&self) -> Result<(), BraintrustError>;
}
```

## Phase 2: Advanced Tracer Features

### 2.1 Span Types and Attributes

- [ ] Implement specialized span types (LLM, Function, Task, etc.)
- [ ] Add type-specific attributes and validation
- [ ] Implement span type coercion

### 2.2 Event Merging Logic

- [ ] Implement field-by-field merge algorithm
- [ ] Handle special fields (metrics, metadata, context)
- [ ] Array and object merging strategies

### 2.3 SpanComponentsV3 Binary Encoding

- [ ] Implement binary span ID encoding
- [ ] Support variable-length UUID encoding
- [ ] Backward compatibility with V2/V1

### 2.4 Span Utilities

- [ ] `with_attributes()` - Add attributes to spans
- [ ] `set_tags()` - Set tags on spans
- [ ] `add_links()` - Link related spans
- [ ] `record_exception()` - Log errors

## Phase 3: Additional Components

### 3.1 Dataset Management

- [ ] Dataset loading and creation
- [ ] Dataset versioning
- [ ] Record insertion and updates

### 3.2 Experiment Management

- [ ] Experiment creation and tracking
- [ ] Summarization and metrics
- [ ] Comparison utilities

### 3.3 Evaluation Framework

- [ ] `Eval` framework for running evaluations
- [ ] Scorer trait and built-in scorers
- [ ] Result aggregation

### 3.4 Framework Integrations

- [ ] OpenAI client wrapper
- [ ] Anthropic client wrapper
- [ ] Generic HTTP interceptor

## Implementation Guidelines

### Rust Idioms

1. **Error Handling**: Use `Result<T, BraintrustError>` with `thiserror`
2. **Async**: Use `tokio` runtime with `async/await`
3. **Ownership**: Use `Arc` for shared state, avoid `Mutex` where possible
4. **Builder Pattern**: Use for complex configuration structs
5. **Type Safety**: Leverage Rust's type system for API safety
6. **Zero-Cost Abstractions**: Prefer traits over dynamic dispatch where possible

### API Design Principles

1. **Ergonomics**: Provide convenient macros and helper functions
2. **Type Safety**: Prevent invalid states at compile time
3. **Performance**: Minimize allocations and use efficient data structures
4. **Async-First**: All I/O operations should be async
5. **Error Context**: Use `anyhow` for rich error context in examples

### Testing Strategy

1. **Unit Tests**: Test individual components in isolation
2. **Integration Tests**: Test end-to-end flows with mock backend
3. **Property Tests**: Use `proptest` for invariant checking
4. **Benchmarks**: Use `criterion` for performance regression testing

## Environment Variables

Support standard Braintrust environment variables:

- `BRAINTRUST_API_KEY` - API key for authentication
- `BRAINTRUST_API_URL` - Backend API URL (default: https://api.braintrustdata.com)
- `BRAINTRUST_APP_URL` - Web app URL for viewing traces
- `BRAINTRUST_ORG_ID` - Organization ID
- `BRAINTRUST_ORG_NAME` - Organization name
- `BRAINTRUST_PROJECT_ID` - Default project ID
- `BRAINTRUST_PROJECT_NAME` - Default project name

## Documentation

- [ ] README with quickstart guide
- [ ] API documentation with rustdoc
- [ ] Examples directory with common use cases
- [ ] Migration guide from JS/Python SDKs
- [ ] Architecture documentation

## Success Criteria

1. **Functional Parity**: Core tracer matches JS/Python SDK capabilities
2. **Performance**: Lower latency and memory usage than JS SDK
3. **Ergonomics**: Idiomatic Rust API that feels natural
4. **Reliability**: Comprehensive tests with >90% coverage
5. **Documentation**: Clear docs with examples for common use cases

## References

- TypeScript SDK: `/Users/kenjiang/Development/braintrust-sdk/js/src/logger.ts`
- Python SDK: `/Users/kenjiang/Development/braintrust-sdk/py/src/braintrust/logger.py`
- Span ID encoding: `/Users/kenjiang/Development/braintrust-sdk/js/util/span_identifier_v3.ts`
- Event queue: `/Users/kenjiang/Development/braintrust-sdk/js/src/queue.ts`
