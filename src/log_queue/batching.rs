use super::config::LogQueueConfig;
use crate::types::{Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3Row};
use tracing::warn;

pub(crate) use crate::internal::ingest::EncodedBatch as SerializedBatch;
use crate::internal::ingest::EncodedRow;

/// Encode rows once, then partition using the complete request size.
pub(crate) fn batch_and_serialize_rows(
    rows: Vec<Logs3Row>,
    config: &LogQueueConfig,
    batch_max_bytes: usize,
) -> Result<Vec<SerializedBatch>, anyhow::Error> {
    let rows = rows
        .into_iter()
        .map(|row| {
            EncodedRow::encode(&row, build_overflow_row_from_logs3row(&row)).map_err(|error| {
                warn!(%error, "failed to serialize row");
                error
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SerializedBatch::partition(
        rows,
        config.batch_max_items(),
        batch_max_bytes,
    ))
}

/// Build overflow metadata directly from a Logs3Row struct.
///
/// Extracts object IDs from the destination and object_delete flag without
/// serializing to an intermediate JSON Value representation.
fn build_overflow_row_from_logs3row(row: &Logs3Row) -> Logs3OverflowInputRow {
    let mut object_ids = serde_json::Map::new();

    // Extract destination fields as object IDs
    if let Some(experiment_id) = row.destination.experiment_id() {
        object_ids.insert(
            "experiment_id".to_string(),
            serde_json::Value::String(experiment_id.to_string()),
        );
    }
    if let Some(dataset_id) = row.destination.dataset_id() {
        object_ids.insert(
            "dataset_id".to_string(),
            serde_json::Value::String(dataset_id.to_string()),
        );
    }
    if let Some(project_id) = row.destination.project_id() {
        object_ids.insert(
            "project_id".to_string(),
            serde_json::Value::String(project_id.to_string()),
        );
    }
    if let Some(prompt_session_id) = row.destination.prompt_session_id() {
        object_ids.insert(
            "prompt_session_id".to_string(),
            serde_json::Value::String(prompt_session_id.to_string()),
        );
    }
    if let Some(log_id) = row.destination.log_id() {
        object_ids.insert(
            "log_id".to_string(),
            serde_json::Value::String(log_id.to_string()),
        );
    }

    Logs3OverflowInputRow {
        object_ids,
        is_delete: row.object_delete.filter(|&b| b),
        input_row: Logs3OverflowInputRowMeta { byte_size: 0 }, // Will be updated after serialization
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log_queue::config::LogQueueConfig;
    use crate::types::LogDestination;
    use crate::types::LOGS_API_VERSION;
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_row(id: &str, input_size: usize) -> Logs3Row {
        Logs3Row {
            id: id.to_string(),
            is_merge: None,
            merge_paths: None,
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
            extra: HashMap::new(),
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

        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes()).unwrap();

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
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes()).unwrap();

        // 5 rows / 2 per batch = 3 batches
        assert_eq!(batches.len(), 3);
    }

    #[test]
    fn test_overflow_metadata_contains_object_ids() {
        let config = LogQueueConfig::default();
        let rows = vec![make_row("row-1", 10)];
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes()).unwrap();

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
        let batches = batch_and_serialize_rows(rows, &config, config.batch_max_bytes()).unwrap();

        let overflow_row = &batches[0].overflow_rows[0];
        assert!(overflow_row.input_row.byte_size > 0);
    }

    #[test]
    fn test_overflow_metadata_is_delete_set_from_object_delete_field() {
        let config = LogQueueConfig::default();
        let mut row = make_row("row-del", 10);
        row.object_delete = Some(true);
        let batches =
            batch_and_serialize_rows(vec![row], &config, config.batch_max_bytes()).unwrap();

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
        let batches =
            batch_and_serialize_rows(vec![row], &config, config.batch_max_bytes()).unwrap();

        let overflow_row = &batches[0].overflow_rows[0];
        assert!(
            overflow_row.is_delete.is_none(),
            "is_delete should be None when _object_delete is not set"
        );
    }
}
