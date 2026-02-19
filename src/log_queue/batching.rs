use super::config::LogQueueConfig;
use crate::types::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3Row, LOGS_API_VERSION, OBJECT_ID_KEYS,
};
use tracing::warn;

/// Generic batching function — matches TypeScript SDK's `batchItems()`.
///
/// Splits `items` into batches respecting `max_items` and `max_bytes` limits.
/// `byte_size` is called once per item to determine its size.
///
/// Flush condition (matches TypeScript): a new batch is started when the current
/// batch is non-empty AND (`batch.len() >= max_items` OR `batch_bytes + item_size >= max_bytes`).
/// This means a single item that exceeds `max_bytes` on its own is always placed in its own batch.
///
/// Note: the byte comparison uses `>=` (not `>`), so two items that sum to exactly `max_bytes`
/// are split into separate batches. This matches the TypeScript SDK's strict `<` check in the
/// negated condition (`!(size < max && len < max)`). The test
/// `test_byte_boundary_flushes_at_exact_limit` verifies this invariant explicitly.
pub(crate) fn batch_items<T>(
    items: Vec<T>,
    max_items: usize,
    max_bytes: usize,
    byte_size: impl Fn(&T) -> usize,
) -> Vec<Vec<T>> {
    let max_items = max_items.max(1);
    let max_bytes = max_bytes.max(1);
    let mut output = Vec::new();
    let mut batch: Vec<T> = Vec::new();
    let mut batch_bytes = 0usize;

    for item in items {
        let item_size = byte_size(&item);
        if !batch.is_empty() && (batch.len() >= max_items || batch_bytes + item_size >= max_bytes) {
            output.push(batch);
            batch = Vec::new();
            batch_bytes = 0;
        }
        batch_bytes += item_size;
        batch.push(item);
    }
    if !batch.is_empty() {
        output.push(batch);
    }
    output
}

/// A serialized batch with per-row overflow metadata.
pub(crate) struct SerializedBatch {
    /// The full batch serialized as a Logs3Request JSON payload.
    pub data: Vec<u8>,
    /// Per-row overflow metadata (for `POST logs3/overflow` requests).
    pub overflow_rows: Vec<Logs3OverflowInputRow>,
}

/// Batch rows respecting size limits, then serialize each batch to JSON.
///
/// Each row is serialized once (via `serde_json::to_value` → `to_vec`).
/// The pre-serialized bytes are used for size estimation AND assembled directly
/// into the final payload, avoiding a third re-serialization pass.
///
/// `batch_max_bytes` is the effective byte limit for this flush, which may be
/// lower than `config.batch_max_bytes()` when the server advertises a tighter
/// limit via `GET /version` (callers should use `min(config, server_limit / 2)`).
pub(crate) fn batch_and_serialize_rows(
    rows: Vec<Logs3Row>,
    config: &LogQueueConfig,
    batch_max_bytes: usize,
) -> Vec<SerializedBatch> {
    // Serialize each row once: to_value for object-ID extraction, to_vec for the bytes.
    let prepared: Vec<(Vec<u8>, Logs3OverflowInputRow)> = rows
        .into_iter()
        .filter_map(|row| {
            let value = match serde_json::to_value(&row) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "failed to serialize row, skipping");
                    return None;
                }
            };
            let row_bytes = match serde_json::to_vec(&value) {
                Ok(b) => b,
                Err(e) => {
                    warn!(error = %e, "failed to serialize row value, skipping");
                    return None;
                }
            };
            let overflow_row = build_overflow_row_from_value(&value, row_bytes.len());
            Some((row_bytes, overflow_row))
        })
        .collect();

    // Split into batches using the shared batch_items logic.
    let batches = batch_items(
        prepared,
        config.batch_max_items(),
        batch_max_bytes,
        |(row_bytes, _)| row_bytes.len(),
    );

    // Assemble each batch by concatenating the pre-serialized row bytes.
    batches
        .into_iter()
        .filter_map(|batch| {
            let (row_bytes, overflow_rows): (Vec<_>, Vec<_>) = batch.into_iter().unzip();
            match assemble_logs3_request(&row_bytes, overflow_rows) {
                Ok(b) => Some(b),
                Err(e) => {
                    warn!(error = %e, "failed to assemble batch");
                    None
                }
            }
        })
        .collect()
}

/// Assemble a `Logs3Request` payload by concatenating pre-serialized row bytes.
///
/// Produces `{"rows":[<row1>,<row2>,...],"api_version":<N>}` without re-serializing rows.
fn assemble_logs3_request(
    row_bytes: &[Vec<u8>],
    overflow_rows: Vec<Logs3OverflowInputRow>,
) -> Result<SerializedBatch, anyhow::Error> {
    let mut data = Vec::new();
    data.extend_from_slice(b"{\"rows\":[");
    for (i, row) in row_bytes.iter().enumerate() {
        if i > 0 {
            data.push(b',');
        }
        data.extend_from_slice(row);
    }
    data.extend_from_slice(b"],\"api_version\":");
    data.extend_from_slice(LOGS_API_VERSION.to_string().as_bytes());
    data.push(b'}');
    Ok(SerializedBatch {
        data,
        overflow_rows,
    })
}

/// Build overflow metadata for a single row from its already-deserialized JSON value.
fn build_overflow_row_from_value(
    value: &serde_json::Value,
    byte_size: usize,
) -> Logs3OverflowInputRow {
    let map = value.as_object();
    let object_ids = match map {
        Some(m) => m
            .iter()
            .filter(|(k, _)| OBJECT_ID_KEYS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        None => serde_json::Map::new(),
    };
    // Mirror TypeScript SDK: is_delete is set when _object_delete === true.
    let is_delete = map
        .and_then(|m| m.get("_object_delete"))
        .and_then(|v| v.as_bool())
        .filter(|&b| b);
    Logs3OverflowInputRow {
        object_ids,
        is_delete,
        input_row: Logs3OverflowInputRowMeta { byte_size },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::config::LogQueueConfig;
    use crate::types::LogDestination;
    use chrono::Utc;

    fn make_row(id: &str, input_size: usize) -> Logs3Row {
        Logs3Row {
            id: id.to_string(),
            is_merge: None,
            span_id: id.to_string(),
            root_span_id: id.to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org".to_string(),
            org_name: Some("org-name".to_string()),
            input: Some(serde_json::json!({"data": "x".repeat(input_size)})),
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            created: Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: None,
        }
    }

    #[test]
    fn test_batch_by_byte_size() {
        let config = LogQueueConfig::builder()
            .batch_max_items(1000) // High item limit
            .batch_max_bytes(100) // Low byte limit to force splitting
            .build();

        // Create rows that will exceed byte limit
        let rows: Vec<Logs3Row> = (0..5).map(|i| make_row(&format!("row-{i}"), 50)).collect();

        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes());

        // Should create multiple batches due to byte limit
        assert!(batches.len() > 1, "Should split into multiple batches");

        // Each batch should be valid JSON with overflow metadata
        for batch in &batches {
            let parsed: serde_json::Value = serde_json::from_slice(&batch.data).unwrap();
            assert!(parsed.get("rows").is_some());
            assert_eq!(parsed["api_version"], LOGS_API_VERSION);
            assert!(!batch.overflow_rows.is_empty());
        }
    }

    #[test]
    fn test_batch_by_item_count() {
        let config = LogQueueConfig::builder()
            .batch_max_items(2)
            .batch_max_bytes(10 * 1024 * 1024) // High byte limit
            .build();

        let rows: Vec<Logs3Row> = (0..5).map(|i| make_row(&format!("row-{i}"), 10)).collect();
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes());

        // 5 rows / 2 per batch = 3 batches
        assert_eq!(batches.len(), 3);
    }

    #[test]
    fn test_overflow_metadata_contains_object_ids() {
        let config = LogQueueConfig::default();
        let rows = vec![make_row("row-1", 10)];
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes());

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].overflow_rows.len(), 1);

        // Should have experiment_id in object_ids
        let overflow_row = &batches[0].overflow_rows[0];
        assert!(overflow_row.object_ids.contains_key("experiment_id"));
        assert!(!overflow_row.object_ids.contains_key("project_id"));
    }

    #[test]
    fn test_overflow_metadata_byte_size() {
        let config = LogQueueConfig::default();
        let rows = vec![make_row("row-1", 10)];
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes());

        let overflow_row = &batches[0].overflow_rows[0];
        assert!(overflow_row.input_row.byte_size > 0);
    }

    #[test]
    fn test_byte_boundary_flushes_at_exact_limit() {
        // A batch should flush when adding the next item would reach the limit (>=),
        // not just exceed it (>). With max_bytes=100, two items of 50 bytes each
        // should NOT be in the same batch (50 + 50 >= 100).
        let items: Vec<Vec<u8>> = vec![vec![0u8; 50], vec![0u8; 50]];
        let batches = batch_items(items, 1000, 100, |b| b.len());

        assert_eq!(
            batches.len(),
            2,
            "Items summing to exactly max_bytes should be split"
        );
    }

    #[test]
    fn test_overflow_metadata_is_delete_set_from_object_delete_field() {
        let config = LogQueueConfig::default();
        let mut row = make_row("row-del", 10);
        row.object_delete = Some(true);
        let batches = batch_and_serialize_rows(vec![row], &config, config.batch_max_bytes());

        assert_eq!(batches.len(), 1);
        let overflow_row = &batches[0].overflow_rows[0];
        assert_eq!(
            overflow_row.is_delete,
            Some(true),
            "is_delete should be true when _object_delete=true"
        );
    }

    #[test]
    fn test_overflow_metadata_is_delete_none_when_not_set() {
        let config = LogQueueConfig::default();
        let row = make_row("row-1", 10); // object_delete is None
        let batches = batch_and_serialize_rows(vec![row], &config, config.batch_max_bytes());

        let overflow_row = &batches[0].overflow_rows[0];
        assert!(
            overflow_row.is_delete.is_none(),
            "is_delete should be None when _object_delete is not set"
        );
    }

    #[test]
    fn test_single_oversized_item_gets_its_own_batch() {
        // An item larger than max_bytes should still be placed in a batch alone.
        let items: Vec<Vec<u8>> = vec![vec![0u8; 200], vec![0u8; 10]];
        let batches = batch_items(items, 1000, 100, |b| b.len());

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 1); // oversized item alone
        assert_eq!(batches[1].len(), 1);
    }
}
