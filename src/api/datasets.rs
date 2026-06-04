//! Dataset resource endpoints.
//!
//! This module exposes the low-level `/v1/dataset` API category, including
//! dataset metadata, event fetch, and summary responses. Higher-level dataset
//! builders and insert flows live outside this module.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};
use crate::api::FetchEventsRequest;

pub use crate::api::shared::{DatasetEvent, DatasetRecord};

/// Client for dataset API operations.
///
/// Create this through [`crate::ApiClient::datasets()`].
#[derive(Clone, Debug)]
pub struct DatasetsClient {
    api: ApiClient,
}

impl DatasetsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    api_endpoint_methods! {
        list(list, ListDatasetsRequest, ListDatasetsResponse, "v1/dataset", "dataset list response");
        get(get, dataset_id, Dataset, "v1/dataset", "dataset response");
        create(create, CreateDatasetRequest, Dataset, "v1/dataset", "dataset create response");
        post_child(fetch, dataset_id, FetchEventsRequest, FetchDatasetEventsResponse, "v1/dataset", "fetch", "dataset fetch response");
        get_child(summarize, dataset_id, SummarizeDatasetRequest, SummarizeDatasetResponse, "v1/dataset", "summarize", "dataset summarize response");
    }

    /// Deletes one dataset by id.
    pub async fn delete(&self, dataset_id: &str) -> crate::Result<DeleteDatasetResponse> {
        self.api
            .delete_json(
                &format!("v1/dataset/{}", path_segment(dataset_id)),
                "dataset delete response",
            )
            .await
    }
}

/// Request body for creating a dataset.
///
/// Construct with [`CreateDatasetRequest::default()`], then set the fields to send.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct CreateDatasetRequest {
    /// Project id that owns the dataset.
    pub project_id: String,
    /// Dataset name.
    pub name: String,
    /// Optional dataset description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional dataset tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Optional dataset metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    /// Optional organization name used to disambiguate dataset creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Query parameters for listing datasets.
///
/// Optional fields are omitted from the query string. `ids` is encoded as
/// repeated `ids` query parameters.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListDatasetsRequest {
    /// Maximum number of datasets to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Pagination cursor for fetching records after a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    /// Pagination cursor for fetching records before a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    /// Dataset ids to include.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// Dataset name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_name: Option<String>,
    /// Project name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// Project id filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Organization name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Query parameters for summarizing a dataset.
///
/// Set [`SummarizeDatasetRequest::summarize_data`] to `Some(true)` to request
/// data summary information when the backend supports it.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct SummarizeDatasetRequest {
    /// Whether to include data summary information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarize_data: Option<bool>,
}

/// Response returned by [`DatasetsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListDatasetsResponse {
    objects: Vec<Dataset>,
}

impl ListDatasetsResponse {
    api_getters! {
        slice objects: Dataset;
    }
}

/// Response returned by [`DatasetsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteDatasetResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl DeleteDatasetResponse {
    api_getters! {
        option_str id;
        option_str deleted_at;
    }
}

/// Response returned by [`DatasetsClient::fetch()`].
///
/// Contains a page of dataset events and an optional cursor for the next page.
#[derive(Debug, Clone, Deserialize)]
pub struct FetchDatasetEventsResponse {
    events: Vec<DatasetEvent>,
    #[serde(default)]
    cursor: Option<String>,
}

impl FetchDatasetEventsResponse {
    api_getters! {
        slice events: DatasetEvent;
        option_str cursor;
    }
}

/// Response returned by [`DatasetsClient::summarize()`].
///
/// Unknown backend fields are preserved in [`SummarizeDatasetResponse::extra()`].
#[derive(Debug, Clone, Deserialize)]
pub struct SummarizeDatasetResponse {
    project_name: String,
    dataset_name: String,
    project_url: String,
    dataset_url: String,
    #[serde(default)]
    data_summary: Option<DatasetDataSummary>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl SummarizeDatasetResponse {
    api_getters! {
        str project_name;
        str dataset_name;
        str project_url;
        str dataset_url;
        option_ref data_summary: DatasetDataSummary;
        map extra;
    }
}

/// Dataset metadata returned by the dataset API.
///
/// Unknown backend fields are preserved in [`Dataset::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Dataset {
    id: String,
    project_id: String,
    name: String,
    url_slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl Dataset {
    api_getters! {
        str id;
        str project_id;
        str name;
        str url_slug;
        option_str description;
        option_str created;
        option_str deleted_at;
        option_str user_id;
        option_slice tags: String;
        option_map metadata;
        map extra;
    }
}

/// Aggregate dataset data summary returned by [`DatasetsClient::summarize()`].
///
/// Unknown backend fields are preserved in [`DatasetDataSummary::extra()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetDataSummary {
    total_records: u64,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl DatasetDataSummary {
    api_getters! {
        u64 total_records;
        map extra;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client, assert_query_value, auth_header, json_body, request, request_with_body,
    };

    #[tokio::test]
    async fn methods_send_expected_requests() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("GET"))
            .and(path("/v1/dataset"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{
                    "id": "dataset-id",
                    "project_id": "project-id",
                    "name": "Dataset",
                    "url_slug": "dataset"
                }]
            })))
            .mount(&server)
            .await;

        let list = api
            .datasets()
            .list(ListDatasetsRequest {
                limit: Some(3),
                dataset_name: Some("Dataset".to_string()),
                project_id: Some("project-id".to_string()),
                ..Default::default()
            })
            .await
            .expect("list datasets");
        assert_eq!(list.objects()[0].name(), "Dataset");

        Mock::given(method("GET"))
            .and(path("/v1/dataset/dataset-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "dataset-id",
                "project_id": "project-id",
                "name": "Dataset",
                "url_slug": "dataset"
            })))
            .mount(&server)
            .await;

        let dataset = api.datasets().get("dataset-id").await.expect("get dataset");
        assert_eq!(dataset.project_id(), "project-id");

        Mock::given(method("POST"))
            .and(path("/v1/dataset"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-dataset-id",
                "project_id": "project-id",
                "name": "Created Dataset",
                "url_slug": "created-dataset"
            })))
            .mount(&server)
            .await;

        let created = api
            .datasets()
            .create(CreateDatasetRequest {
                project_id: "project-id".to_string(),
                name: "Created Dataset".to_string(),
                description: Some("description".to_string()),
                tags: Some(vec!["tag".to_string()]),
                org_name: Some("Org".to_string()),
                ..Default::default()
            })
            .await
            .expect("create dataset");
        assert_eq!(created.id(), "created-dataset-id");

        Mock::given(method("POST"))
            .and(path("/v1/dataset/dataset-id/fetch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "events": [{
                    "id": "event-id",
                    "_xact_id": "xact-id",
                    "created": "2026-01-01T00:00:00Z",
                    "project_id": "project-id",
                    "dataset_id": "dataset-id",
                    "input": {"question": "2+2"},
                    "expected": {"answer": "4"},
                    "span_id": "span-id",
                    "root_span_id": "root-span-id",
                    "_pagination_key": "page-key",
                    "is_root": true,
                    "origin": {"id": "origin-id"}
                }],
                "cursor": "next"
            })))
            .mount(&server)
            .await;

        let fetched = api
            .datasets()
            .fetch(
                "dataset-id",
                FetchEventsRequest {
                    limit: Some(10),
                    cursor: Some("cursor".to_string()),
                    ..Default::default()
                },
            )
            .await
            .expect("fetch dataset");
        let event = &fetched.events()[0];
        assert_eq!(event.record().id(), "event-id");
        assert_eq!(event.id(), "event-id");
        assert_eq!(event.xact_id(), Some("xact-id"));
        assert_eq!(event.created(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(event.pagination_key(), Some("page-key"));
        assert_eq!(event.is_root(), Some(&true));
        assert_eq!(event.input().unwrap()["question"], "2+2");
        assert_eq!(event.expected().unwrap()["answer"], "4");
        assert_eq!(event.extra()["origin"]["id"], "origin-id");
        assert_eq!(fetched.cursor(), Some("next"));

        Mock::given(method("GET"))
            .and(path("/v1/dataset/dataset-id/summarize"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project_name": "Project",
                "dataset_name": "Dataset",
                "project_url": "https://www.braintrust.dev/app/Project",
                "dataset_url": "https://www.braintrust.dev/app/Project/datasets/Dataset",
                "data_summary": {
                    "total_records": 12
                }
            })))
            .mount(&server)
            .await;

        let summary = api
            .datasets()
            .summarize(
                "dataset-id",
                SummarizeDatasetRequest {
                    summarize_data: Some(true),
                },
            )
            .await
            .expect("summarize dataset");
        assert_eq!(summary.data_summary().unwrap().total_records(), 12);

        let requests = server.received_requests().await.expect("requests");
        let list = request(&requests, "GET", "/v1/dataset");
        assert_eq!(auth_header(list), "Bearer token");
        assert_query_value(list, "limit", "3");
        assert_query_value(list, "dataset_name", "Dataset");
        assert_query_value(list, "project_id", "project-id");

        let create = request_with_body(&requests, "POST", "/v1/dataset");
        assert_eq!(json_body(create)["project_id"], "project-id");
        assert_eq!(json_body(create)["name"], "Created Dataset");
        assert_eq!(json_body(create)["tags"], json!(["tag"]));
        assert_eq!(json_body(create)["org_name"], "Org");

        let fetch = request_with_body(&requests, "POST", "/v1/dataset/dataset-id/fetch");
        assert_eq!(auth_header(fetch), "Bearer token");
        assert_eq!(json_body(fetch)["limit"], 10);
        assert_eq!(json_body(fetch)["cursor"], "cursor");

        let summarize = request(&requests, "GET", "/v1/dataset/dataset-id/summarize");
        assert_query_value(summarize, "summarize_data", "true");
    }

    #[tokio::test]
    async fn delete_sends_expected_request() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("DELETE"))
            .and(path("/v1/dataset/dataset-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "dataset-id",
                "deleted_at": "2026-01-01T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let deleted = api.datasets().delete("dataset-id").await.expect("delete");
        assert_eq!(deleted.id(), Some("dataset-id"));

        let requests = server.received_requests().await.expect("requests");
        let request = request(&requests, "DELETE", "/v1/dataset/dataset-id");
        assert_eq!(auth_header(request), "Bearer token");
    }
}
