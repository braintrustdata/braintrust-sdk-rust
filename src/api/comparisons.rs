//! Raw experiment comparison endpoints.

use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

use crate::api::client::{ApiBase, ApiClient};
use crate::error::Result;

/// Request body for fetching base experiment.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct BaseExperimentRequest {
    pub id: String,
}

/// Response from base experiment fetch.
#[derive(Debug, Clone, Deserialize)]
pub struct BaseExperimentResponse {
    base_exp_id: Option<String>,
    base_exp_name: Option<String>,
}

impl BaseExperimentResponse {
    api_getters! {
        option_str base_exp_id;
        option_str base_exp_name;
    }
}

/// Response from experiment comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentComparisonResponse {
    #[serde(default)]
    scores: HashMap<String, ComparisonScoreData>,
    #[serde(default)]
    metrics: HashMap<String, ComparisonMetricData>,
    base_experiment_name: Option<String>,
}

impl ExperimentComparisonResponse {
    api_getters! {
        hash_map scores: ComparisonScoreData;
        hash_map metrics: ComparisonMetricData;
        option_str base_experiment_name;
    }
}

/// Score data from comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ComparisonScoreData {
    score: f64,
    diff: Option<f64>,
    improvements: Option<u32>,
    regressions: Option<u32>,
}

impl ComparisonScoreData {
    api_getters! {
        f64 score;
        option_f64 diff;
        option_u32 improvements;
        option_u32 regressions;
    }
}

/// Metric data from comparison API.
#[derive(Debug, Clone, Deserialize)]
pub struct ComparisonMetricData {
    metric: f64,
    unit: Option<String>,
    diff: Option<f64>,
    improvements: Option<u32>,
    regressions: Option<u32>,
}

impl ComparisonMetricData {
    api_getters! {
        f64 metric;
        option_str unit;
        option_f64 diff;
        option_u32 improvements;
        option_u32 regressions;
    }
}

/// Trait for fetching base experiment information.
#[async_trait::async_trait]
pub(crate) trait BaseExperimentFetcher: Send + Sync {
    async fn fetch_base_experiment(&self, experiment_id: &str) -> Result<BaseExperimentResponse>;
}

/// Trait for fetching experiment comparison data.
#[async_trait::async_trait]
pub(crate) trait ExperimentComparisonFetcher: Send + Sync {
    async fn fetch_experiment_comparison(
        &self,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse>;
}

/// Client for experiment comparison API operations.
#[derive(Clone, Debug)]
pub struct ComparisonsClient {
    api: ApiClient,
}

impl ComparisonsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub async fn base_experiment(
        &self,
        request: BaseExperimentRequest,
    ) -> crate::Result<BaseExperimentResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/base_experiment/get_id",
                &request,
                "base experiment response",
            )
            .await
    }

    pub async fn experiment_comparison(
        &self,
        request: ExperimentComparisonRequest,
    ) -> crate::Result<ExperimentComparisonResponse> {
        self.api
            .get_json_with_query_from(
                ApiBase::App,
                "experiment-comparison2",
                &request,
                "experiment comparison response",
            )
            .await
    }
}

/// Query parameters for fetching experiment comparison data.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ExperimentComparisonRequest {
    pub experiment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_experiment_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client_with_urls, assert_query_value, auth_header, json_body, query_value, request,
        request_with_body,
    };

    #[tokio::test]
    async fn comparison_endpoints_use_app_url() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "");

        Mock::given(method("POST"))
            .and(path("/api/base_experiment/get_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "base_exp_id": "base-id",
                "base_exp_name": "Base"
            })))
            .mount(&app_server)
            .await;

        let base = api
            .comparisons()
            .base_experiment(BaseExperimentRequest {
                id: "experiment-id".to_string(),
            })
            .await
            .expect("base experiment");
        assert_eq!(base.base_exp_id(), Some("base-id"));

        Mock::given(method("GET"))
            .and(path("/experiment-comparison2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "base_experiment_name": "Base",
                "scores": {
                    "accuracy": {
                        "score": 0.9
                    }
                }
            })))
            .mount(&app_server)
            .await;

        let comparison = api
            .comparisons()
            .experiment_comparison(ExperimentComparisonRequest {
                experiment_id: "experiment-id".to_string(),
                base_experiment_id: Some("base-id".to_string()),
            })
            .await
            .expect("comparison");
        assert_eq!(comparison.base_experiment_name(), Some("Base"));

        let app_requests = app_server.received_requests().await.expect("requests");
        let base_request = request_with_body(&app_requests, "POST", "/api/base_experiment/get_id");
        assert_eq!(auth_header(base_request), "Bearer token");
        assert_eq!(json_body(base_request)["id"], "experiment-id");

        let comparison_request = request(&app_requests, "GET", "/experiment-comparison2");
        assert_query_value(comparison_request, "experiment_id", "experiment-id");
        assert_query_value(comparison_request, "base_experiment_id", "base-id");

        let api_requests = api_server.received_requests().await.expect("requests");
        assert!(api_requests.is_empty());
    }

    #[tokio::test]
    async fn experiment_comparison_omits_missing_base_id() {
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(app_server.uri(), app_server.uri(), "");

        Mock::given(method("GET"))
            .and(path("/experiment-comparison2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&app_server)
            .await;

        api.comparisons()
            .experiment_comparison(ExperimentComparisonRequest {
                experiment_id: "experiment-id".to_string(),
                ..Default::default()
            })
            .await
            .expect("comparison");

        let requests = app_server.received_requests().await.expect("requests");
        let request = request(&requests, "GET", "/experiment-comparison2");
        assert_query_value(request, "experiment_id", "experiment-id");
        assert_eq!(query_value(request, "base_experiment_id"), None);
    }
}
