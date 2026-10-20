//! Pure logs3 encoding and upload decisions. No transport or retry policy.
use super::batching::{BatchLimits, Batcher, Partition};
use crate::types::{Logs3OverflowInputRow, Logs3OverflowInputRowMeta, LOGS_API_VERSION};
use bytes::Bytes;
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(crate) struct EncodedRow {
    data: Bytes,
    overflow: Logs3OverflowInputRow,
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
        })
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
        Self::encode(
            row,
            Logs3OverflowInputRow {
                object_ids,
                is_delete: row.get("_object_delete").and_then(Value::as_bool),
                input_row: Logs3OverflowInputRowMeta { byte_size: 0 },
            },
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EncodedBatch {
    pub data: Bytes,
    pub overflow_rows: Vec<Logs3OverflowInputRow>,
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
            overflow_rows: rows.iter().map(|row| row.overflow.clone()).collect(),
            rows,
        }
    }

    pub fn partition(rows: Vec<EncodedRow>, max_items: usize, max_bytes: usize) -> Vec<Self> {
        let container_bytes = Self::encode(Vec::new()).data.len();
        Batcher::partition(
            rows,
            BatchLimits {
                max_items,
                max_bytes,
                container_bytes,
                separator_bytes: 1,
            },
            |row| row.data.len(),
        )
        .into_iter()
        .map(|part| {
            Self::encode(match part {
                Partition::Batch(rows) => rows,
                // Keep the oversized row as a singleton for the planner to handle.
                Partition::Oversized(row) => vec![row],
            })
        })
        .collect()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    fn split(mut self) -> (Self, Self) {
        let right = self.rows.split_off(self.rows.len() / 2);
        (Self::encode(self.rows), Self::encode(right))
    }
}

pub(crate) enum UploadPlan {
    Direct(EncodedBatch),
    Overflow(EncodedBatch),
    Split(EncodedBatch, EncodedBatch),
}

#[derive(Clone, Copy)]
pub(crate) struct UploadPlanner {
    pub max_request_bytes: usize,
    pub overflow: bool,
}

impl UploadPlanner {
    pub fn plan(&self, batch: EncodedBatch) -> Result<UploadPlan, crate::error::BraintrustError> {
        self.decide(batch, false)
    }

    /// A rejected direct request must change shape, never retry identical bytes.
    pub fn rejected(
        &self,
        batch: EncodedBatch,
    ) -> Result<UploadPlan, crate::error::BraintrustError> {
        self.decide(batch, true)
    }

    fn decide(
        &self,
        batch: EncodedBatch,
        rejected: bool,
    ) -> Result<UploadPlan, crate::error::BraintrustError> {
        if !rejected && batch.data.len() <= self.max_request_bytes {
            Ok(UploadPlan::Direct(batch))
        } else if self.overflow {
            Ok(UploadPlan::Overflow(batch))
        } else if batch.len() > 1 {
            let (left, right) = batch.split();
            Ok(UploadPlan::Split(left, right))
        } else {
            Err(crate::error::BraintrustError::Api {
                status: 413,
                message: "single row exceeds server payload limit and overflow is unavailable"
                    .into(),
            })
        }
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
                .zip(batch.overflow_rows)
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
    fn planner_handles_oversized_and_rejected_batches_without_reordering() {
        let batch = EncodedBatch::encode(rows());
        let planner = UploadPlanner {
            max_request_bytes: batch.data.len(),
            overflow: false,
        };
        assert!(matches!(
            planner.plan(batch.clone()).unwrap(),
            UploadPlan::Direct(_)
        ));
        let UploadPlan::Split(left, right) = planner.rejected(batch).unwrap() else {
            panic!("expected split")
        };
        let ids: Vec<_> = [left.clone(), right]
            .iter()
            .flat_map(|batch| {
                serde_json::from_slice::<Value>(&batch.data).unwrap()["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["id"].clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(ids, vec![json!("é\\\""), json!("second"), json!("third")]);
        assert!(planner.rejected(left.clone()).is_err());
        assert!(matches!(
            UploadPlanner {
                overflow: true,
                ..planner
            }
            .rejected(left)
            .unwrap(),
            UploadPlan::Overflow(_)
        ));
        assert!(matches!(
            UploadPlanner {
                max_request_bytes: 1,
                overflow: true
            }
            .plan(EncodedBatch::encode(rows()))
            .unwrap(),
            UploadPlan::Overflow(_)
        ));
    }
}
