use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

use super::{CaseError, DatasetVersionRef};
use crate::SpanHandle;

/// A single test case for evaluation
#[derive(Debug, Clone, Serialize, Deserialize, bon::Builder)]
pub struct Case<I, O> {
    /// The input to the task
    pub(crate) input: I,
    /// The expected output (optional)
    pub(crate) expected: Option<O>,
    /// Additional metadata for this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
    /// Tags for categorizing this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tags: Option<Vec<String>>,
    /// Unique identifier for this case
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    /// Transaction ID for dataset linking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) xact_id: Option<String>,
    /// Creation timestamp
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created: Option<String>,
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
    pub(crate) name: String,
    /// Numeric score value
    pub(crate) score: f64,
    /// Additional metadata about the score
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
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
#[derive(Debug, bon::Builder)]
pub struct ScorerArgs<'a, I, O> {
    /// The input that was provided
    pub(crate) input: &'a I,
    /// The output that was produced
    pub(crate) output: &'a O,
    /// The expected output (if available)
    pub(crate) expected: Option<&'a O>,
    /// Metadata from the case
    pub(crate) metadata: Option<&'a Map<String, Value>>,
}

/// Result of evaluating a single case
#[derive(Debug, Serialize)]
pub struct EvalResult<I, O> {
    /// The input that was provided
    pub(crate) input: I,
    /// The output that was produced (None if task failed)
    pub(crate) output: Option<O>,
    /// The expected output (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expected: Option<O>,
    /// Scores from all scorers
    pub(crate) scores: Scores,
    /// Metadata from the case and task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<Map<String, Value>>,
    /// Tags from the case and task
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tags: Option<Vec<String>>,
    /// Computation failures, with task/scorer identity and original causes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) errors: Vec<CaseError>,
    /// Source case identity and dataset transaction information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) case_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) xact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created: Option<String>,
    /// Span ID for this evaluation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) span_id: Option<String>,
}

/// Aggregated score statistics
#[derive(Debug, Clone, Serialize)]
pub struct ScoreStats {
    /// Name of the score metric
    pub(crate) name: String,
    /// Average score across all cases
    pub(crate) mean: f64,
    /// Minimum score
    pub(crate) min: f64,
    /// Maximum score
    pub(crate) max: f64,
    /// Number of scores
    pub(crate) count: usize,
}

/// Summary of evaluation results
#[derive(Debug, Serialize)]
pub struct EvalSummary<I, O> {
    /// Name of the evaluation
    pub(crate) name: String,
    /// All individual results
    pub(crate) results: Vec<EvalResult<I, O>>,
    /// Aggregated score statistics
    pub(crate) score_stats: Vec<ScoreStats>,
    /// Total number of cases evaluated
    pub(crate) total_cases: usize,
    /// Cases without task/scorer errors, independently of recording status.
    pub(crate) successful_cases: usize,
    /// Cases with task/scorer errors, independently of recording status.
    pub(crate) failed_cases: usize,
    /// Full dataset provenance captured before source consumption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dataset: Option<DatasetVersionRef>,
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
    pub(crate) span: Option<&'a SpanHandle>,
    /// Expected output (if available)
    pub(crate) expected: Option<&'a O>,
    /// Mutable metadata map for the task to add context
    pub(crate) metadata: Arc<Mutex<Map<String, Value>>>,
    /// Mutable tags list for the task to add categorization
    pub(crate) tags: Arc<Mutex<Vec<String>>>,
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

    pub(crate) fn rebind<'b>(
        &self,
        span: Option<&'b SpanHandle>,
        expected: Option<&'b O>,
    ) -> TaskHooks<'b, I, O> {
        TaskHooks {
            span,
            expected,
            metadata: self.metadata.clone(),
            tags: self.tags.clone(),
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

impl<I, O> Case<I, O> {
    pub fn input(&self) -> &I {
        &self.input
    }
    pub fn expected(&self) -> Option<&O> {
        self.expected.as_ref()
    }
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    pub fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn xact_id(&self) -> Option<&str> {
        self.xact_id.as_deref()
    }
    pub fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }
}
impl<'a, I, O> ScorerArgs<'a, I, O> {
    pub fn input(&self) -> &'a I {
        self.input
    }
    pub fn output(&self) -> &'a O {
        self.output
    }
    pub fn expected(&self) -> Option<&'a O> {
        self.expected
    }
    pub fn metadata(&self) -> Option<&'a Map<String, Value>> {
        self.metadata
    }
}
impl<I, O> EvalResult<I, O> {
    pub fn input(&self) -> &I {
        &self.input
    }
    pub fn output(&self) -> Option<&O> {
        self.output.as_ref()
    }
    pub fn expected(&self) -> Option<&O> {
        self.expected.as_ref()
    }
    pub fn scores(&self) -> &[Score] {
        &self.scores
    }
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
    pub fn tags(&self) -> Option<&Vec<String>> {
        self.tags.as_ref()
    }
    pub fn errors(&self) -> &[CaseError] {
        &self.errors
    }
    pub fn case_id(&self) -> Option<&str> {
        self.case_id.as_deref()
    }
    pub fn xact_id(&self) -> Option<&str> {
        self.xact_id.as_deref()
    }
    pub fn created(&self) -> Option<&str> {
        self.created.as_deref()
    }
    pub fn span_id(&self) -> Option<&str> {
        self.span_id.as_deref()
    }
}
impl ScoreStats {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn mean(&self) -> f64 {
        self.mean
    }
    pub fn min(&self) -> f64 {
        self.min
    }
    pub fn max(&self) -> f64 {
        self.max
    }
    pub fn count(&self) -> usize {
        self.count
    }
}
impl<I, O> EvalSummary<I, O> {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn results(&self) -> &[EvalResult<I, O>] {
        &self.results
    }
    pub fn score_stats(&self) -> &[ScoreStats] {
        &self.score_stats
    }
    pub fn total_cases(&self) -> usize {
        self.total_cases
    }
    pub fn successful_cases(&self) -> usize {
        self.successful_cases
    }
    pub fn failed_cases(&self) -> usize {
        self.failed_cases
    }
    pub fn dataset(&self) -> Option<&DatasetVersionRef> {
        self.dataset.as_ref()
    }
}
impl<'a, I, O> TaskHooks<'a, I, O> {
    pub fn span(&self) -> Option<&'a SpanHandle> {
        self.span
    }
    pub fn expected(&self) -> Option<&'a O> {
        self.expected
    }
}

impl Score {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn score(&self) -> f64 {
        self.score
    }
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.metadata.as_ref()
    }
}
