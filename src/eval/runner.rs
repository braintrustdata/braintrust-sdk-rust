use futures::stream::{FuturesUnordered, StreamExt};
use serde::Serialize;
use serde_json::{Map, Value};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::{BraintrustClient, Result, SpanHandle};

use super::{
    case_runner::CaseRunner,
    scorer::Scorer,
    source::CaseSource,
    task::Task,
    types::{Case, EvalResult, EvalSummary},
    EvalConfigError, EvalRunError, EvalRunFailure,
};

/// Options for running an evaluation
#[derive(bon::Builder)]
pub struct EvalOpts<I, O> {
    /// Name of the evaluation
    pub name: String,
    /// Owned source providing test cases
    pub source: Box<dyn CaseSource<I, O>>,
    /// Task to execute on each input
    pub task: Box<dyn Task<I, O>>,
    /// Scorers to evaluate outputs
    pub scorers: Vec<Box<dyn Scorer<I, O>>>,
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
        let mut names = std::collections::HashSet::new();
        for (index, scorer) in self.scorers.iter().enumerate() {
            let name = scorer.name();
            if name.trim().is_empty() {
                return Err(EvalConfigError::EmptyScorerName { index });
            }
            if !names.insert(name) {
                return Err(EvalConfigError::DuplicateScorerName {
                    name: name.to_owned(),
                });
            }
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
        mut opts: EvalOpts<I, O>,
    ) -> std::result::Result<EvalSummary<I, O>, EvalRunError<I, O>> {
        opts.validate()?;
        if !opts.quiet {
            println!("Running evaluation: {}", opts.name);
        }

        // Create root span for the evaluation
        let root_span = self
            .client
            .span_builder()
            .await?
            .span_type(crate::SpanType::Eval)
            .build();

        // Set name and metadata on the span
        let mut log_builder = crate::SpanLog::builder().name(format!("eval-{}", opts.name));

        if let Some(metadata) = &opts.metadata {
            log_builder = log_builder.metadata(metadata.clone());
        }
        if let Some(tags) = &opts.tags {
            log_builder = log_builder.tags(tags.clone());
        }

        let root_write = root_span.log(log_builder.build().unwrap());

        let dataset = opts.source.provenance();
        let mut results = Vec::new();
        let mut failures = Vec::new();
        let mut case_count = 0;
        let mut futures = FuturesUnordered::new();

        loop {
            // Retain the next-read future while polling active cases. A source
            // may wait for prior computation, and need not be cancellation-safe.
            let mut next = Box::pin(opts.source.next());
            let next_case = loop {
                if futures.is_empty() {
                    break next.await;
                }
                tokio::select! {
                    next_case = next.as_mut() => break next_case,
                    Some((index, result)) = futures.next() => {
                        Self::collect_case_result(index, result, &mut results, &mut failures);
                        if !failures.is_empty() { break Ok(None); }
                    }
                }
            };
            let case = match next_case {
                Ok(Some(case)) => case,
                Ok(None) => break,
                Err(source) => {
                    failures.push(EvalRunFailure::Source {
                        case_index: case_count,
                        source,
                    });
                    break;
                }
            };
            case_count += 1;
            let index = case_count - 1;
            let client = Arc::clone(&self.client);
            let root_span = root_span.clone();
            let task = &opts.task;
            let scorers = &opts.scorers;
            let quiet = opts.quiet;
            futures.push(async move {
                let result = Self::process_case(
                    case,
                    task.as_ref(),
                    scorers,
                    &root_span,
                    &client,
                    index + 1,
                    quiet,
                )
                .await;
                (index, result)
            });
            if futures.len() >= opts.max_concurrency {
                if let Some((index, result)) = futures.next().await {
                    Self::collect_case_result(index, result, &mut results, &mut failures);
                }
                if !failures.is_empty() {
                    break;
                }
            }
        }
        // Source/recording failure does not abandon already scheduled cases.
        while let Some((index, result)) = futures.next().await {
            Self::collect_case_result(index, result, &mut results, &mut failures);
        }
        let end = root_span.end();
        for result in [root_write.await, end.await] {
            if let Err(error) = result {
                failures.push(EvalRunFailure::Recording {
                    case_index: None,
                    source: error.into(),
                });
            }
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        let results: Vec<_> = results.into_iter().map(|(_, result)| result).collect();

        // Compute statistics
        let score_stats = EvalSummary::<I, O>::compute_stats(&results);
        let successful = results.iter().filter(|r| r.errors.is_empty()).count();
        let failed = case_count - successful;

        if !opts.quiet {
            println!("\nEvaluation complete!");
            println!("Total cases: {}", results.len());
            println!("Successful: {}", successful);
            println!("Failed: {}", failed);
            println!("\nScore summary:");
            for stat in &score_stats {
                println!(
                    "  {}: mean={:.3}, min={:.3}, max={:.3}, count={}",
                    stat.name, stat.mean, stat.min, stat.max, stat.count
                );
            }
        }

        let summary = EvalSummary {
            name: opts.name,
            results,
            score_stats,
            total_cases: case_count,
            successful_cases: successful,
            failed_cases: failed,
            dataset,
        };
        if failures.is_empty() {
            Ok(summary)
        } else {
            Err(EvalRunError {
                failures,
                partial: Some(Box::new(summary)),
            })
        }
    }

    fn collect_case_result(
        index: usize,
        result: Result<EvalResult<I, O>>,
        results: &mut Vec<(usize, EvalResult<I, O>)>,
        failures: &mut Vec<EvalRunFailure>,
    ) {
        match result {
            Ok(result) => results.push((index, result)),
            Err(source) => failures.push(EvalRunFailure::Recording {
                case_index: Some(index),
                source,
            }),
        }
    }

    /// Process a single case
    async fn process_case(
        case: Case<I, O>,
        task: &dyn Task<I, O>,
        scorers: &[Box<dyn Scorer<I, O>>],
        _root_span: &SpanHandle,
        client: &BraintrustClient,
        case_num: usize,
        quiet: bool,
    ) -> Result<EvalResult<I, O>> {
        let case_span = client
            .span_builder()
            .await?
            .span_type(crate::SpanType::Eval)
            .build();

        // Build initial span log with name, input, and expected
        let mut log_builder = crate::SpanLog::builder().name(format!("case-{}", case_num));

        if let Ok(input_json) = serde_json::to_value(&case.input) {
            log_builder = log_builder.input(input_json);
        }

        if let Some(expected) = &case.expected {
            if let Ok(expected_json) = serde_json::to_value(expected) {
                log_builder = log_builder.expected(expected_json);
            }
        }

        // Add case metadata and tags
        if let Some(metadata) = &case.metadata {
            log_builder = log_builder.metadata(metadata.clone());
        }
        if let Some(tags) = &case.tags {
            log_builder = log_builder.tags(tags.clone());
        }

        let initial_write = case_span.log(log_builder.build().unwrap());
        let mut result = CaseRunner::run(case, task, scorers, Some(&case_span)).await;
        result.span_id = case_span.export()?.span_id;

        // Recording adapts the completed outcome; computation never waits on
        // HTTP or emits console messages. Observe every accepted write before
        // reporting recording success, including writes that failed earlier.
        let mut final_log = crate::SpanLog::builder().scores(
            result
                .scores
                .iter()
                .map(|score| (score.name.clone(), score.score))
                .collect(),
        );
        if let Some(output) = &result.output {
            if let Ok(output) = serde_json::to_value(output) {
                final_log = final_log.output(output);
            }
        }
        if let Some(metadata) = &result.metadata {
            final_log = final_log.metadata(metadata.clone());
        }
        if let Some(tags) = &result.tags {
            final_log = final_log.tags(tags.clone());
        }
        if !result.errors.is_empty() {
            final_log = final_log.error(
                serde_json::to_value(&result.errors)
                    .map_err(|error| crate::BraintrustError::Background(error.to_string()))?,
            );
        }
        let final_write = case_span.log(final_log.build().unwrap());
        let end = case_span.end();
        let initial_result = initial_write.await;
        let final_result = final_write.await;
        let end_result = end.await;
        initial_result?;
        final_result?;
        end_result?;

        if !quiet {
            let score_str = result
                .scores
                .iter()
                .map(|s| format!("{}={:.3}", s.name, s.score))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Case {}: {}", case_num, score_str);
            for error in &result.errors {
                eprintln!("Case {}: {}", case_num, error);
            }
        }
        Ok(result)
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
    use crate::eval::{scorer::ExactMatch, source::VecCaseSource, task::SimpleFnTask, types::Case};

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
            .task(task)
            .scorers(scorers)
            .build();
    }
}
