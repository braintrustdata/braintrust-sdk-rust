//! Incremental aggregation of OpenAI-compatible streaming responses.
mod accumulator;
mod instrumentation;
mod provider;
#[cfg(test)]
mod tests;
mod types;

use accumulator::Accumulator;
pub use instrumentation::wrap_stream_with_span;
use serde_json::Value;
pub use types::*;

/// Accumulates each chunk once without retaining raw provider responses.
/// Snapshots contain the output received so far; `final_value` caches a snapshot
/// until another chunk is pushed. Keep-alive markers are not counted.
#[derive(Clone, Default)]
pub struct BraintrustStream {
    state: Accumulator,
    chunks: usize,
    finalized: Option<FinalizedStream>,
}
impl BraintrustStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and apply a raw chunk. Unknown or malformed chunks are ignored.
    pub fn push(&mut self, value: Value) {
        if value.get("_keep_alive").is_some() {
            return;
        }
        self.chunks += 1;
        self.finalized = None;
        if let Ok(chunk) = serde_json::from_value(value) {
            self.state.apply(chunk);
        }
    }

    /// Return a cached snapshot, recomputing it after the next pushed chunk.
    pub fn final_value(&mut self) -> &FinalizedStream {
        self.finalized.get_or_insert_with(|| self.state.snapshot())
    }
    /// Return an owned snapshot of the output received so far.
    pub fn current_value(&self) -> FinalizedStream {
        self.state.snapshot()
    }
    pub fn is_empty(&self) -> bool {
        self.chunks == 0
    }
    pub fn chunk_count(&self) -> usize {
        self.chunks
    }
}
