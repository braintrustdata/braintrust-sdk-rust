//! Dataset registration, row mutation, and summary endpoint contracts.
use super::{Client, Error, Plane, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body for dataset registration.
#[derive(Debug, Clone, Serialize)]
pub struct DatasetRegisterRequest {
    pub project_name: String,
    pub org_name: String,
    pub dataset_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// Response from dataset registration.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetRegisterResponse {
    pub project: DatasetProjectInfo,
    pub dataset: DatasetInfo,
}

/// Project info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetProjectInfo {
    pub id: String,
    pub name: String,
}

/// Dataset info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetInfo {
    pub id: String,
    pub name: String,
}

/// Fields stored in a dataset record. Absent fields are omitted from the wire.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DatasetFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

/// A dataset row mutation, independent of tracing/span payloads.
#[derive(Debug, Clone)]
pub enum DatasetWrite {
    Insert {
        dataset_id: String,
        id: String,
        fields: DatasetFields,
    },
    /// Merge only the supplied fields into an existing record.
    Update {
        dataset_id: String,
        id: String,
        fields: DatasetFields,
    },
    Delete {
        dataset_id: String,
        id: String,
    },
}

impl Serialize for DatasetWrite {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Row<'a> {
            dataset_id: &'a str,
            id: &'a str,
            #[serde(flatten)]
            fields: Option<&'a DatasetFields>,
            #[serde(rename = "_is_merge", skip_serializing_if = "Option::is_none")]
            is_merge: Option<bool>,
            #[serde(rename = "_object_delete", skip_serializing_if = "Option::is_none")]
            is_delete: Option<bool>,
        }
        let (dataset_id, id, fields, is_merge, is_delete) = match self {
            Self::Insert {
                dataset_id,
                id,
                fields,
            } => (dataset_id, id, Some(fields), None, None),
            Self::Update {
                dataset_id,
                id,
                fields,
            } => (dataset_id, id, Some(fields), Some(true), None),
            Self::Delete { dataset_id, id } => (dataset_id, id, None, None, Some(true)),
        };
        Row {
            dataset_id,
            id,
            fields,
            is_merge,
            is_delete,
        }
        .serialize(serializer)
    }
}

/// Response from dataset summary API.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetSummaryResponse {
    pub project_name: String,
    pub dataset_name: String,
    pub project_id: String,
    pub dataset_id: String,
    pub project_url: Option<String>,
    pub dataset_url: Option<String>,
    pub data_summary: Option<DataSummaryStats>,
}

/// Data summary statistics.
#[derive(Debug, Clone, Deserialize)]
pub struct DataSummaryStats {
    pub total_records: Option<u64>,
}

#[derive(Clone)]
pub struct Datasets {
    pub(crate) client: Client,
}
impl Datasets {
    /// Register one dataset on the control plane.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn register(
        &self,
        request: DatasetRegisterRequest,
    ) -> crate::Completion<DatasetRegisterResponse, Error> {
        self.client
            .submit(move |transport| async move { transport.register_dataset(request).await })
    }
    /// Submit exactly these dataset mutations once, without buffering or splitting.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn write(&self, rows: &[DatasetWrite]) -> crate::Completion<(), Error> {
        self.client.submit(move |transport| {
            let rows = rows.to_vec();
            async move { transport.write_dataset(&rows).await }
        })
    }
    /// Fetch one dataset summary.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn summarize(&self, dataset_id: &str) -> crate::Completion<DatasetSummaryResponse, Error> {
        self.client.submit(move |transport| {
            let dataset_id = dataset_id.to_owned();
            async move { transport.summarize_dataset(&dataset_id).await }
        })
    }
}

impl Transport {
    /// Register once on the control plane; no resolution cache or retry.
    pub(crate) async fn register_dataset(
        &self,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse, Error> {
        self.post_json(Plane::Control, "api/dataset/register", &request)
            .await
    }
    /// Send exactly one typed dataset mutation request on the data plane.
    /// No retry, buffering, splitting, or overflow upload is performed.
    pub(crate) async fn write_dataset(&self, rows: &[DatasetWrite]) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Request<'a> {
            rows: &'a [DatasetWrite],
            api_version: u8,
        }
        self.post_empty(
            Plane::Data,
            "logs3",
            &Request {
                rows,
                api_version: crate::types::LOGS_API_VERSION,
            },
        )
        .await
    }

    pub(crate) async fn summarize_dataset(
        &self,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse, Error> {
        self.get_json(
            Plane::Data,
            "dataset-summary",
            &[("dataset_id", dataset_id)],
        )
        .await
    }
}
