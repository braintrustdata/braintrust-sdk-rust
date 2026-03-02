use crate::json_merge::{deep_merge, deep_merge_maps_with_stop_paths, deep_merge_with_stop_paths};
use crate::types::{Logs3Row, SpanAttributes};
use indexmap::IndexSet;
use std::collections::HashSet;

/// Extract the sub-paths for a given top-level field from `merge_paths`.
///
/// Per the Braintrust API, `merge_paths` elements are top-level-prefixed:
/// `["input", "data"]` means "within `input`, stop deep merge at `data`".
///
/// Returns the set of sub-paths (with the leading field name stripped) to use
/// as stop paths when calling `deep_merge_with_stop_paths` on that field.
/// Returns an empty set if no paths in `merge_paths` start with `field`.
fn extract_sub_paths<'a>(
    merge_paths: &'a Option<Vec<Vec<String>>>,
    field: &str,
) -> HashSet<Vec<&'a str>> {
    let Some(paths) = merge_paths else {
        return HashSet::new();
    };
    paths
        .iter()
        .filter(|path| path.first().map(|s| s.as_str()) == Some(field))
        .map(|path| path[1..].iter().map(|s| s.as_str()).collect())
        .collect()
}

/// Merge source row into target row.
///
/// Mirrors the TypeScript SDK's `mergeDicts` (via `mergeDictsWithPathsHelper`):
/// - JSON object fields (`input`, `output`, `expected`, `error`, `context`) are deep-merged.
/// - Map fields (`scores`, `metadata`, `metrics`) are merged key-by-key.
/// - `tags` is a set-union (TypeScript's SET_UNION_FIELDS = {"tags"}).
/// - Identity fields (`id`, `span_id`, `root_span_id`, `span_parents`, `created`,
///   `destination`, `org_id`) are preserved from the target without modification.
///   (These correspond to TypeScript's MERGE_ROW_SKIP_FIELDS.)
/// - TypeScript also skips `_parent_id`, an internal span-cache field that does not
///   appear in the Rust wire format, so no special handling is needed here.
///
/// Fully supports `_merge_paths` for controlling merge behavior.
/// When `source.merge_paths` is set, those paths are used as stop paths for deep merge
/// (i.e., values at those paths are replaced wholesale instead of deep-merged).
///
/// Note: `target.is_merge` is intentionally not modified here.  The TypeScript SDK's
/// `mergeRowBatch` deletes `_is_merge` from the merged result when the existing row
/// didn't originally have it (`preserveNoMerge` logic).  In this SDK, `is_merge` is
/// always `Some(true)` or `None` — never `Some(false)` — so the effective behavior is
/// identical: if target had `None` it keeps `None`; if it had `Some(true)` it keeps
/// `Some(true)`.  Any future code that explicitly sets `is_merge = Some(false)` would
/// need to re-evaluate this logic to stay aligned with TypeScript.
pub(crate) fn merge_row_into(target: &mut Logs3Row, source: Logs3Row) {
    // Deep merge JSON fields, respecting merge_paths stop paths.
    // Paths in merge_paths are top-level-prefixed: ["input", "key"] means "within
    // input, stop deep merge at key". We strip the leading field name and pass the
    // remaining sub-paths to deep_merge_with_stop_paths.
    if let Some(source_input) = source.input {
        match &mut target.input {
            Some(target_input) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "input");
                if stop_paths.is_empty() {
                    deep_merge(target_input, &source_input);
                } else {
                    deep_merge_with_stop_paths(target_input, &source_input, &stop_paths);
                }
            }
            None => target.input = Some(source_input),
        }
    }
    if let Some(source_output) = source.output {
        match &mut target.output {
            Some(target_output) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "output");
                if stop_paths.is_empty() {
                    deep_merge(target_output, &source_output);
                } else {
                    deep_merge_with_stop_paths(target_output, &source_output, &stop_paths);
                }
            }
            None => target.output = Some(source_output),
        }
    }
    if let Some(source_expected) = source.expected {
        match &mut target.expected {
            Some(target_expected) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "expected");
                if stop_paths.is_empty() {
                    deep_merge(target_expected, &source_expected);
                } else {
                    deep_merge_with_stop_paths(target_expected, &source_expected, &stop_paths);
                }
            }
            None => target.expected = Some(source_expected),
        }
    }
    if let Some(source_error) = source.error {
        match &mut target.error {
            Some(target_error) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "error");
                if stop_paths.is_empty() {
                    deep_merge(target_error, &source_error);
                } else {
                    deep_merge_with_stop_paths(target_error, &source_error, &stop_paths);
                }
            }
            None => target.error = Some(source_error),
        }
    }
    if let Some(source_context) = source.context {
        match &mut target.context {
            Some(target_context) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "context");
                if stop_paths.is_empty() {
                    deep_merge(target_context, &source_context);
                } else {
                    deep_merge_with_stop_paths(target_context, &source_context, &stop_paths);
                }
            }
            None => target.context = Some(source_context),
        }
    }

    // Merge map fields (last-write-wins for keys)
    if let Some(source_scores) = source.scores {
        target.scores.get_or_insert_default().extend(source_scores);
    }
    if let Some(source_metrics) = source.metrics {
        target
            .metrics
            .get_or_insert_default()
            .extend(source_metrics);
    }

    // Deep merge metadata, respecting merge_paths stop paths.
    // Paths like ["metadata", "config"] mean "within metadata, stop at config".
    // We strip the leading "metadata" component just like the other JSON fields.
    if let Some(source_meta) = source.metadata {
        match &mut target.metadata {
            Some(target_meta) => {
                let stop_paths = extract_sub_paths(&source.merge_paths, "metadata");
                if stop_paths.is_empty() {
                    // Convert to serde_json::Value for deep merge, then back to Map
                    let mut target_value = serde_json::Value::Object(target_meta.clone());
                    let source_value = serde_json::Value::Object(source_meta);
                    deep_merge(&mut target_value, &source_value);
                    if let serde_json::Value::Object(merged) = target_value {
                        *target_meta = merged;
                    }
                } else {
                    deep_merge_maps_with_stop_paths(target_meta, &source_meta, &stop_paths);
                }
            }
            None => target.metadata = Some(source_meta),
        }
    }

    // Merge tags using IndexSet (O(n) set-union, matches TypeScript SDK)
    if let Some(source_tags) = source.tags {
        match &mut target.tags {
            Some(target_tags) => {
                // Convert to IndexSet, extend, convert back to Vec
                let mut tag_set: IndexSet<String> = target_tags.iter().cloned().collect();
                tag_set.extend(source_tags);
                *target_tags = tag_set.into_iter().collect();
            }
            None => target.tags = Some(source_tags),
        }
    }

    // Merge span_attributes fields
    if let Some(source_attrs) = source.span_attributes {
        let target_attrs = target
            .span_attributes
            .get_or_insert_with(SpanAttributes::default);
        if let Some(name) = source_attrs.name {
            target_attrs.name = Some(name);
        }
        if let Some(span_type) = source_attrs.span_type {
            target_attrs.span_type = Some(span_type);
        }
        if let Some(purpose) = source_attrs.purpose {
            target_attrs.purpose = Some(purpose);
        }
        for (k, v) in source_attrs.extra {
            target_attrs.extra.insert(k, v);
        }
    }

    // Combine merge_paths from both entries.
    // Only retain merge paths if both entries are merge-type entries.
    let final_is_merge = target.is_merge.unwrap_or(false) && source.is_merge.unwrap_or(false);
    if final_is_merge {
        // Combine unique merge paths from both entries
        if let Some(source_paths) = source.merge_paths {
            match &mut target.merge_paths {
                Some(target_paths) => {
                    // Create a HashSet of paths in target for efficient lookup
                    let target_path_set: HashSet<&Vec<String>> = target_paths.iter().collect();
                    // Only add paths from source that aren't already in target
                    let mut new_paths: Vec<Vec<String>> = source_paths
                        .into_iter()
                        .filter(|path| !target_path_set.contains(path))
                        .collect();
                    target_paths.append(&mut new_paths);
                }
                None => {
                    target.merge_paths = Some(source_paths);
                }
            }
        }
        // If source doesn't have merge_paths, keep target's as-is
    } else {
        // If the result is not a merge-type entry, clear the merge paths
        target.merge_paths = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogDestination, SpanAttributes};
    use chrono::Utc;
    use serde_json::json;
    use std::collections::HashMap;

    fn make_base_row(id: &str) -> Logs3Row {
        Logs3Row {
            id: id.to_string(),
            is_merge: Some(true),
            merge_paths: None,
            span_id: format!("span-{}", id),
            root_span_id: format!("root-{}", id),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: Some("test-org".to_string()),
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            extra: HashMap::new(),
            created: Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: None,
        }
    }

    #[test]
    fn test_merge_json_fields_deep_merge() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"a": {"b": 1}, "c": 2}));
        target.output = Some(json!({"x": 1}));

        let mut source = make_base_row("1");
        source.input = Some(json!({"a": {"d": 3}, "e": 4}));
        source.output = Some(json!({"y": 2}));

        merge_row_into(&mut target, source);

        // Input should be deep merged
        assert_eq!(
            target.input,
            Some(json!({"a": {"b": 1, "d": 3}, "c": 2, "e": 4}))
        );
        // Output should be deep merged
        assert_eq!(target.output, Some(json!({"x": 1, "y": 2})));
    }

    #[test]
    fn test_merge_json_fields_source_overwrites_primitives() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"count": 1}));

        let mut source = make_base_row("1");
        source.input = Some(json!({"count": 2}));

        merge_row_into(&mut target, source);

        // Primitives are overwritten
        assert_eq!(target.input, Some(json!({"count": 2})));
    }

    #[test]
    fn test_merge_all_json_fields() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"in": 1}));
        target.output = Some(json!({"out": 1}));
        target.expected = Some(json!({"exp": 1}));
        target.error = Some(json!({"err": 1}));
        target.context = Some(json!({"ctx": 1}));

        let mut source = make_base_row("1");
        source.input = Some(json!({"in2": 2}));
        source.output = Some(json!({"out2": 2}));
        source.expected = Some(json!({"exp2": 2}));
        source.error = Some(json!({"err2": 2}));
        source.context = Some(json!({"ctx2": 2}));

        merge_row_into(&mut target, source);

        assert_eq!(target.input, Some(json!({"in": 1, "in2": 2})));
        assert_eq!(target.output, Some(json!({"out": 1, "out2": 2})));
        assert_eq!(target.expected, Some(json!({"exp": 1, "exp2": 2})));
        assert_eq!(target.error, Some(json!({"err": 1, "err2": 2})));
        assert_eq!(target.context, Some(json!({"ctx": 1, "ctx2": 2})));
    }

    #[test]
    fn test_merge_json_fields_target_none() {
        let mut target = make_base_row("1");
        target.input = None;

        let mut source = make_base_row("1");
        source.input = Some(json!({"data": "value"}));

        merge_row_into(&mut target, source);

        assert_eq!(target.input, Some(json!({"data": "value"})));
    }

    #[test]
    fn test_merge_json_fields_source_none() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"data": "value"}));

        let mut source = make_base_row("1");
        source.input = None;

        merge_row_into(&mut target, source);

        // Target should remain unchanged
        assert_eq!(target.input, Some(json!({"data": "value"})));
    }

    #[test]
    fn test_merge_map_fields_last_write_wins() {
        let mut target = make_base_row("1");
        target.scores = Some({
            let mut m = HashMap::new();
            m.insert("score1".to_string(), 0.5);
            m.insert("score2".to_string(), 0.6);
            m
        });

        let mut source = make_base_row("1");
        source.scores = Some({
            let mut m = HashMap::new();
            m.insert("score2".to_string(), 0.8); // Overwrites
            m.insert("score3".to_string(), 0.9); // New
            m
        });

        merge_row_into(&mut target, source);

        let scores = target.scores.unwrap();
        assert_eq!(scores.get("score1").unwrap(), &0.5);
        assert_eq!(scores.get("score2").unwrap(), &0.8); // Overwritten
        assert_eq!(scores.get("score3").unwrap(), &0.9);
    }

    #[test]
    fn test_merge_metrics_last_write_wins() {
        let mut target = make_base_row("1");
        target.metrics = Some({
            let mut m = HashMap::new();
            m.insert("latency".to_string(), 100.0);
            m
        });

        let mut source = make_base_row("1");
        source.metrics = Some({
            let mut m = HashMap::new();
            m.insert("latency".to_string(), 200.0); // Overwrites
            m.insert("tokens".to_string(), 50.0); // New
            m
        });

        merge_row_into(&mut target, source);

        let metrics = target.metrics.unwrap();
        assert_eq!(metrics.get("latency").unwrap(), &200.0);
        assert_eq!(metrics.get("tokens").unwrap(), &50.0);
    }

    #[test]
    fn test_merge_tags_set_union_with_indexset() {
        let mut target = make_base_row("1");
        target.tags = Some(vec!["tag1".to_string(), "tag2".to_string()]);

        let mut source = make_base_row("1");
        source.tags = Some(vec!["tag2".to_string(), "tag3".to_string()]);

        merge_row_into(&mut target, source);

        let tags = target.tags.unwrap();
        // Should have all unique tags, preserving order
        assert_eq!(tags.len(), 3);
        assert!(tags.contains(&"tag1".to_string()));
        assert!(tags.contains(&"tag2".to_string()));
        assert!(tags.contains(&"tag3".to_string()));
    }

    #[test]
    fn test_merge_tags_preserves_insertion_order() {
        let mut target = make_base_row("1");
        target.tags = Some(vec!["a".to_string(), "b".to_string()]);

        let mut source = make_base_row("1");
        source.tags = Some(vec!["c".to_string(), "d".to_string()]);

        merge_row_into(&mut target, source);

        let tags = target.tags.unwrap();
        // Original tags should come first, then new ones
        assert_eq!(
            tags,
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn test_merge_tags_deduplicates() {
        let mut target = make_base_row("1");
        target.tags = Some(vec!["a".to_string(), "b".to_string(), "c".to_string()]);

        let mut source = make_base_row("1");
        source.tags = Some(vec!["b".to_string(), "c".to_string(), "d".to_string()]);

        merge_row_into(&mut target, source);

        let tags = target.tags.unwrap();
        // Should deduplicate: a, b, c (from target), d (new from source)
        assert_eq!(tags.len(), 4);
        assert_eq!(
            tags,
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ]
        );
    }

    #[test]
    fn test_merge_span_attributes() {
        use crate::types::SpanType;

        let mut target = make_base_row("1");
        target.span_attributes = Some(SpanAttributes {
            name: Some("original".to_string()),
            span_type: Some(SpanType::Llm),
            purpose: None,
            extra: {
                let mut m = HashMap::new();
                m.insert("key1".to_string(), json!("value1"));
                m
            },
        });

        let mut source = make_base_row("1");
        source.span_attributes = Some(SpanAttributes {
            name: Some("updated".to_string()),
            span_type: None,
            purpose: Some("eval".to_string()),
            extra: {
                let mut m = HashMap::new();
                m.insert("key2".to_string(), json!("value2"));
                m
            },
        });

        merge_row_into(&mut target, source);

        let attrs = target.span_attributes.unwrap();
        assert_eq!(attrs.name, Some("updated".to_string()));
        assert_eq!(attrs.span_type, Some(SpanType::Llm));
        assert_eq!(attrs.purpose, Some("eval".to_string()));
        assert_eq!(attrs.extra.len(), 2);
        assert_eq!(attrs.extra.get("key1").unwrap(), &json!("value1"));
        assert_eq!(attrs.extra.get("key2").unwrap(), &json!("value2"));
    }

    #[test]
    fn test_merge_span_attributes_target_none() {
        let mut target = make_base_row("1");
        target.span_attributes = None;

        let mut source = make_base_row("1");
        source.span_attributes = Some(SpanAttributes {
            name: Some("test".to_string()),
            span_type: None,
            purpose: None,
            extra: HashMap::new(),
        });

        merge_row_into(&mut target, source);

        assert!(target.span_attributes.is_some());
        assert_eq!(
            target.span_attributes.unwrap().name,
            Some("test".to_string())
        );
    }

    #[test]
    fn test_merge_metadata_with_merge_paths_stops_at_paths() {
        let mut target = make_base_row("1");
        target.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "config".to_string(),
                json!({"model": "gpt-4", "temperature": 0.7}),
            );
            m.insert("other".to_string(), json!({"a": 1}));
            m
        });

        let mut source = make_base_row("1");
        source.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "config".to_string(),
                json!({"model": "gpt-3.5", "max_tokens": 100}),
            );
            m.insert("other".to_string(), json!({"b": 2}));
            m
        });
        // Top-level-prefixed stop path: ["metadata", "config"] stops deep merge of metadata.config
        source.merge_paths = Some(vec![vec!["metadata".to_string(), "config".to_string()]]);

        merge_row_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        // config should be completely replaced (not deep merged)
        assert_eq!(
            metadata.get("config").unwrap(),
            &json!({"model": "gpt-3.5", "max_tokens": 100})
        );
        // other should be deep merged (not a stop path)
        assert_eq!(metadata.get("other").unwrap(), &json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_merge_metadata_with_nested_merge_paths() {
        let mut target = make_base_row("1");
        target.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "config".to_string(),
                json!({"llm": {"model": "gpt-4", "temp": 0.7}, "other": {"x": 1}}),
            );
            m
        });

        let mut source = make_base_row("1");
        source.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "config".to_string(),
                json!({"llm": {"model": "gpt-3.5"}, "other": {"y": 2}}),
            );
            m
        });
        // Top-level-prefixed stop path: ["metadata", "config", "llm"] stops at metadata.config.llm
        source.merge_paths = Some(vec![vec![
            "metadata".to_string(),
            "config".to_string(),
            "llm".to_string(),
        ]]);

        merge_row_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        let config = metadata.get("config").unwrap();
        // llm should be replaced completely
        assert_eq!(config["llm"], json!({"model": "gpt-3.5"}));
        // other should be deep merged (not on stop path)
        assert_eq!(config["other"], json!({"x": 1, "y": 2}));
    }

    #[test]
    fn test_merge_metadata_without_merge_paths_deep_merges() {
        let mut target = make_base_row("1");
        target.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("config".to_string(), json!({"a": 1, "b": 2}));
            m
        });

        let mut source = make_base_row("1");
        source.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("config".to_string(), json!({"b": 3, "c": 4}));
            m
        });
        source.merge_paths = None; // No stop paths

        merge_row_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        // Should deep merge without stop paths
        assert_eq!(
            metadata.get("config").unwrap(),
            &json!({"a": 1, "b": 3, "c": 4})
        );
    }

    #[test]
    fn test_merge_paths_combined_when_both_merge() {
        let mut target = make_base_row("1");
        target.is_merge = Some(true);
        target.merge_paths = Some(vec![vec!["path1".to_string()], vec!["path2".to_string()]]);

        let mut source = make_base_row("1");
        source.is_merge = Some(true);
        source.merge_paths = Some(vec![
            vec!["path2".to_string()], // Duplicate
            vec!["path3".to_string()], // New
        ]);

        merge_row_into(&mut target, source);

        let merge_paths = target.merge_paths.unwrap();
        assert_eq!(merge_paths.len(), 3);
        assert!(merge_paths.contains(&vec!["path1".to_string()]));
        assert!(merge_paths.contains(&vec!["path2".to_string()]));
        assert!(merge_paths.contains(&vec!["path3".to_string()]));
    }

    #[test]
    fn test_merge_paths_deduplicates() {
        let mut target = make_base_row("1");
        target.is_merge = Some(true);
        target.merge_paths = Some(vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["c".to_string()],
        ]);

        let mut source = make_base_row("1");
        source.is_merge = Some(true);
        source.merge_paths = Some(vec![
            vec!["a".to_string(), "b".to_string()], // Duplicate
            vec!["c".to_string()],                  // Duplicate
            vec!["d".to_string()],                  // New
        ]);

        merge_row_into(&mut target, source);

        let merge_paths = target.merge_paths.unwrap();
        // Should deduplicate: only unique paths
        assert_eq!(merge_paths.len(), 3);
    }

    #[test]
    fn test_merge_paths_cleared_when_target_not_merge() {
        let mut target = make_base_row("1");
        target.is_merge = None; // Not a merge entry
        target.merge_paths = Some(vec![vec!["path1".to_string()]]);

        let mut source = make_base_row("1");
        source.is_merge = Some(true);
        source.merge_paths = Some(vec![vec!["path2".to_string()]]);

        merge_row_into(&mut target, source);

        // final_is_merge = false (target.is_merge is None)
        assert!(target.merge_paths.is_none());
    }

    #[test]
    fn test_merge_paths_cleared_when_source_not_merge() {
        let mut target = make_base_row("1");
        target.is_merge = Some(true);
        target.merge_paths = Some(vec![vec!["path1".to_string()]]);

        let mut source = make_base_row("1");
        source.is_merge = None; // Not a merge entry
        source.merge_paths = Some(vec![vec!["path2".to_string()]]);

        merge_row_into(&mut target, source);

        // final_is_merge = false (source.is_merge is None)
        assert!(target.merge_paths.is_none());
    }

    #[test]
    fn test_merge_paths_kept_when_both_merge() {
        let mut target = make_base_row("1");
        target.is_merge = Some(true);
        target.merge_paths = Some(vec![vec!["path1".to_string()]]);

        let mut source = make_base_row("1");
        source.is_merge = Some(true);
        source.merge_paths = None; // Source has no paths

        merge_row_into(&mut target, source);

        // final_is_merge = true, so target's paths are kept
        assert_eq!(target.merge_paths, Some(vec![vec!["path1".to_string()]]));
    }

    #[test]
    fn test_identity_fields_preserved() {
        let target_created = Utc::now();
        let mut target = make_base_row("target-id");
        target.span_id = "target-span".to_string();
        target.root_span_id = "target-root".to_string();
        target.span_parents = Some(vec!["parent1".to_string()]);
        target.destination = LogDestination::experiment("target-exp");
        target.org_id = "target-org".to_string();
        target.created = target_created;

        let source_created = Utc::now();
        let mut source = make_base_row("source-id");
        source.span_id = "source-span".to_string();
        source.root_span_id = "source-root".to_string();
        source.span_parents = Some(vec!["parent2".to_string()]);
        source.destination = LogDestination::experiment("source-exp");
        source.org_id = "source-org".to_string();
        source.created = source_created;

        merge_row_into(&mut target, source);

        // Identity fields should be preserved from target
        assert_eq!(target.id, "target-id");
        assert_eq!(target.span_id, "target-span");
        assert_eq!(target.root_span_id, "target-root");
        assert_eq!(target.span_parents, Some(vec!["parent1".to_string()]));
        assert_eq!(target.org_id, "target-org");
        assert_eq!(target.created, target_created);
        // Destination is also an identity field
        match target.destination {
            LogDestination::Experiment { experiment_id } => {
                assert_eq!(experiment_id, "target-exp");
            }
            _ => panic!("Expected Experiment destination"),
        }
    }

    #[test]
    fn test_merge_with_empty_source() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"data": 1}));
        target.scores = Some({
            let mut m = HashMap::new();
            m.insert("score1".to_string(), 0.5);
            m
        });
        target.tags = Some(vec!["tag1".to_string()]);

        let source = make_base_row("1");
        // Source has all None fields

        merge_row_into(&mut target, source);

        // Target should remain unchanged
        assert_eq!(target.input, Some(json!({"data": 1})));
        assert_eq!(target.scores.as_ref().unwrap().len(), 1);
        assert_eq!(target.tags, Some(vec!["tag1".to_string()]));
    }

    #[test]
    fn test_merge_into_empty_target() {
        let mut target = make_base_row("1");
        // Target has all None fields

        let mut source = make_base_row("1");
        source.input = Some(json!({"data": 1}));
        source.scores = Some({
            let mut m = HashMap::new();
            m.insert("score1".to_string(), 0.5);
            m
        });
        source.tags = Some(vec!["tag1".to_string()]);

        merge_row_into(&mut target, source);

        // Target should receive all source fields
        assert_eq!(target.input, Some(json!({"data": 1})));
        assert_eq!(target.scores.as_ref().unwrap().len(), 1);
        assert_eq!(target.tags, Some(vec!["tag1".to_string()]));
    }

    #[test]
    fn test_merge_multiple_merge_paths() {
        let mut target = make_base_row("1");
        target.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("path1".to_string(), json!({"a": 1, "b": 2}));
            m.insert("path2".to_string(), json!({"c": 3, "d": 4}));
            m.insert("path3".to_string(), json!({"e": 5}));
            m
        });

        let mut source = make_base_row("1");
        source.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("path1".to_string(), json!({"a": 10}));
            m.insert("path2".to_string(), json!({"c": 30}));
            m.insert("path3".to_string(), json!({"f": 6}));
            m
        });
        // Top-level-prefixed stop paths for metadata
        source.merge_paths = Some(vec![
            vec!["metadata".to_string(), "path1".to_string()],
            vec!["metadata".to_string(), "path2".to_string()],
        ]);

        merge_row_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        // path1 and path2 should be replaced
        assert_eq!(metadata.get("path1").unwrap(), &json!({"a": 10}));
        assert_eq!(metadata.get("path2").unwrap(), &json!({"c": 30}));
        // path3 should be deep merged (not a stop path)
        assert_eq!(metadata.get("path3").unwrap(), &json!({"e": 5, "f": 6}));
    }

    #[test]
    fn test_merge_paths_apply_to_input_field() {
        let mut target = make_base_row("1");
        target.input = Some(json!({"config": {"model": "gpt-4", "temp": 0.7}, "other": {"a": 1}}));

        let mut source = make_base_row("1");
        source.input = Some(json!({"config": {"model": "gpt-3.5"}, "other": {"b": 2}}));
        // ["input", "config"] means: within input, stop deep merge at "config"
        source.merge_paths = Some(vec![vec!["input".to_string(), "config".to_string()]]);

        merge_row_into(&mut target, source);

        let input = target.input.unwrap();
        // config should be replaced entirely (stop path)
        assert_eq!(input["config"], json!({"model": "gpt-3.5"}));
        // other should be deep merged (not a stop path)
        assert_eq!(input["other"], json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_merge_paths_apply_to_output_field() {
        let mut target = make_base_row("1");
        target.output = Some(json!({"result": {"x": 1, "y": 2}, "meta": {"count": 1}}));

        let mut source = make_base_row("1");
        source.output = Some(json!({"result": {"x": 10}, "meta": {"extra": 99}}));
        // ["output", "result"] means: within output, stop deep merge at "result"
        source.merge_paths = Some(vec![vec!["output".to_string(), "result".to_string()]]);

        merge_row_into(&mut target, source);

        let output = target.output.unwrap();
        // result should be replaced entirely (stop path)
        assert_eq!(output["result"], json!({"x": 10}));
        // meta should be deep merged (not a stop path)
        assert_eq!(output["meta"], json!({"count": 1, "extra": 99}));
    }

    #[test]
    fn test_merge_paths_require_toplevel_prefix_for_metadata() {
        let mut target = make_base_row("1");
        target.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("config".to_string(), json!({"model": "gpt-4", "temp": 0.7}));
            m.insert("other".to_string(), json!({"a": 1}));
            m
        });

        let mut source = make_base_row("1");
        source.metadata = Some({
            let mut m = serde_json::Map::new();
            m.insert("config".to_string(), json!({"model": "gpt-3.5"}));
            m.insert("other".to_string(), json!({"b": 2}));
            m
        });
        // Correctly prefixed path: ["metadata", "config"] stops at metadata.config
        source.merge_paths = Some(vec![vec!["metadata".to_string(), "config".to_string()]]);

        merge_row_into(&mut target, source);

        let metadata = target.metadata.unwrap();
        // config should be replaced (stop path with correct top-level prefix)
        assert_eq!(metadata["config"], json!({"model": "gpt-3.5"}));
        // other should be deep merged (not a stop path)
        assert_eq!(metadata["other"], json!({"a": 1, "b": 2}));
    }

    #[test]
    fn test_merge_row_reference_impl_matches() {
        // Verify that merge_row_into produces the same result as independently
        // deep-merging each JSON field — the "reference implementation" check.
        let mut target = make_base_row("1");
        target.input = Some(json!({"a": {"nested": 1}, "b": 2}));
        target.output = Some(json!({"x": {"y": 10}}));
        target.expected = Some(json!({"exp": {"a": 1}}));
        target.error = Some(json!({"msg": "error", "code": 500}));
        target.context = Some(json!({"ctx": {"key": "val"}}));

        let source_input = json!({"a": {"extra": 2}, "c": 3});
        let source_output = json!({"x": {"z": 20}, "w": 5});
        let source_expected = json!({"exp": {"b": 2}});
        let source_error = json!({"stack": "trace"});
        let source_context = json!({"ctx": {"other": "val2"}});

        // Build reference by deep-merging each field independently
        let mut ref_input = target.input.clone().unwrap();
        deep_merge(&mut ref_input, &source_input);
        let mut ref_output = target.output.clone().unwrap();
        deep_merge(&mut ref_output, &source_output);
        let mut ref_expected = target.expected.clone().unwrap();
        deep_merge(&mut ref_expected, &source_expected);
        let mut ref_error = target.error.clone().unwrap();
        deep_merge(&mut ref_error, &source_error);
        let mut ref_context = target.context.clone().unwrap();
        deep_merge(&mut ref_context, &source_context);

        let mut source = make_base_row("1");
        source.input = Some(source_input);
        source.output = Some(source_output);
        source.expected = Some(source_expected);
        source.error = Some(source_error);
        source.context = Some(source_context);

        merge_row_into(&mut target, source);

        assert_eq!(target.input.as_ref().unwrap(), &ref_input, "input mismatch");
        assert_eq!(
            target.output.as_ref().unwrap(),
            &ref_output,
            "output mismatch"
        );
        assert_eq!(
            target.expected.as_ref().unwrap(),
            &ref_expected,
            "expected mismatch"
        );
        assert_eq!(target.error.as_ref().unwrap(), &ref_error, "error mismatch");
        assert_eq!(
            target.context.as_ref().unwrap(),
            &ref_context,
            "context mismatch"
        );
    }
}
