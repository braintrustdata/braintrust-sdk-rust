use serde_json::{Map, Value};

use super::{source::CaseSource, EvalConfigError, EvalDefinition};

/// Options for running an evaluation
#[derive(bon::Builder)]
pub struct EvalOpts<I, O> {
    /// Name of the evaluation
    pub name: String,
    /// Owned source providing test cases
    pub source: Box<dyn CaseSource<I, O>>,
    /// Reusable task and ordered scorers
    pub definition: EvalDefinition<I, O>,
    /// Braintrust project for recording; falls back to the client default project.
    pub project_name: Option<String>,
    /// Experiment name for recording; defaults to the evaluation name.
    pub experiment_name: Option<String>,
    /// Run metadata stored on the experiment and used as case defaults.
    /// Case metadata overrides matching keys.
    pub metadata: Option<Map<String, Value>>,
    /// Case tag defaults, combined with source case tags without duplicates.
    pub tags: Option<Vec<String>>,
    /// Maximum concurrent evaluations (default: 5)
    #[builder(default = 5)]
    pub max_concurrency: usize,
    /// Suppress default terminal output (default: false). An explicit request reporter takes precedence.
    #[builder(default = false)]
    pub quiet: bool,
    /// Explicit runtime for local runs. Recorded runs inherit their client binding.
    pub runtime: Option<tokio::runtime::Handle>,
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
