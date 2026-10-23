//! Stream polling and span lifecycle, independent of provider accumulation.
use super::BraintrustStream;
use crate::{types::usage_metrics_to_map, SpanHandle, SpanLog};
use futures::{future::BoxFuture, Stream};
use serde_json::Value;
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{Duration, Instant},
};

const UPDATE_CHUNKS: usize = 10;
const UPDATE_INTERVAL: Duration = Duration::from_secs(1);

/// Yield the original stream items while recording incremental output and usage.
/// Exhaustion waits for final delivery. Dropping early submits the partial output
/// and final span immediately; drain the client before exiting to observe delivery.
/// Provider errors are yielded unchanged and mark the span as failed.
pub fn wrap_stream_with_span<S, E>(
    stream: S,
    span: SpanHandle,
) -> Pin<Box<dyn Stream<Item = Result<Value, E>> + Send>>
where
    S: Stream<Item = Result<Value, E>> + Send + Unpin + 'static,
    E: Send + 'static,
{
    Box::pin(RecordedStream {
        inner: stream,
        recorder: Recorder {
            span: Some(span),
            state: BraintrustStream::new(),
            started: Instant::now(),
            last_update: Instant::now(),
            ttft: false,
        },
        finalizing: None,
        done: false,
    })
}

struct Recorder {
    span: Option<SpanHandle>,
    state: BraintrustStream,
    started: Instant,
    last_update: Instant,
    ttft: bool,
}
impl Recorder {
    fn push(&mut self, value: &Value) {
        if value.get("_keep_alive").is_some() {
            return;
        }
        let Some(span) = &self.span else {
            return;
        };
        if !self.ttft && super::provider::has_content(value) {
            self.ttft = true;
            if let Ok(log) = SpanLog::builder()
                .metric("time_to_first_token", self.started.elapsed().as_secs_f64())
                .build()
            {
                drop(span.log(log));
            }
        }
        self.state.push(value.clone());
        if self.state.chunk_count() % UPDATE_CHUNKS == 0
            || self.last_update.elapsed() > UPDATE_INTERVAL
        {
            if let Ok(partial) = self.state.current_value() {
                if let Ok(output) = serde_json::to_value(partial.output()) {
                    drop(span.log(SpanLog {
                        output: Some(output),
                        ..Default::default()
                    }));
                }
            }
            drop(span.flush());
            self.last_update = Instant::now();
        }
    }
    // Submission is synchronous so Drop can finish accepted partial output too.
    fn finish(&mut self) -> Option<BoxFuture<'static, ()>> {
        let span = self.span.take()?;
        if !self.state.is_empty() {
            if let Ok(finalized) = self.state.final_value() {
                let mut builder = SpanLog::builder();
                if let Ok(output) = serde_json::to_value(finalized.output()) {
                    builder = builder.output(output);
                }
                if let Some(metadata) = finalized.metadata().to_map() {
                    builder = builder.metadata(metadata);
                }
                if let Some(usage) = finalized.usage() {
                    builder = builder.metrics(usage_metrics_to_map(usage.clone()));
                }
                if let Ok(log) = builder.build() {
                    drop(span.log(log));
                }
            }
        }
        let end = span.end();
        let flush = span.flush();
        Some(Box::pin(async move {
            if let Err(error) = end.await {
                tracing::warn!(%error, "Failed to deliver final stream span");
            }
            if let Err(error) = flush.await {
                tracing::warn!(%error, "Failed to flush stream span");
            }
        }))
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        drop(self.finish());
    }
}
struct RecordedStream<S> {
    inner: S,
    recorder: Recorder,
    finalizing: Option<BoxFuture<'static, ()>>,
    done: bool,
}
impl<S, E> Stream for RecordedStream<S>
where
    S: Stream<Item = Result<Value, E>> + Unpin,
{
    type Item = Result<Value, E>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        if this.finalizing.is_none() {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(item)) => {
                    match &item {
                        Ok(value) => this.recorder.push(value),
                        Err(_) => {
                            if let Some(span) = &this.recorder.span {
                                if let Ok(log) = SpanLog::builder().error("stream failed").build() {
                                    drop(span.log(log));
                                }
                            }
                        }
                    }
                    return Poll::Ready(Some(item));
                }
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => this.finalizing = this.recorder.finish(),
            }
        }
        if let Some(future) = &mut this.finalizing {
            if future.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        this.finalizing = None;
        this.done = true;
        Poll::Ready(None)
    }
}
