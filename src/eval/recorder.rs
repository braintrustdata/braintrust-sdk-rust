//! Braintrust serialization and write completions, outside the run engine.
use super::{
    executor::{CaseRecording, EvalRecorder},
    Case, EvalRecording, EvalResult,
};
use crate::{
    BraintrustClient, BraintrustError, Completion, DeliveryReceipt, Result, SpanHandle, SpanLog,
    WriteError,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::Arc;

type Write = Completion<DeliveryReceipt, WriteError>;

pub(crate) struct BraintrustRecorder {
    template: crate::span::SpanBuilder,
    target: EvalRecording,
}
impl BraintrustRecorder {
    pub async fn new(
        client: BraintrustClient,
        submitter: Arc<dyn crate::internal::ports::SpanSubmitter>,
        project_name: Option<String>,
        experiment_name: String,
        metadata: Option<Map<String, Value>>,
    ) -> Result<Self> {
        let mut builder = client
            .experiment_builder_for_operation(submitter.clone(), project_name)
            .await?
            .experiment_name(experiment_name);
        if let Some(metadata) = metadata {
            builder = builder.metadata(metadata);
        }
        let experiment = builder
            .build()
            .map_err(|error| BraintrustError::InvalidConfig(error.to_string()))?;
        let resolved = experiment.register_metadata().await?;
        let target = EvalRecording {
            project_id: resolved.project_id.clone(),
            experiment_id: resolved.experiment_id.clone(),
            experiment_name: resolved.experiment_name.clone(),
        };
        let template = client
            .span_builder_for_operation(
                submitter,
                Some(crate::types::ParentSpanInfo::Experiment {
                    object_id: target.experiment_id.clone(),
                }),
            )
            .await?
            .span_type(crate::SpanType::Eval);
        Ok(Self { template, target })
    }
}

struct BraintrustCase {
    span: SpanHandle,
    initial_write: Write,
    failures: Vec<BraintrustError>,
}
fn serialize<T: Serialize>(
    value: &T,
    field: &str,
    failures: &mut Vec<BraintrustError>,
) -> Option<Value> {
    match serde_json::to_value(value) {
        Ok(value) => Some(value),
        Err(error) => {
            failures.push(BraintrustError::Background(format!(
                "failed to serialize evaluation {field}: {error}"
            )));
            None
        }
    }
}
#[async_trait]
impl<I: Serialize + Send + Sync + 'static, O: Serialize + Send + Sync + 'static> EvalRecorder<I, O>
    for BraintrustRecorder
{
    async fn start(&self, index: usize, case: &Case<I, O>) -> Result<Box<dyn CaseRecording<I, O>>> {
        let span = self
            .template
            .clone()
            .name(format!("case-{}", index + 1))
            .build();
        let mut failures = Vec::new();
        let mut log = SpanLog::builder().name(format!("case-{}", index + 1));
        if let Some(input) = serialize(&case.input, "input", &mut failures) {
            log = log.input(input);
        }
        if let Some(expected) = &case.expected {
            if let Some(expected) = serialize(expected, "expected", &mut failures) {
                log = log.expected(expected);
            }
        }
        if let Some(metadata) = &case.metadata {
            log = log.metadata(metadata.clone());
        }
        if let Some(tags) = &case.tags {
            log = log.tags(tags.clone());
        }
        let initial_write = span.log(log.build().unwrap());
        Ok(Box::new(BraintrustCase {
            span,
            initial_write,
            failures,
        }))
    }
    fn target(&self) -> Option<EvalRecording> {
        Some(self.target.clone())
    }

    async fn finish(&self) -> Vec<BraintrustError> {
        // Every owned case write and end has been observed before the executor
        // finalizes this recorder. The run summary is not a trace span.
        Vec::new()
    }
}
#[async_trait]
impl<I: Serialize + Send + Sync, O: Serialize + Send + Sync> CaseRecording<I, O>
    for BraintrustCase
{
    fn span(&self) -> Option<&SpanHandle> {
        Some(&self.span)
    }
    async fn finish(mut self: Box<Self>, result: &mut EvalResult<I, O>) -> Vec<BraintrustError> {
        match self.span.export() {
            Ok(export) => result.span_id = export.span_id,
            Err(error) => self.failures.push(error),
        }
        let mut log = SpanLog::builder().scores(
            result
                .scores
                .iter()
                .map(|score| (score.name.clone(), score.score))
                .collect(),
        );
        if let Some(output) = &result.output {
            if let Some(output) = serialize(output, "output", &mut self.failures) {
                log = log.output(output);
            }
        }
        if let Some(metadata) = &result.metadata {
            log = log.metadata(metadata.clone());
        }
        if let Some(tags) = &result.tags {
            log = log.tags(tags.clone());
        }
        if !result.errors.is_empty() {
            if let Some(errors) = serialize(&result.errors, "case errors", &mut self.failures) {
                log = log.error(errors);
            }
        }
        let final_write = self.span.log(log.build().unwrap());
        let end = self.span.end();
        // Observe every completion, even if serialization or an earlier write
        // failed. The owned computation result is never discarded here.
        for outcome in [self.initial_write.await, final_write.await, end.await] {
            if let Err(error) = outcome {
                self.failures.push(error.into());
            }
        }
        self.failures
    }
}
