//! Raw summary endpoints.

use serde::{Deserialize, Serialize};

use crate::api::client::ApiClient;

/// Response from dataset summary API.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetSummaryResponse {
    project_name: String,
    dataset_name: String,
    project_id: String,
    dataset_id: String,
    project_url: Option<String>,
    dataset_url: Option<String>,
    data_summary: Option<DataSummaryStats>,
}

impl DatasetSummaryResponse {
    api_getters! {
        str project_name;
        str dataset_name;
        str project_id;
        str dataset_id;
        option_str project_url;
        option_str dataset_url;
        option_ref data_summary: DataSummaryStats;
    }
}

/// Data summary statistics.
#[derive(Debug, Clone, Deserialize)]
pub struct DataSummaryStats {
    total_records: Option<u64>,
}

impl DataSummaryStats {
    api_getters! {
        option_u64 total_records;
    }
}

/// Client for summary API operations.
#[derive(Clone, Debug)]
pub struct SummariesClient {
    api: ApiClient,
}

impl SummariesClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub async fn dataset_summary(
        &self,
        request: DatasetSummaryRequest,
    ) -> crate::Result<DatasetSummaryResponse> {
        self.api
            .get_json_with_query("dataset-summary", &request, "dataset summary response")
            .await
    }
}

/// Query parameters for fetching a dataset summary.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct DatasetSummaryRequest {
    pub dataset_id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{api_client, assert_query_value, auth_header, request};

    #[tokio::test]
    async fn dataset_summary_uses_api_url_query_and_auth() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("GET"))
            .and(path("/dataset-summary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project_name": "Project",
                "dataset_name": "Dataset",
                "project_id": "project-id",
                "dataset_id": "dataset-id",
                "data_summary": {
                    "total_records": 12
                }
            })))
            .mount(&server)
            .await;

        let response = api
            .summaries()
            .dataset_summary(DatasetSummaryRequest {
                dataset_id: "dataset-id".to_string(),
            })
            .await
            .expect("summary");

        assert_eq!(response.dataset_id(), "dataset-id");
        assert_eq!(
            response
                .data_summary()
                .and_then(|summary| summary.total_records()),
            Some(12)
        );

        let requests = server.received_requests().await.expect("requests");
        let request = request(&requests, "GET", "/dataset-summary");
        assert_eq!(auth_header(request), "Bearer token");
        assert_query_value(request, "dataset_id", "dataset-id");
    }
}
