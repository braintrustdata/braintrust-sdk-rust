use serde_json::{Map, Value};

use super::{source::CaseSource, EvalConfigError, EvalDefinition};

/// Options for running an evaluation
#[derive(bon::Builder)]
pub struct EvalOpts<I, O> {
    /// Name of the evaluation
    pub(crate) name: String,
    /// Owned source providing test cases
    pub(crate) source: Box<dyn CaseSource<I, O>>,
    /// Reusable task and ordered scorers
    pub(crate) definition: EvalDefinition<I, O>,
    /// Braintrust project to log results to
    pub(crate) project_name: Option<String>,
    /// Optional experiment name for Braintrust logging
    pub(crate) experiment_name: Option<String>,
    /// Optional metadata for the evaluation
    pub(crate) metadata: Option<Map<String, Value>>,
    /// Optional tags for the evaluation
    pub(crate) tags: Option<Vec<String>>,
    /// Maximum concurrent evaluations (default: 5)
    #[builder(default = 5)]
    pub(crate) max_concurrency: usize,
    /// Suppress progress output (default: false)
    #[builder(default = false)]
    pub(crate) quiet: bool,
    /// Explicit reporter; overrides default terminal output.
    pub(crate) reporter: Option<Box<dyn super::EvalReporter<I, O>>>,
    /// Explicit runtime for local runs. Recorded runs inherit their client binding.
    pub(crate) runtime: Option<tokio::runtime::Handle>,
}

impl<I, O> EvalOpts<I, O> {
    /// Optional experiment label supplied by the caller.
    pub fn experiment_name(&self) -> Option<&str> {
        self.experiment_name.as_deref()
    }

    /// Set run-owned output, overriding the default reporter.
    pub fn with_reporter(mut self, reporter: impl super::EvalReporter<I, O> + 'static) -> Self {
        self.reporter = Some(Box::new(reporter));
        self
    }
    pub(crate) fn take_reporter(&mut self) -> Box<dyn super::EvalReporter<I, O>> {
        self.reporter
            .take()
            .unwrap_or_else(|| Box::new(super::reporter::TerminalReporter { quiet: self.quiet }))
    }

    /// Validate execution options without reading cases or creating recording work.
    pub fn validate(&self) -> std::result::Result<(), EvalConfigError> {
        if self.max_concurrency == 0 {
            return Err(EvalConfigError::ZeroConcurrency);
        }
        Ok(())
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
