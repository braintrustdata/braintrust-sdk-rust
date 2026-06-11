//! Raw registration endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{ApiBase, ApiClient};
use crate::error::Result;

/// Request body for dataset registration.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
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
    project: DatasetProjectInfo,
    dataset: DatasetInfo,
}

impl DatasetRegisterResponse {
    api_getters! {
        ref project: DatasetProjectInfo;
        ref dataset: DatasetInfo;
    }
}

/// Project info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetProjectInfo {
    id: String,
    name: String,
}

impl DatasetProjectInfo {
    api_getters! {
        str id;
        str name;
    }
}

/// Dataset info from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct DatasetInfo {
    id: String,
    name: String,
}

impl DatasetInfo {
    api_getters! {
        str id;
        str name;
    }
}

/// Repository information for experiment tracking.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct RepoInfo {
    /// Git commit SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Current branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Current tag (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Whether the working directory has uncommitted changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    /// Git author name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Git author email.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_email: Option<String>,
    /// Git commit message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    /// Git commit timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_time: Option<String>,
}

/// Request body for experiment registration.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ExperimentRegisterRequest {
    pub project_name: String,
    pub org_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_experiment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_info: Option<RepoInfo>,
}

/// Response from experiment registration.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentRegisterResponse {
    project: ExperimentProjectInfo,
    experiment: ExperimentInfo,
}

impl ExperimentRegisterResponse {
    api_getters! {
        ref project: ExperimentProjectInfo;
        ref experiment: ExperimentInfo;
    }
}

/// Project information from experiment registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentProjectInfo {
    id: String,
    name: String,
}

impl ExperimentProjectInfo {
    api_getters! {
        str id;
        str name;
    }
}

/// Experiment information from registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct ExperimentInfo {
    id: String,
    name: String,
}

impl ExperimentInfo {
    api_getters! {
        str id;
        str name;
    }
}

/// Trait for registering experiments with the Braintrust API.
#[async_trait::async_trait]
pub(crate) trait ExperimentRegistrar: Send + Sync {
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse>;
}

/// Client for registration API operations.
#[derive(Clone, Debug)]
pub struct RegistrationsClient {
    api: ApiClient,
}

impl RegistrationsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub async fn register_project(
        &self,
        request: ProjectRegisterRequest,
    ) -> crate::Result<ProjectRegisterResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/project/register",
                &request,
                "project registration response",
            )
            .await
    }

    pub async fn register_dataset(
        &self,
        request: DatasetRegisterRequest,
    ) -> crate::Result<DatasetRegisterResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/dataset/register",
                &request,
                "dataset registration response",
            )
            .await
    }

    pub async fn register_experiment(
        &self,
        request: ExperimentRegisterRequest,
    ) -> crate::Result<ExperimentRegisterResponse> {
        self.api
            .post_json_to(
                ApiBase::App,
                "api/experiment/register",
                &request,
                "experiment registration response",
            )
            .await
    }
}

/// Request body for project registration.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ProjectRegisterRequest {
    pub project_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Response from project registration.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectRegisterResponse {
    project: ProjectInfo,
}

impl ProjectRegisterResponse {
    api_getters! {
        ref project: ProjectInfo;
    }
}

/// Project info in registration response.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectInfo {
    id: String,
}

impl ProjectInfo {
    api_getters! {
        str id;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::test_support::{
        api_client_with_urls, auth_header, json_body, request_with_body,
    };

    #[tokio::test]
    async fn registration_endpoints_use_app_url() {
        let api_server = MockServer::start().await;
        let app_server = MockServer::start().await;
        let api = api_client_with_urls(api_server.uri(), app_server.uri(), "Org");

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project": { "id": "project-id" }
            })))
            .mount(&app_server)
            .await;

        let response = api
            .registrations()
            .register_project(ProjectRegisterRequest {
                project_name: "Project".to_string(),
                org_id: Some("org-id".to_string()),
                org_name: Some("Org".to_string()),
            })
            .await
            .expect("register project");
        assert_eq!(response.project().id(), "project-id");

        let app_requests = app_server.received_requests().await.expect("requests");
        let request = request_with_body(&app_requests, "POST", "/api/project/register");
        assert_eq!(auth_header(request), "Bearer token");
        assert_eq!(json_body(request)["project_name"], "Project");
        assert_eq!(json_body(request)["org_id"], "org-id");
        assert_eq!(json_body(request)["org_name"], "Org");

        Mock::given(method("POST"))
            .and(path("/api/dataset/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project": { "id": "project-id", "name": "Project" },
                "dataset": { "id": "dataset-id", "name": "Dataset" }
            })))
            .mount(&app_server)
            .await;

        let dataset = api
            .registrations()
            .register_dataset(DatasetRegisterRequest {
                project_name: "Project".to_string(),
                org_name: "Org".to_string(),
                dataset_name: "Dataset".to_string(),
                description: Some("description".to_string()),
                ..Default::default()
            })
            .await
            .expect("register dataset");
        assert_eq!(dataset.dataset().id(), "dataset-id");

        Mock::given(method("POST"))
            .and(path("/api/experiment/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "project": { "id": "project-id", "name": "Project" },
                "experiment": { "id": "experiment-id", "name": "Experiment" }
            })))
            .mount(&app_server)
            .await;

        let experiment = api
            .registrations()
            .register_experiment(ExperimentRegisterRequest {
                project_name: "Project".to_string(),
                org_id: "org-id".to_string(),
                experiment_name: Some("Experiment".to_string()),
                public: Some(false),
                ..Default::default()
            })
            .await
            .expect("register experiment");
        assert_eq!(experiment.experiment().id(), "experiment-id");

        let app_requests = app_server.received_requests().await.expect("requests");
        let dataset_request = request_with_body(&app_requests, "POST", "/api/dataset/register");
        assert_eq!(auth_header(dataset_request), "Bearer token");
        assert_eq!(json_body(dataset_request)["dataset_name"], "Dataset");
        assert_eq!(json_body(dataset_request)["description"], "description");

        let experiment_request =
            request_with_body(&app_requests, "POST", "/api/experiment/register");
        assert_eq!(auth_header(experiment_request), "Bearer token");
        assert_eq!(
            json_body(experiment_request)["experiment_name"],
            "Experiment"
        );
        assert_eq!(json_body(experiment_request)["public"], false);

        let api_requests = api_server.received_requests().await.expect("requests");
        assert!(api_requests.is_empty());
    }
}
