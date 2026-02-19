use crate::json_merge::deep_merge;
use crate::types::{Logs3Row, SpanAttributes};
use serde_json::Map;
use std::collections::HashMap;

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
/// TODO: support `_merge_paths` (TS SDK's `MERGE_PATHS_FIELD`).
/// The TypeScript SDK's `merge_row_batch.ts` lists `_merge_paths` as a skip field
/// with a matching TODO. Once the TS SDK wires it end-to-end, Rust should follow suit.
///
/// Note: `target.is_merge` is intentionally not modified here.  The TypeScript SDK's
/// `mergeRowBatch` deletes `_is_merge` from the merged result when the existing row
/// didn't originally have it (`preserveNoMerge` logic).  In this SDK, `is_merge` is
/// always `Some(true)` or `None` — never `Some(false)` — so the effective behavior is
/// identical: if target had `None` it keeps `None`; if it had `Some(true)` it keeps
/// `Some(true)`.  Any future code that explicitly sets `is_merge = Some(false)` would
/// need to re-evaluate this logic to stay aligned with TypeScript.
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
}
