//! Evaluation framework for Braintrust
//!
//! The eval module provides a type-safe framework for running evaluations on AI systems.
//! It allows you to:
//! - Define test datasets with input/expected output pairs
//! - Run tasks (LLM calls, model inference, etc.) on each input
//! - Score outputs using built-in or custom scorers
//! - Aggregate results and track experiments
//!
//! # Example
//!
//! ```no_run
//! use braintrust_sdk_rust::*;
//! use braintrust_sdk_rust::eval::*;
//!
//! # async fn example() -> Result<()> {
//! let client = BraintrustClient::builder()
//!     .api_key("sk-...")
//!     .build()
//!     .await?;
//!
//! let evaluator = Evaluator::new(client);
//!
//! let cases = vec![
//!     Case {
//!         input: "hello".to_string(),
//!         expected: Some("HELLO".to_string()),
//!         ..Default::default()
//!     },
//! ];
//!
//! let result = evaluator
//!     .run(
//!         EvalOpts::builder()
//!             .name("uppercase-eval".to_string())
//!             .dataset(cases.into())
//!             .task(Box::new(SimpleFnTask::new(|input: &String| {
//!                 Ok(input.to_uppercase())
//!             })))
//!             .scorers(vec![Box::new(ExactMatch::new())])
//!             .build(),
//!     )
//!     .await?;
//!
//! println!("Results: {:?}", result);
//! # Ok(())
//! # }
//! ```

mod bt_runner;
mod dataset;
mod runner;
mod scorer;
mod task;
mod types;

// Re-export public API
pub use bt_runner::{summary_to_value, BtEvalRunner};
pub use dataset::{Dataset, IterDataset, StreamDataset, VecDataset};
pub use runner::{EvalOpts, Evaluator, SyncEvaluator};
pub use scorer::{ExactMatch, LevenshteinScorer, NumericDiffScorer, Scorer, SyncFnScorer};
pub use task::{AsyncFnTask, SimpleAsyncFnTask, SimpleFnTask, SyncFnTask, Task};
pub use types::{Case, EvalResult, EvalSummary, Score, ScoreStats, ScorerArgs, Scores, TaskHooks};
