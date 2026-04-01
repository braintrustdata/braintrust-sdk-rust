use futures::stream::{FuturesUnordered, StreamExt};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::{BraintrustClient, Result, SpanHandle};

use super::{
    dataset::Dataset,
    scorer::Scorer,
    task::Task,
    types::{Case, EvalResult, EvalSummary, TaskHooks},
};

/// Options for running an evaluation
#[derive(bon::Builder)]
pub struct EvalOpts<I, O> {
    /// Name of the evaluation
    pub name: String,
    /// Dataset providing test cases
    pub dataset: Box<dyn Dataset<I, O>>,
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

/// Async evaluator for running evaluations
pub struct Evaluator<I, O> {
    client: Arc<BraintrustClient>,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O> Evaluator<I, O>
where
    I: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    O: Serialize + DeserializeOwned + Send + Sync + PartialEq + Clone + 'static,
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
    pub async fn run(&self, mut opts: EvalOpts<I, O>) -> Result<EvalSummary<I, O>> {
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

        root_span.log(log_builder.build().unwrap());

        // Get dataset ID if available
        let dataset_id = opts.dataset.id();

        // Collect all results
        let mut results = Vec::new();
        let mut case_count = 0;

        // Create semaphore for concurrency control
        let semaphore = Arc::new(Semaphore::new(opts.max_concurrency));

        // Process cases with controlled concurrency
        let mut futures = FuturesUnordered::new();

        loop {
            // Try to get next case
            let case = match opts.dataset.next().await {
                Ok(Some(case)) => case,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("Error fetching case: {}", e);
                    break;
                }
            };

            case_count += 1;

            // Clone what we need for the async task
            let client = Arc::clone(&self.client);
            let root_span = root_span.clone();
            let task = &opts.task;
            let scorers = &opts.scorers;
            let semaphore = Arc::clone(&semaphore);
            let quiet = opts.quiet;

            // Acquire permit before spawning
            let permit = semaphore.clone().acquire_owned().await.unwrap();

            // Process this case
            let future = async move {
                let result = Self::process_case(
                    case,
                    task.as_ref(),
                    scorers,
                    &root_span,
                    &client,
                    case_count,
                    quiet,
                )
                .await;
                drop(permit); // Release permit
                result
            };

            futures.push(future);

            // Collect completed futures if we have many pending
            while futures.len() >= opts.max_concurrency {
                if let Some(result) = futures.next().await {
                    results.push(result?);
                }
            }
        }

        // Collect remaining futures
        while let Some(result) = futures.next().await {
            results.push(result?);
        }

        // End root span
        root_span.end().await;

        // Compute statistics
        let score_stats = EvalSummary::<I, O>::compute_stats(&results);
        let successful = results.iter().filter(|r| r.error.is_none()).count();
        let failed = results.len() - successful;

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

        Ok(EvalSummary {
            name: opts.name,
            results,
            score_stats,
            total_cases: case_count,
            successful_cases: successful,
            failed_cases: failed,
            dataset_id,
        })
    }

    /// Process a single case
    async fn process_case(
        case: Case<I, O>,
        task: &dyn Task<I, O>,
        scorers: &[Box<dyn Scorer<I, O>>],
        _root_span: &SpanHandle<BraintrustClient>,
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

        case_span.log(log_builder.build().unwrap());

        // Create hooks for the task
        let hooks = TaskHooks::new(&case_span, case.expected.as_ref());

        // Run the task
        let output = match task.run(&case.input, &hooks).await {
            Ok(output) => output,
            Err(e) => {
                // Log error to span
                case_span.log(
                    crate::SpanLog::builder()
                        .error(json!(e.to_string()))
                        .build()
                        .unwrap(),
                );
                case_span.end().await;

                return Ok(EvalResult {
                    input: case.input.clone(),
                    output: None, // Task failed, no output
                    expected: case.expected.clone(),
                    scores: vec![],
                    metadata: case.metadata.clone(),
                    tags: case.tags.clone(),
                    error: Some(e.to_string()),
                    span_id: None,
                });
            }
        };

        // Set output in span
        if let Ok(output_json) = serde_json::to_value(&output) {
            case_span.log(
                crate::SpanLog::builder()
                    .output(output_json)
                    .build()
                    .unwrap(),
            );
        }

        // Run scorers in parallel
        let mut scorer_futures = FuturesUnordered::new();
        for scorer in scorers {
            let scorer_args = super::types::ScorerArgs {
                input: &case.input,
                output: &output,
                expected: case.expected.as_ref(),
                metadata: case.metadata.as_ref(),
            };

            let future = async move {
                match scorer.score(scorer_args).await {
                    Ok(scores) => scores,
                    Err(e) => {
                        eprintln!("Scorer {} failed: {}", scorer.name(), e);
                        vec![]
                    }
                }
            };

            scorer_futures.push(future);
        }

        // Collect all scores
        let mut all_scores = vec![];
        while let Some(scores) = scorer_futures.next().await {
            all_scores.extend(scores);
        }

        // Add scores to span
        let mut scores_map = std::collections::HashMap::new();
        for score in &all_scores {
            scores_map.insert(score.name.clone(), score.score);
        }

        // Get final metadata and tags from hooks
        let final_metadata = hooks.get_metadata().or(case.metadata.clone());
        let final_tags = hooks.get_tags().or(case.tags.clone());

        // Log scores and final metadata/tags
        let mut final_log = crate::SpanLog::builder().scores(scores_map);
        if let Some(meta) = &final_metadata {
            final_log = final_log.metadata(meta.clone());
        }
        if let Some(tags) = &final_tags {
            final_log = final_log.tags(tags.clone());
        }
        case_span.log(final_log.build().unwrap());

        case_span.end().await;

        if !quiet {
            let score_str = all_scores
                .iter()
                .map(|s| format!("{}={:.3}", s.name, s.score))
                .collect::<Vec<_>>()
                .join(", ");
            println!("Case {}: {}", case_num, score_str);
        }

        Ok(EvalResult {
            input: case.input.clone(),
            output: Some(output.clone()),
            expected: case.expected.clone(),
            scores: all_scores,
            metadata: final_metadata,
            tags: final_tags,
            error: None,
            span_id: None,
        })
    }
}

/// Sync evaluator (wraps async evaluator with Runtime::block_on)
pub struct SyncEvaluator<I, O> {
    evaluator: Evaluator<I, O>,
    runtime: tokio::runtime::Runtime,
}

impl<I, O> SyncEvaluator<I, O>
where
    I: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
    O: Serialize + DeserializeOwned + Send + Sync + PartialEq + Clone + 'static,
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
    pub fn run(&self, opts: EvalOpts<I, O>) -> Result<EvalSummary<I, O>> {
        self.runtime.block_on(self.evaluator.run(opts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{dataset::VecDataset, scorer::ExactMatch, task::SimpleFnTask, types::Case};

    // Note: Full integration tests require a BraintrustClient setup
    // These tests verify type correctness and compilation

    #[test]
    fn test_eval_opts_builder() {
        let cases = vec![Case {
            input: "hello".to_string(),
            expected: Some("HELLO".to_string()),
            ..Default::default()
        }];

        let dataset: Box<dyn Dataset<String, String>> = Box::new(VecDataset::new(cases));
        let task: Box<dyn Task<String, String>> =
            Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
        let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

        let _opts = EvalOpts::builder()
            .name("test-eval".to_string())
            .dataset(dataset)
            .task(task)
            .scorers(scorers)
            .build();
    }
}
