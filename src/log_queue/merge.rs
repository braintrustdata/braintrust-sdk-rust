use crate::json_merge::deep_merge;
use crate::types::{Logs3Row, SpanAttributes};
use serde_json::Map;
use std::collections::HashMap;

/// Merge source row into target row (matches logger.rs merge_into).
pub(crate) fn merge_row_into(target: &mut Logs3Row, source: Logs3Row) {
    // Deep merge JSON fields
    if let Some(source_input) = source.input {
        match &mut target.input {
            Some(target_input) => deep_merge(target_input, &source_input),
            None => target.input = Some(source_input),
        }
    }
    if let Some(source_output) = source.output {
        match &mut target.output {
            Some(target_output) => deep_merge(target_output, &source_output),
            None => target.output = Some(source_output),
        }
    }
    if let Some(source_expected) = source.expected {
        match &mut target.expected {
            Some(target_expected) => deep_merge(target_expected, &source_expected),
            None => target.expected = Some(source_expected),
        }
    }
    if let Some(source_error) = source.error {
        match &mut target.error {
            Some(target_error) => deep_merge(target_error, &source_error),
            None => target.error = Some(source_error),
        }
    }
    if let Some(source_context) = source.context {
        match &mut target.context {
            Some(target_context) => deep_merge(target_context, &source_context),
            None => target.context = Some(source_context),
        }
    }

    // Merge map fields
    if let Some(source_scores) = source.scores {
        let target_scores = target.scores.get_or_insert_with(HashMap::new);
        for (k, v) in source_scores {
            target_scores.insert(k, v);
        }
    }
    if let Some(source_meta) = source.metadata {
        let target_meta = target.metadata.get_or_insert_with(Map::new);
        for (k, v) in source_meta {
            target_meta.insert(k, v);
        }
    }
    if let Some(source_metrics) = source.metrics {
        let target_metrics = target.metrics.get_or_insert_with(HashMap::new);
        for (k, v) in source_metrics {
            target_metrics.insert(k, v);
        }
    }

    // Merge tags (deduplicate)
    if let Some(source_tags) = source.tags {
        match &mut target.tags {
            Some(target_tags) => {
                for tag in source_tags {
                    if !target_tags.contains(&tag) {
                        target_tags.push(tag);
                    }
                }
            }
            None => target.tags = Some(source_tags),
        }
    }

    // Overwrite scalar fields
    if let Some(name) = source
        .span_attributes
        .as_ref()
        .and_then(|a| a.name.as_ref())
    {
        target
            .span_attributes
            .get_or_insert_with(SpanAttributes::default)
            .name = Some(name.clone());
    }
}
