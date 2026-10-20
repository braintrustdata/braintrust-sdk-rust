use serde::Serialize;
use serde_json::{Map, Value};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::{BraintrustClient, Result};

use super::{
    source::CaseSource, types::EvalSummary, EvalConfigError, EvalDefinition, EvalRunError,
};

/// Options for running an evaluation
#[derive(bon::Builder)]
pub struct EvalOpts<I, O> {
    /// Name of the evaluation
    pub name: String,
    /// Owned source providing test cases
    pub source: Box<dyn CaseSource<I, O>>,
    /// Reusable task and ordered scorers
    pub definition: EvalDefinition<I, O>,
    /// Braintrust project to log results to
    pub project_name: Option<String>,
    /// Optional experiment name for Braintrust logging
    pub experiment_name: Option<String>,
    /// Optional metadata for the evaluation
    pub metadata: Option<Map<String, Value>>,
    /// Optional tags for the evaluation
    pub tags: Option<Vec<String>>,
    /// Maximum concurrent evaluations (default: 5)
    #[builder(default = 5)]
    pub max_concurrency: usize,
    /// Suppress progress output (default: false)
    #[builder(default = false)]
    pub quiet: bool,
}

impl<I, O> EvalOpts<I, O> {
    /// Validate execution options without reading cases or creating recording work.
    pub fn validate(&self) -> std::result::Result<(), EvalConfigError> {
        if self.max_concurrency == 0 {
            return Err(EvalConfigError::ZeroConcurrency);
        }
        Ok(())
    }
}

/// Async evaluator for running evaluations
pub struct Evaluator<I, O> {
    client: Arc<BraintrustClient>,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O> Evaluator<I, O>
where
    I: Serialize + Send + Sync + 'static,
    O: Serialize + Send + Sync + 'static,
{
    /// Create a new evaluator
    pub fn new(client: BraintrustClient) -> Self {
        Self {
            client: Arc::new(client),
            _phantom: PhantomData,
        }
    }

    /// Create a new evaluator from an Arc client (avoids cloning)
    pub fn from_arc(client: Arc<BraintrustClient>) -> Self {
        Self {
            client,
            _phantom: PhantomData,
        }
    }

    /// Run the evaluation
    pub async fn run(
        &self,
        opts: EvalOpts<I, O>,
    ) -> std::result::Result<EvalSummary<I, O>, EvalRunError<I, O>> {
        let name = opts.name.clone();
        let metadata = opts.metadata.clone();
        let tags = opts.tags.clone();
        let mut reporter = super::reporter::TerminalReporter { quiet: opts.quiet };
        let plan = super::executor::EvalPlan::new(opts)?;
        let recorder =
            super::recorder::BraintrustRecorder::new(self.client.clone(), name, metadata, tags)
                .await?;
        super::executor::EvalExecutor::run(plan, Some(&recorder), &mut reporter).await
    }
}

/// Sync evaluator (wraps async evaluator with Runtime::block_on)
pub struct SyncEvaluator<I, O> {
    evaluator: Evaluator<I, O>,
    runtime: tokio::runtime::Runtime,
}

impl<I, O> SyncEvaluator<I, O>
where
    I: Serialize + Send + Sync + 'static,
    O: Serialize + Send + Sync + 'static,
{
    /// Create a new sync evaluator
    pub fn new(client: BraintrustClient) -> Result<Self> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| crate::BraintrustError::Background(e.to_string()))?;
        Ok(Self {
            evaluator: Evaluator::new(client),
            runtime,
        })
    }

    /// Run the evaluation synchronously
    pub fn run(
        &self,
        opts: EvalOpts<I, O>,
    ) -> std::result::Result<EvalSummary<I, O>, EvalRunError<I, O>> {
        self.runtime.block_on(self.evaluator.run(opts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{
        scorer::ExactMatch, source::VecCaseSource, task::SimpleFnTask, types::Case, Scorer, Task,
    };

    // Note: Full integration tests require a BraintrustClient setup
    // These tests verify type correctness and compilation

    #[test]
    fn test_eval_opts_builder() {
        let cases = vec![Case {
            input: "hello".to_string(),
            expected: Some("HELLO".to_string()),
            ..Default::default()
        }];

        let dataset: Box<dyn CaseSource<String, String>> = Box::new(VecCaseSource::new(cases));
        let task: Box<dyn Task<String, String>> =
            Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
        let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

        let _opts = EvalOpts::builder()
            .name("test-eval".to_string())
            .source(dataset)
            .definition(crate::eval::EvalDefinition::new(task, scorers).unwrap())
            .build();
    }
}
