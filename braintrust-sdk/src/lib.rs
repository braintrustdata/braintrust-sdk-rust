//! # Braintrust SDK for Rust
//!
//! The Braintrust Rust SDK provides a powerful tracer for logging and monitoring
//! LLM applications. It enables hierarchical tracing of spans with automatic
//! batching and efficient transmission to the Braintrust platform.
//!
//! ## Quick Start
//!
//! ```no_run
//! use braintrust_sdk::{Logger, LoggerOptions};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize logger
//!     let logger = Logger::new(LoggerOptions::new("my-project"));
//!
//!     // Create a span and log data
//!     let span = logger.start_span(None).await;
//!     span.log(serde_json::json!({
//!         "input": "What is 2+2?",
//!         "output": "4"
//!     })).await?;
//!     span.end().await?;
//!
//!     // Flush to ensure all events are sent
//!     logger.flush().await?;
//!
//!     Ok(())
//! }
//! ```

pub mod error;
pub mod types;
pub mod client;
pub mod span;
pub mod queue;
pub mod logger;
pub mod lingua;
pub mod memory_logger;

// Test utilities - available in both test and non-test builds for user testing
pub mod test_helpers;

// Re-export main types
pub use error::BraintrustError;
pub use logger::{Logger, LoggerOptions};
pub use span::Span;
pub use types::{SpanEvent, SpanType, StartSpanOptions};
pub use memory_logger::MemoryLogger;
