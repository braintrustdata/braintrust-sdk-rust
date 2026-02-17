use super::config::LogQueueConfig;
use crate::types::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3Request, Logs3Row, LOGS_API_VERSION,
    OBJECT_ID_KEYS,
};
use serde_json::Value;
use tracing::warn;

/// Generic batching function - matches TypeScript SDK's batchItems().
///
/// Splits `items` into batches respecting `max_items` and `max_bytes` limits.
/// `byte_size` is called once per item to determine its size.
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
        if !batch.is_empty() && (batch.len() >= max_items || batch_bytes + item_size > max_bytes) {
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
pub(crate) fn batch_and_serialize_rows(
    rows: Vec<Logs3Row>,
    config: &LogQueueConfig,
) -> Vec<SerializedBatch> {
    let mut batches = Vec::new();
    let mut current_batch: Vec<Logs3Row> = Vec::new();
    let mut current_overflow: Vec<Logs3OverflowInputRow> = Vec::new();
    let mut current_size = 0;

    for row in rows {
        // Serialize row to estimate size and build overflow metadata
        let row_json = match serde_json::to_vec(&row) {
            Ok(json) => json,
            Err(e) => {
                warn!(error = %e, "failed to serialize row, skipping");
                continue;
            }
        };
        let row_size = row_json.len();

        // Check if we need to start a new batch
        if current_batch.len() >= config.batch_max_items()
            || (current_size + row_size > config.batch_max_bytes() && !current_batch.is_empty())
        {
            // Serialize current batch
            if let Ok(batch) = serialize_batch(&current_batch, current_overflow) {
                batches.push(batch);
            }
            current_batch.clear();
            current_overflow = Vec::new();
            current_size = 0;
        }

        // Build overflow metadata for this row
        let overflow_row = build_overflow_row(&row, row_size);

        current_size += row_size;
        current_batch.push(row);
        current_overflow.push(overflow_row);
    }

    // Serialize final batch
    if !current_batch.is_empty() {
        if let Ok(batch) = serialize_batch(&current_batch, current_overflow) {
            batches.push(batch);
        }
    }

    batches
}

/// Serialize a batch of rows into Logs3Request format.
fn serialize_batch(
    rows: &[Logs3Row],
    overflow_rows: Vec<Logs3OverflowInputRow>,
) -> Result<SerializedBatch, anyhow::Error> {
    let request = Logs3Request {
        rows: rows.to_vec(),
        api_version: LOGS_API_VERSION,
    };
    let data = serde_json::to_vec(&request)
        .map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))?;
    Ok(SerializedBatch {
        data,
        overflow_rows,
    })
}

/// Build overflow metadata for a single row.
///
/// Extracts OBJECT_ID_KEYS from the serialized row (matches TypeScript SDK's
/// `pickLogs3OverflowObjectIds`).
fn build_overflow_row(row: &Logs3Row, byte_size: usize) -> Logs3OverflowInputRow {
    // Serialize the row to extract object ID keys
    let object_ids = match serde_json::to_value(row) {
        Ok(Value::Object(map)) => map
            .into_iter()
            .filter(|(k, _)| OBJECT_ID_KEYS.contains(&k.as_str()))
            .collect(),
        _ => serde_json::Map::new(),
    };

    Logs3OverflowInputRow {
        object_ids,
        is_delete: None,
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

        let batches = batch_and_serialize_rows(rows, &config);

        // Should create multiple batches due to byte limit
        assert!(batches.len() > 1, "Should split into multiple batches");

        // Each batch should be valid JSON with overflow metadata
        for batch in &batches {
            let parsed: serde_json::Value = serde_json::from_slice(&batch.data).unwrap();
            assert!(parsed.get("rows").is_some());
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
        let batches = batch_and_serialize_rows(rows, &config);

        // 5 rows / 2 per batch = 3 batches
        assert_eq!(batches.len(), 3);
    }

    #[test]
    fn test_overflow_metadata_contains_object_ids() {
        let config = LogQueueConfig::default();
        let rows = vec![make_row("row-1", 10)];
        let batches = batch_and_serialize_rows(rows, &config);

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
        let batches = batch_and_serialize_rows(rows, &config);

        let overflow_row = &batches[0].overflow_rows[0];
        assert!(overflow_row.input_row.byte_size > 0);
    }
}
