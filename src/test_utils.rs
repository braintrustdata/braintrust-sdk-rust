#![cfg(test)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::Result;
use crate::span::{SpanBuilder, SpanHandle};
use crate::types::{ParentSpanInfo, SpanPayload};

use crate::span::SpanSubmitter;

#[derive(Clone, Default)]
pub(crate) struct TestSpanCollector {
    inner: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl TestSpanCollector {
    pub fn push(&self, span: CapturedSpan) {
        self.inner.lock().unwrap().push(span);
    }

    pub fn spans(&self) -> Vec<CapturedSpan> {
        self.inner.lock().unwrap().clone()
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CapturedSpan {
    #[allow(dead_code)]
    pub token: String,
    pub payload: SpanPayload,
    pub parent: Option<ParentSpanInfo>,
}

#[derive(Clone)]
pub(crate) struct MockBraintrustClient {
    collector: TestSpanCollector,
}

impl MockBraintrustClient {
    pub fn new() -> (Arc<Self>, TestSpanCollector) {
        let collector = TestSpanCollector::default();
        let submitter = Arc::new(Self {
            collector: collector.clone(),
        });
        (submitter, collector)
    }
}

#[async_trait]
impl SpanSubmitter for MockBraintrustClient {
    async fn submit(
        &self,
        token: String,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> Result<()> {
        self.collector.push(CapturedSpan {
            token,
            payload,
            parent: parent_info,
        });
        Ok(())
    }
}

pub(crate) fn mock_span_builder() -> (SpanBuilder, TestSpanCollector) {
    let (submitter, collector) = MockBraintrustClient::new();
    let builder = SpanBuilder::new(submitter, "test-token", "org-id");
    (builder, collector)
}

pub(crate) fn build_test_span() -> (SpanHandle, TestSpanCollector) {
    let (builder, collector) = mock_span_builder();
    (builder.build(), collector)
}

