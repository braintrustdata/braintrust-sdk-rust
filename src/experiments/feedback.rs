//! Feedback data for updating existing experiment records.

use std::collections::HashMap;

use serde_json::{Map, Value};

/// Feedback data for updating an existing experiment record.
///
/// Use `Feedback::builder()` to construct instances.
#[derive(bon::Builder, Clone, Default)]
#[builder(derive(Clone))]
#[non_exhaustive]
pub struct Feedback {
    #[builder(field)]
    pub(crate) scores: Option<HashMap<String, f64>>,
    #[builder(field)]
    pub(crate) metadata: Option<Map<String, Value>>,
    #[builder(field)]
    pub(crate) tags: Option<Vec<String>>,
    #[builder(into)]
    pub(crate) expected: Option<Value>,
    #[builder(into)]
    pub(crate) comment: Option<String>,
    #[builder(into)]
    pub(crate) source: Option<String>,
}

impl<S: feedback_builder::State> FeedbackBuilder<S> {
    /// Set multiple scores at once.
    pub fn scores(mut self, scores: HashMap<String, f64>) -> Self {
        self.scores = Some(scores);
        self
    }

    /// Add a single score (0-1 range recommended).
    pub fn score(mut self, key: impl Into<String>, value: f64) -> Self {
        self.scores
            .get_or_insert_with(HashMap::new)
            .insert(key.into(), value);
        self
    }

    /// Set the metadata map.
    pub fn metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Add a single metadata entry.
    pub fn metadata_entry(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata
            .get_or_insert_with(Map::new)
            .insert(key.into(), value.into());
        self
    }

    /// Set multiple tags at once.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Add a single tag for categorization.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.get_or_insert_with(Vec::new).push(tag.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builder_preserves_defaults_and_collection_updates() {
        let empty = Feedback::builder().build();
        assert!(empty.expected.is_none() && empty.scores.is_none() && empty.tags.is_none());
        let feedback = Feedback::builder()
            .score("old", 0.0)
            .scores(HashMap::from([("accuracy".into(), 0.5)]))
            .score("accuracy", 1.0)
            .metadata(Map::new())
            .metadata_entry("reviewed", true)
            .tags(vec!["first".into()])
            .tag("second")
            .expected(json!(null))
            .comment("review")
            .source("human")
            .build();
        assert_eq!(
            feedback.scores.unwrap(),
            HashMap::from([("accuracy".into(), 1.0)])
        );
        assert_eq!(feedback.metadata.unwrap()["reviewed"], true);
        assert_eq!(feedback.tags.unwrap(), ["first", "second"]);
        assert_eq!(feedback.expected, Some(Value::Null));
        assert_eq!(feedback.comment.as_deref(), Some("review"));
        assert_eq!(feedback.source.as_deref(), Some("human"));
    }
}
