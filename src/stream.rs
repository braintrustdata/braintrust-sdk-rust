//! Incremental aggregation of OpenAI-compatible streaming responses.
mod accumulator;
mod instrumentation;
mod provider;
#[cfg(test)]
mod tests;
mod types;
mod update;

use accumulator::Accumulator;
use anyhow::Result;
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
        if let Some(chunk) = provider::parse(value) {
            self.state.apply(chunk);
        }
    }

    pub fn final_value(&mut self) -> Result<&FinalizedStream> {
        Ok(self.finalized.get_or_insert_with(|| self.state.snapshot()))
    }
    pub fn current_value(&self) -> Result<FinalizedStream> {
        Ok(self.state.snapshot())
    }
    pub fn is_empty(&self) -> bool {
        self.chunks == 0
    }
    pub fn chunk_count(&self) -> usize {
        self.chunks
    }
}
