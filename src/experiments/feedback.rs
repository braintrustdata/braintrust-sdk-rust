//! Feedback data for updating existing experiment records.

use std::collections::HashMap;
use std::fmt;

use serde_json::{Map, Value};

/// Error type for Feedback builder validation.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FeedbackBuilderError {
    /// A score value was outside the valid range (0.0 to 1.0).
    ScoreOutOfRange { key: String, value: f64 },
}

impl fmt::Display for FeedbackBuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScoreOutOfRange { key, value } => {
                write!(
                    f,
                    "score '{}' has value {} which is outside the valid range [0.0, 1.0]",
                    key, value
                )
            }
        }
    }
}

impl std::error::Error for FeedbackBuilderError {}

/// Feedback data for updating an existing experiment record.
///
/// Use `Feedback::builder()` to construct instances.
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct Feedback {
    pub(crate) expected: Option<Value>,
    pub(crate) scores: Option<HashMap<String, f64>>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) comment: Option<String>,
    pub(crate) source: Option<String>,
}

impl Feedback {
    /// Create a new Feedback builder.
    pub fn builder() -> FeedbackBuilder {
        FeedbackBuilder::new()
    }
}

/// Builder for Feedback.
#[derive(Clone, Default)]
pub struct FeedbackBuilder {
    inner: Feedback,
}

impl FeedbackBuilder {
    /// Create a new FeedbackBuilder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the expected output for comparison.
    pub fn expected(mut self, expected: impl Into<Value>) -> Self {
        self.inner.expected = Some(expected.into());
        self
    }

    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.inner.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.inner
            .scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.inner.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner
            .metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.inner
            .tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Set a comment for this feedback.
    pub fn comment(mut self, comment: impl Into<String>) -> Self {
        self.inner.comment = Some(comment.into());
        self
    }

    /// Set the source of this feedback (e.g., "human", "auto").
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    /// Build the Feedback.
    pub fn build(self) -> std::result::Result<Feedback, FeedbackBuilderError> {
        Ok(self.inner)
    }
}
