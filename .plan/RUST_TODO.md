# Braintrust SDK Rust - TODO

## Current Focus: Phase 1 - Tracer Foundation

### Project Setup

- [ ] Create Cargo workspace structure
- [ ] Initialize `braintrust-sdk` crate
- [ ] Add core dependencies to Cargo.toml
  - [ ] tokio (async runtime)
  - [ ] serde, serde_json (serialization)
  - [ ] reqwest (HTTP client)
  - [ ] uuid (span IDs)
  - [ ] chrono (timestamps)
  - [ ] thiserror (error handling)
- [ ] Configure clippy and rustfmt
- [ ] Create basic README.md
- [ ] Set up directory structure (src/lib.rs, src/span.rs, src/logger.rs, etc.)

### Core Data Types

- [ ] Define `SpanId` struct with V3 binary encoding support
- [ ] Define `SpanRowIds` struct
- [ ] Define `SpanType` enum (Task, Llm, Function, Eval, Score, Tool)
- [ ] Define `SpanEvent` struct with all fields
- [ ] Define `SpanContext` struct
- [ ] Define `StartSpanOptions` builder
- [ ] Define `LoggerOptions` builder
- [ ] Implement serialization/deserialization for all types

### Error Handling

- [ ] Create `BraintrustError` enum with thiserror
- [ ] Add error variants (NetworkError, AuthError, SerializationError, etc.)
- [ ] Implement error conversions from underlying libraries

### Span Trait and Implementation

- [ ] Define `Span` trait with core methods
- [ ] Implement `SpanImpl` struct
- [ ] Implement `log()` method with event merging
- [ ] Implement `start_span()` method
- [ ] Implement `traced()` method for automatic logging
- [ ] Implement `end()` method
- [ ] Implement `id()` getter
- [ ] Implement `export()` method
- [ ] Add internal event storage with Arc<RwLock<SpanEvent>>
- [ ] Implement parent-child relationship tracking

### Logger/Tracer Entry Point

- [ ] Implement `Logger` struct
- [ ] Implement `Logger::new()` with options
- [ ] Implement `Logger::start_span()`
- [ ] Implement `Logger::traced()`
- [ ] Implement `Logger::flush()`
- [ ] Create `init_logger()` convenience function
- [ ] Add default logger singleton pattern

### Async Context Management

- [ ] Set up tokio::task_local for CURRENT_SPAN
- [ ] Implement `current_span()` getter
- [ ] Implement `with_span()` context manager
- [ ] Ensure spans are set as current when created
- [ ] Test nested span contexts

### Event Queue and Batching

- [ ] Create `EventQueue` struct
- [ ] Implement MPSC channel for event queueing
- [ ] Implement background task for batch processing
- [ ] Add batch size limits (100 events)
- [ ] Add batch byte size limits (6MB)
- [ ] Implement parallel batch sending with retries
- [ ] Add exponential backoff for failed requests
- [ ] Implement `flush()` method
- [ ] Add graceful shutdown handling

### HTTP Client and Authentication

- [ ] Create `HttpClient` struct with reqwest
- [ ] Implement `new()` with API key and base URL
- [ ] Implement `send_batch()` method
- [ ] Implement `login()` method
- [ ] Add authentication headers
- [ ] Add retry logic with exponential backoff
- [ ] Add request/response logging for debugging
- [ ] Handle rate limiting (429 responses)

### BraintrustState - Global State Manager

- [ ] Create `BraintrustState` struct
- [ ] Implement initialization from options and env vars
- [ ] Implement `login()` method
- [ ] Implement `log_event()` method
- [ ] Implement `flush()` method
- [ ] Add project info caching
- [ ] Add organization info caching
- [ ] Ensure thread-safe access with Arc

### Testing

- [ ] Unit tests for SpanId encoding/decoding
- [ ] Unit tests for event merging logic
- [ ] Unit tests for span creation and hierarchy
- [ ] Integration tests with mock HTTP server
- [ ] Test async context propagation
- [ ] Test batch queue behavior
- [ ] Test error handling and retries
- [ ] Add test fixtures and utilities

### Examples

- [ ] Create basic tracer example
- [ ] Create nested spans example
- [ ] Create async context example
- [ ] Create LLM tracing example
- [ ] Add example with error handling

## Backlog: Phase 2 & 3

### Advanced Features (Phase 2)

- [ ] Implement specialized span types
- [ ] Implement SpanComponentsV3 binary encoding
- [ ] Add span utilities (with_attributes, set_tags, etc.)
- [ ] Implement comprehensive event merging

### Additional Components (Phase 3)

- [ ] Dataset management
- [ ] Experiment management
- [ ] Evaluation framework
- [ ] Framework integrations

## Notes

- Focus on getting a working tracer first before adding advanced features
- Prioritize idiomatic Rust patterns and ergonomics
- Keep API compatible with JS/Python SDKs where possible
- Reference TypeScript implementation for behavior details
