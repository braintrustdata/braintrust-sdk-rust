//! Logs3 encoding and size-limited batching.
use crate::types::{Logs3OverflowInputRow, Logs3OverflowInputRowMeta, LOGS_API_VERSION};
use bytes::Bytes;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(crate) struct EncodedRow {
    data: Bytes,
    overflow: Logs3OverflowInputRow,
    submissions: Vec<usize>,
}

impl EncodedRow {
    pub fn encode(
        row: &impl Serialize,
        mut overflow: Logs3OverflowInputRow,
    ) -> Result<Self, serde_json::Error> {
        let data = serde_json::to_vec(row)?;
        overflow.input_row.byte_size = data.len();
        Ok(Self {
            data: data.into(),
            overflow,
            submissions: Vec::new(),
        })
    }

    pub(crate) fn with_submissions(mut self, submissions: Vec<usize>) -> Self {
        self.submissions = submissions;
        self
    }

    pub fn from_map(row: &Map<String, Value>) -> Result<Self, serde_json::Error> {
        let object_ids = [
            "experiment_id",
            "dataset_id",
            "prompt_session_id",
            "project_id",
            "log_id",
            "function_data",
        ]
        .into_iter()
        .filter_map(|key| row.get(key).map(|value| (key.to_owned(), value.clone())))
        .collect();
        Self::encode(row, {
            let mut value =
                Logs3OverflowInputRow::new(object_ids, Logs3OverflowInputRowMeta::new(0));
            value.is_delete = row.get("_object_delete").and_then(Value::as_bool);
            value
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedBatch {
    pub data: Bytes,
    rows: Vec<EncodedRow>,
}

impl EncodedBatch {
    fn encode(rows: Vec<EncodedRow>) -> Self {
        let mut data = Vec::new();
        data.extend_from_slice(b"{\"rows\":[");
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                data.push(b',');
            }
            data.extend_from_slice(&row.data);
        }
        data.extend_from_slice(format!("],\"api_version\":{LOGS_API_VERSION}}}").as_bytes());
        Self {
            data: data.into(),
            rows,
        }
    }

    pub fn partition(rows: Vec<EncodedRow>, max_items: usize, max_bytes: usize) -> Vec<Self> {
        let container_bytes = Self::encode(Vec::new()).data.len();
        let mut batches = Vec::new();
        let mut batch = Vec::new();
        let mut bytes = container_bytes;
        for row in rows {
            let next = bytes
                .checked_add(usize::from(!batch.is_empty()))
                .and_then(|bytes| bytes.checked_add(row.data.len()));
            if !batch.is_empty()
                && (batch.len() >= max_items.max(1) || next.is_none_or(|size| size > max_bytes))
            {
                batches.push(Self::encode(std::mem::take(&mut batch)));
                bytes = container_bytes;
            }
            // An oversized row remains a singleton for overflow or a clear error.
            bytes = bytes
                .saturating_add(usize::from(!batch.is_empty()))
                .saturating_add(row.data.len());
            batch.push(row);
        }
        if !batch.is_empty() {
            batches.push(Self::encode(batch));
        }
        batches
    }

    /// Partition rows so each batch holds a single destination. The data plane
    /// rejects a whole request when any project in it is rate limited, so mixing
    /// destinations would let one throttled project drop another's rows.
    pub fn partition_by_destination(
        rows: Vec<EncodedRow>,
        max_items: usize,
        max_bytes: usize,
    ) -> Vec<Self> {
        let mut destinations: IndexMap<String, Vec<EncodedRow>> = IndexMap::new();
        for row in rows {
            let destination = serde_json::to_string(&row.overflow.object_ids).unwrap_or_default();
            destinations.entry(destination).or_default().push(row);
        }
        destinations
            .into_values()
            .flat_map(|rows| Self::partition(rows, max_items, max_bytes))
            .collect()
    }

    pub(crate) fn submissions(&self) -> Vec<Vec<usize>> {
        self.rows
            .iter()
            .map(|row| row.submissions.clone())
            .collect()
    }

    pub fn overflow_rows(&self) -> Vec<Logs3OverflowInputRow> {
        self.rows.iter().map(|row| row.overflow.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn split(mut self) -> (Self, Self) {
        let right = self.rows.split_off(self.rows.len() / 2);
        (Self::encode(self.rows), Self::encode(right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows() -> Vec<EncodedRow> {
        ["é\\\"", "second", "third"]
            .into_iter()
            .map(|id| {
                EncodedRow::from_map(
                    json!({"id":id,"dataset_id":"ds","_object_delete":true})
                        .as_object()
                        .unwrap(),
                )
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn encoded_limits_include_envelope_separators_and_utf8() {
        let expected = EncodedBatch::encode(rows());
        let exact = EncodedBatch::partition(rows(), 3, expected.data.len());
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].data, expected.data);
        let smaller = EncodedBatch::partition(rows(), 3, expected.data.len() - 1);
        assert_eq!(smaller.len(), 2);
        for batch in smaller {
            assert!(batch.data.len() < expected.data.len());
            let value: Value = serde_json::from_slice(&batch.data).unwrap();
            for (row, meta) in value["rows"]
                .as_array()
                .unwrap()
                .iter()
                .zip(batch.overflow_rows())
            {
                assert_eq!(
                    meta.input_row.byte_size,
                    serde_json::to_vec(row).unwrap().len()
                );
                assert_eq!(meta.object_ids["dataset_id"], "ds");
                assert_eq!(meta.is_delete, Some(true));
            }
        }
    }

    #[test]
    fn oversized_rows_do_not_reorder_neighboring_batches() {
        let input: Vec<_> = ["a".to_owned(), "x".repeat(200), "b".to_owned()]
            .iter()
            .map(|id| EncodedRow::from_map(json!({"id": id}).as_object().unwrap()).unwrap())
            .collect();
        let batches = EncodedBatch::partition(input.clone(), 100, 100);
        assert_eq!(batches.len(), 3);
        for (batch, row) in batches.into_iter().zip(input) {
            assert_eq!(batch.len(), 1);
            let value: Value = serde_json::from_slice(&batch.data).unwrap();
            assert_eq!(
                value["rows"][0],
                serde_json::from_slice::<Value>(&row.data).unwrap()
            );
        }
    }

    #[test]
    fn oversized_rows_stay_in_order_and_empty_input_has_no_batches() {
        assert!(EncodedBatch::partition(vec![], 0, 0).is_empty());
        let input = rows();
        let batches = EncodedBatch::partition(input.clone(), 0, 0);
        assert_eq!(batches.len(), input.len());
        for (batch, row) in batches.iter().zip(input) {
            assert_eq!(batch.len(), 1);
            let value: Value = serde_json::from_slice(&batch.data).unwrap();
            assert_eq!(
                value["rows"][0],
                serde_json::from_slice::<Value>(&row.data).unwrap()
            );
        }
    }

    #[test]
    fn partition_by_destination_keeps_each_batch_to_one_destination_in_order() {
        let row = |id: &str, project: &str| {
            EncodedRow::from_map(
                json!({"id": id, "project_id": project, "log_id": "g"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap()
        };
        let input = vec![
            row("a1", "a"),
            row("b1", "b"),
            row("a2", "a"),
            row("a3", "a"),
            row("b2", "b"),
        ];
        let batches = EncodedBatch::partition_by_destination(input, 2, usize::MAX);
        let ids: Vec<Vec<String>> = batches
            .iter()
            .map(|batch| {
                let value: Value = serde_json::from_slice(&batch.data).unwrap();
                value["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["id"].as_str().unwrap().to_owned())
                    .collect()
            })
            .collect();
        assert_eq!(ids, [vec!["a1", "a2"], vec!["a3"], vec!["b1", "b2"]]);
    }
}
