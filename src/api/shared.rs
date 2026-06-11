use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Request body used by event-fetch endpoints.
///
/// This shape is shared by dataset, experiment, and project-log fetch
/// endpoints. Construct it with [`FetchEventsRequest::default()`], then set
/// the fields to send.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct FetchEventsRequest {
    /// Maximum number of events to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Cursor returned by a previous fetch response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Upper transaction id bound for the fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_xact_id: Option<String>,
    /// Upper root span id bound for the fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_root_span_id: Option<String>,
    /// Object version to fetch from when supported by the endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// A record within a dataset.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct DatasetRecord {
    /// Unique record ID.
    id: String,
    /// Transaction ID used for dataset versioning.
    #[serde(default, rename = "_xact_id", skip_serializing_if = "Option::is_none")]
    xact_id: Option<String>,
    /// ISO 8601 creation timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    /// Input data for the dataset record.
    // TODO: Harden this Value-backed dataset input if dataset schemas become statically known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    /// Expected output for evaluation.
    // TODO: Harden this Value-backed expected value if dataset schemas become statically known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected: Option<Value>,
    /// Key-value metadata.
    // TODO: Harden this Value-backed metadata map if backend metadata becomes schema-constrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    /// String tags for filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    /// Additional backend fields attached to dataset records.
    // TODO: Harden this Value-backed extension map to concrete dataset record fields as they stabilize.
    #[serde(flatten, default, skip_serializing_if = "Map::is_empty")]
    extra: Map<String, Value>,
}

impl DatasetRecord {
    api_getters! {
        str id;
        option_str xact_id;
        option_str created;
        option_ref input: Value;
        option_ref expected: Value;
        option_map metadata;
        option_slice tags: String;
        map extra;
    }
}

/// Event row returned by dataset fetch endpoints.
///
/// Dataset events include the shared dataset record fields plus fetch-specific
/// event identifiers. Unknown backend fields are preserved in
/// [`DatasetEvent::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DatasetEvent {
    #[serde(flatten)]
    record: DatasetRecord,
    project_id: String,
    dataset_id: String,
    span_id: String,
    root_span_id: String,
    #[serde(
        default,
        rename = "_pagination_key",
        skip_serializing_if = "Option::is_none"
    )]
    pagination_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    is_root: Option<bool>,
}

impl DatasetEvent {
    /// Returns the shared dataset record fields for this event.
    pub fn record(&self) -> &DatasetRecord {
        &self.record
    }

    /// Returns the `id` field.
    pub fn id(&self) -> &str {
        self.record.id()
    }

    /// Returns the optional `xact_id` field.
    pub fn xact_id(&self) -> Option<&str> {
        self.record.xact_id()
    }

    /// Returns the optional `created` field.
    pub fn created(&self) -> Option<&str> {
        self.record.created()
    }

    /// Returns the optional `input` field.
    pub fn input(&self) -> Option<&Value> {
        self.record.input()
    }

    /// Returns the optional `expected` field.
    pub fn expected(&self) -> Option<&Value> {
        self.record.expected()
    }

    /// Returns the optional `metadata` object.
    pub fn metadata(&self) -> Option<&Map<String, Value>> {
        self.record.metadata()
    }

    /// Returns the optional `tags` collection.
    pub fn tags(&self) -> Option<&[String]> {
        self.record.tags()
    }

    /// Returns the `extra` object.
    pub fn extra(&self) -> &Map<String, Value> {
        self.record.extra()
    }

    api_getters! {
        str project_id;
        str dataset_id;
        str span_id;
        str root_span_id;
        option_str pagination_key;
        option_ref is_root: bool;
    }
}
