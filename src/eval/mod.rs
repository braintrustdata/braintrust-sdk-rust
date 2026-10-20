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
//! # async fn example() -> anyhow::Result<()> {
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
//! // Reuse this definition with a fresh source and options for every run.
//! let definition = EvalDefinition::new(
//!     Box::new(task(|input: &String, _hooks| Box::pin(async move {
//!         Ok(input.to_uppercase())
//!     }))),
//!     vec![Box::new(ExactMatch::new())],
//! )?;
//! let result = evaluator
//!     .run(
//!         EvalOpts::builder()
//!             .name("uppercase-eval".to_string())
//!             .source(cases.into())
//!             .definition(definition.clone())
//!             .build(),
//!     )
//!     .await?;
//!
//! println!("Results: {:?}", result);
//! # Ok(())
//! # }
//! ```

mod bt_runner;
mod case_runner;
mod definition;
mod error;
mod executor;
mod recorder;
mod reporter;
mod runner;
mod scorer;
mod source;
mod task;
mod types;

pub use definition::EvalDefinition;
pub use error::{CaseError, EvalConfigError, EvalRunError, EvalRunFailure};

// Re-export public API
pub use bt_runner::{summary_to_value, BtEvalRunner};
pub use runner::{EvalOpts, Evaluator, SyncEvaluator};
pub use scorer::{scorer, ExactMatch, LevenshteinScorer, NumericDiffScorer, Scorer, SyncFnScorer};
pub use source::{
    CaseSource, DatasetVersionRef, IterCaseSource, SourceError, StreamCaseSource, VecCaseSource,
};
pub use task::{task, SimpleFnTask, SyncFnTask, Task};
pub use types::{Case, EvalResult, EvalSummary, Score, ScoreStats, ScorerArgs, Scores, TaskHooks};
