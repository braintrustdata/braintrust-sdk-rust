use std::sync::{Arc, Mutex};

use crate::error::Result;
use crate::internal::session::CredentialScope;
use crate::span::{SpanBuilder, SpanHandle};
use crate::types::{ParentSpanInfo, SpanPayload};

use crate::internal::ports::{DeliveryControl, PreparedSpan, SpanSubmitter};
use crate::internal::shared_outcome::{WriteOutcome, WriteResponder};

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

impl SpanSubmitter for MockBraintrustClient {
    fn operation(
        &self,
    ) -> std::result::Result<crate::internal::ports::SpanOperation, crate::EnqueueError> {
        Ok(crate::internal::ports::SpanOperation::new(
            Arc::new(self.clone()),
            || {},
        ))
    }
    fn submit_deferred(
        &self,
        _: String,
        prepare: futures::future::BoxFuture<'static, Result<PreparedSpan>>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        let collector = self.collector.clone();
        crate::internal::execution::ExecutionBinding::external(tokio::runtime::Handle::current())
            .response(async move {
                let PreparedSpan {
                    scope,
                    payload,
                    parent_info,
                } = prepare.await.map_err(|source| {
                    crate::WriteError::Delivery(Arc::new(crate::DeliveryError {
                        source: source.into(),
                    }))
                })?;
                collector.push(CapturedSpan {
                    token: scope.token().to_owned(),
                    payload,
                    parent: parent_info,
                });
                Ok(crate::DeliveryReceipt)
            })
    }

    fn submit(
        &self,
        scope: CredentialScope,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        outcome: Option<WriteOutcome>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.collector.push(CapturedSpan {
            token: scope.token().to_owned(),
            payload,
            parent: parent_info,
        });
        let (sender, completion) = crate::Completion::pair();
        let _ = WriteResponder::new(sender, outcome).send(Ok(crate::DeliveryReceipt));
        completion
    }
}

impl DeliveryControl for MockBraintrustClient {
    fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        let (sender, completion) = crate::Completion::pair();
        let _ = sender.send(Ok(crate::FlushReport::default()));
        completion
    }
}

pub(crate) fn mock_span_builder() -> (SpanBuilder, TestSpanCollector) {
    let (submitter, collector) = MockBraintrustClient::new();
    let builder = SpanBuilder::new(submitter, CredentialScope::test("test-token", "org-id"));
    (builder, collector)
}

pub(crate) fn build_test_span() -> (SpanHandle, TestSpanCollector) {
    let (builder, collector) = mock_span_builder();
    (builder.build(), collector)
}
