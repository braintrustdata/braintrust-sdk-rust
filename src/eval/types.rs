use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

use super::CaseError;
use crate::SpanHandle;

/// A single test case for evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case<I, O> {
    /// The input to the task
    pub input: I,
    /// The expected output (optional)
    pub expected: Option<O>,
    /// Additional metadata for this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Tags for categorizing this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Unique identifier for this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Transaction ID for dataset linking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xact_id: Option<String>,
    /// Creation timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

impl<I, O> Default for Case<I, O>
where
    I: Default,
{
    fn default() -> Self {
        Self {
            input: I::default(),
            expected: None,
            metadata: None,
            tags: None,
            id: None,
            xact_id: None,
            created: None,
        }
    }
}

/// A single score result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    /// Name of the score metric
    pub name: String,
    /// Numeric score value
    pub score: f64,
    /// Additional metadata about the score
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl Score {
    pub fn new(name: impl Into<String>, score: f64) -> Self {
        Self {
            name: name.into(),
            score,
            metadata: None,
        }
    }

    pub fn with_metadata(
        name: impl Into<String>,
        score: f64,
        metadata: Map<String, Value>,
    ) -> Self {
        Self {
            name: name.into(),
            score,
            metadata: Some(metadata),
        }
    }
}

/// Collection of scores
pub type Scores = Vec<Score>;

/// Arguments passed to a scorer
#[derive(Debug)]
pub struct ScorerArgs<'a, I, O> {
    /// The input that was provided
    pub input: &'a I,
    /// The output that was produced
    pub output: &'a O,
    /// The expected output (if available)
    pub expected: Option<&'a O>,
    /// Metadata from the case
    pub metadata: Option<&'a Map<String, Value>>,
}

/// Result of evaluating a single case
#[derive(Debug, Serialize)]
pub struct EvalResult<I, O> {
    /// The input that was provided
    pub input: I,
    /// The output that was produced (None if task failed)
    pub output: Option<O>,
    /// The expected output (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<O>,
    /// Scores from all scorers
    pub scores: Scores,
    /// Metadata from the case and task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Tags from the case and task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Computation failures, with task/scorer identity and original causes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<CaseError>,
    /// Source case identity and dataset transaction information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Span ID for this evaluation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

/// Aggregated score statistics
#[derive(Debug, Clone, Serialize)]
pub struct ScoreStats {
    /// Name of the score metric
    pub name: String,
    /// Average score across all cases
    pub mean: f64,
    /// Minimum score
    pub min: f64,
    /// Maximum score
    pub max: f64,
    /// Number of scores
    pub count: usize,
}

/// Summary of evaluation results
#[derive(Debug, Serialize)]
pub struct EvalSummary<I, O> {
    /// Name of the evaluation
    pub name: String,
    /// All individual results
    pub results: Vec<EvalResult<I, O>>,
    /// Aggregated score statistics
    pub score_stats: Vec<ScoreStats>,
    /// Total number of cases evaluated
    pub total_cases: usize,
    /// Number of successful evaluations
    pub successful_cases: usize,
    /// Number of failed evaluations
    pub failed_cases: usize,
    /// Dataset ID (if using Braintrust dataset)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
}

impl<I, O> EvalSummary<I, O> {
    /// Compute statistics from scores that were actually produced, including
    /// successful scorers in cases where another scorer failed.
    pub fn compute_stats(results: &[EvalResult<I, O>]) -> Vec<ScoreStats> {
        use std::collections::HashMap;

        let mut score_map: HashMap<String, Vec<f64>> = HashMap::new();

        for result in results {
            for score in &result.scores {
                score_map
                    .entry(score.name.clone())
                    .or_default()
                    .push(score.score);
            }
        }

        score_map
            .into_iter()
            .map(|(name, scores)| {
                let count = scores.len();
                let sum: f64 = scores.iter().sum();
                let mean = if count > 0 { sum / count as f64 } else { 0.0 };
                let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
                let max = scores.iter().copied().fold(f64::NEG_INFINITY, f64::max);

                ScoreStats {
                    name,
                    mean,
                    min,
                    max,
                    count,
                }
            })
            .collect()
    }
}

/// Hooks provided to tasks for instrumentation and metadata
pub struct TaskHooks<'a, I, O> {
    /// Optional span handle supplied by the recording adapter.
    pub span: Option<&'a SpanHandle>,
    /// Expected output (if available)
    pub expected: Option<&'a O>,
    /// Mutable metadata map for the task to add context
    pub metadata: Arc<Mutex<Map<String, Value>>>,
    /// Mutable tags list for the task to add categorization
    pub tags: Arc<Mutex<Vec<String>>>,
    /// Phantom data for input type
    _phantom: std::marker::PhantomData<I>,
}

impl<'a, I, O> TaskHooks<'a, I, O> {
    pub fn new(span: &'a SpanHandle, expected: Option<&'a O>) -> Self {
        Self::for_case(Some(span), expected, None, None)
    }

    /// Hooks for computation without a client, runtime or recording span.
    pub fn local(expected: Option<&'a O>) -> Self {
        Self::for_case(None, expected, None, None)
    }

    pub(crate) fn for_case(
        span: Option<&'a SpanHandle>,
        expected: Option<&'a O>,
        metadata: Option<Map<String, Value>>,
        tags: Option<Vec<String>>,
    ) -> Self {
        Self {
            span,
            expected,
            metadata: Arc::new(Mutex::new(metadata.unwrap_or_default())),
            tags: Arc::new(Mutex::new(tags.unwrap_or_default())),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Add metadata key-value pair
    pub fn add_metadata(&self, key: impl Into<String>, value: Value) {
        if let Ok(mut metadata) = self.metadata.lock() {
            metadata.insert(key.into(), value);
        }
    }

    /// Add a tag
    pub fn add_tag(&self, tag: impl Into<String>) {
        if let Ok(mut tags) = self.tags.lock() {
            let tag = tag.into();
            if !tags.contains(&tag) {
                tags.push(tag);
            }
        }
    }

    /// Get all metadata
    pub fn get_metadata(&self) -> Option<Map<String, Value>> {
        self.metadata.lock().ok().map(|m| m.clone())
    }

    /// Get all tags
    pub fn get_tags(&self) -> Option<Vec<String>> {
        self.tags.lock().ok().map(|t| t.clone())
    }
}
