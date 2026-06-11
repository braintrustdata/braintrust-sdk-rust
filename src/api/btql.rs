//! Raw BTQL endpoints.

use crate::api::client::ApiClient;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::api::shared::DatasetRecord;

/// BTQL query for fetching dataset records.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct BTQLQuery {
    /// Dataset reference: dataset('id')
    pub from: String,
    /// Maximum records per page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Pagination cursor token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Generic BTQL request body for raw SQL-like or structured BTQL.
///
/// Prefer [`BtqlClient::query_dataset_records`] for typed dataset reads. This
/// request type is an escape hatch for query forms that do not yet have a
/// typed SDK method.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct BTQLRequest {
    /// Raw string query or structured BTQL JSON object.
    // TODO: Harden this Value-backed query if raw and parsed BTQL request bodies get separate types.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub query: Value,
    /// Optional response format requested by callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fmt: Option<BTQLFormat>,
    /// Optional lint handling mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lint_mode: Option<BTQLLintMode>,
    /// Deprecated strict lint mode flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_lint_mode: Option<bool>,
    /// Optional query source label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_source: Option<String>,
    /// Whether to request realtime Brainstore data when supported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brainstore_realtime: Option<bool>,
}

/// BTQL response format.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BTQLFormat {
    Json,
    Jsonl,
    Parquet,
}

/// BTQL lint handling mode.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BTQLLintMode {
    Default,
    Strict,
}

impl BTQLRequest {
    /// Builds a raw string BTQL request.
    pub fn from_query_string(query: impl Into<String>) -> Self {
        Self {
            query: Value::String(query.into()),
            ..Default::default()
        }
    }

    /// Builds a structured JSON BTQL request.
    pub fn from_query_json(query: Value) -> Self {
        Self {
            query,
            ..Default::default()
        }
    }
}

/// Response from a BTQL query.
#[derive(Debug, Clone, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct BTQLResponse<T = DatasetRecord> {
    /// Rows returned by the query.
    #[serde(default)]
    data: Vec<T>,
    /// Cursor for next page (None if last page).
    #[serde(default)]
    cursor: Option<String>,
}

impl<T> BTQLResponse<T> {
    api_getters! {
        slice data: T;
        option_str cursor;
    }
}

/// Client for BTQL API operations.
#[derive(Clone, Debug)]
pub struct BtqlClient {
    api: ApiClient,
}

impl BtqlClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub async fn query_dataset_records(
        &self,
        query: BTQLQuery,
    ) -> crate::Result<BTQLResponse<DatasetRecord>> {
        #[derive(Serialize)]
        struct DatasetBTQLRequest {
            query: BTQLQuery,
        }

        self.api
            .post_json("btql", &DatasetBTQLRequest { query }, "BTQL response")
            .await
    }

    /// Runs a generic BTQL request.
    ///
    /// Prefer [`Self::query_dataset_records`] for typed dataset records. This is
    /// an escape hatch for raw SQL-like or structured BTQL queries.
    pub async fn query<T>(&self, request: BTQLRequest) -> crate::Result<BTQLResponse<T>>
    where
        T: DeserializeOwned,
    {
        self.api
            .post_json("btql", &request, "generic BTQL response")
            .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{api_client, auth_header, json_body, request_with_body};

    #[test]
    fn btql_query_serializes_as_query_body_not_request_envelope() {
        let body = serde_json::to_value(BTQLQuery {
            from: "dataset('dataset-id')".to_string(),
            limit: Some(5),
            cursor: None,
        })
        .expect("serialize query");

        assert_eq!(
            body,
            json!({
                "from": "dataset('dataset-id')",
                "limit": 5
            })
        );
    }

    #[test]
    fn btql_request_constructors_serialize_raw_and_structured_queries() {
        let raw = serde_json::to_value(BTQLRequest {
            fmt: Some(BTQLFormat::Json),
            lint_mode: Some(BTQLLintMode::Strict),
            strict_lint_mode: Some(true),
            query_source: Some("sdk-test".to_string()),
            brainstore_realtime: Some(true),
            ..BTQLRequest::from_query_string("select * from dataset")
        })
        .expect("serialize raw request");

        assert_eq!(
            raw,
            json!({
                "query": "select * from dataset",
                "fmt": "json",
                "lint_mode": "strict",
                "strict_lint_mode": true,
                "query_source": "sdk-test",
                "brainstore_realtime": true
            })
        );

        let structured = serde_json::to_value(BTQLRequest::from_query_json(
            json!({"from": "dataset('id')"}),
        ))
        .expect("serialize structured request");

        assert_eq!(structured["query"]["from"], "dataset('id')");
    }

    #[tokio::test]
    async fn btql_uses_api_url() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("POST"))
            .and(path("/btql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "row-id",
                    "input": { "question": "2+2" }
                }],
                "cursor": "next"
            })))
            .mount(&server)
            .await;

        let response = api
            .btql()
            .query_dataset_records(BTQLQuery {
                from: "dataset('dataset-id')".to_string(),
                limit: Some(5),
                cursor: Some("cursor".to_string()),
            })
            .await
            .expect("btql");

        assert_eq!(response.data()[0].id(), "row-id");
        assert_eq!(response.cursor(), Some("next"));

        let requests = server.received_requests().await.expect("requests");
        let request = request_with_body(&requests, "POST", "/btql");
        assert_eq!(auth_header(request), "Bearer token");
        assert_eq!(json_body(request)["query"]["from"], "dataset('dataset-id')");
        assert_eq!(json_body(request)["query"]["limit"], 5);
        assert_eq!(json_body(request)["query"]["cursor"], "cursor");
    }

    #[tokio::test]
    async fn generic_btql_query_supports_string_query_and_custom_rows() {
        #[derive(Debug, Deserialize)]
        struct Row {
            id: String,
            score: f64,
        }

        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("POST"))
            .and(path("/btql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "row-id",
                    "score": 0.9
                }]
            })))
            .mount(&server)
            .await;

        let response = api
            .btql()
            .query::<Row>(BTQLRequest::from_query_string("select id, score"))
            .await
            .expect("generic btql");

        assert_eq!(response.data()[0].id, "row-id");
        assert_eq!(response.data()[0].score, 0.9);
        assert_eq!(response.cursor(), None);

        let requests = server.received_requests().await.expect("requests");
        let request = request_with_body(&requests, "POST", "/btql");
        assert_eq!(json_body(request)["query"], "select id, score");
    }

    #[tokio::test]
    async fn generic_btql_query_supports_structured_query_and_json_rows() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("POST"))
            .and(path("/btql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "value": {"nested": true}
                }],
                "cursor": null
            })))
            .mount(&server)
            .await;

        let response = api
            .btql()
            .query::<Value>(BTQLRequest::from_query_json(json!({
                "from": "dataset('dataset-id')",
                "limit": 1
            })))
            .await
            .expect("generic btql");

        assert_eq!(response.data()[0]["value"]["nested"], true);
        assert_eq!(response.cursor(), None);

        let requests = server.received_requests().await.expect("requests");
        let request = request_with_body(&requests, "POST", "/btql");
        assert_eq!(json_body(request)["query"]["from"], "dataset('dataset-id')");
        assert_eq!(json_body(request)["query"]["limit"], 1);
    }
}
