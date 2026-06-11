//! Project resource endpoints.
//!
//! Projects are top-level containers for datasets, experiments, and logs. This
//! module exposes the low-level `/v1/project` API category and the typed shapes
//! used only by those endpoints.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::api::client::{path_segment, ApiClient};

/// Client for project API operations.
///
/// Create this through [`crate::ApiClient::projects()`].
#[derive(Clone, Debug)]
pub struct ProjectsClient {
    api: ApiClient,
}

impl ProjectsClient {
    pub(crate) fn new(api: ApiClient) -> Self {
        Self { api }
    }

    api_endpoint_methods! {
        list(list, ListProjectsRequest, ListProjectsResponse, "v1/project", "project list response");
        get(get, project_id, Project, "v1/project", "project response");
        create(create, CreateProjectRequest, Project, "v1/project", "project create response");
    }

    /// Deletes one project by id.
    pub async fn delete(&self, project_id: &str) -> crate::Result<DeleteProjectResponse> {
        self.api
            .delete_json(
                &format!("v1/project/{}", path_segment(project_id)),
                "project delete response",
            )
            .await
    }
}

/// Request body for creating a project.
///
/// Construct with [`CreateProjectRequest::default()`], then set the fields to send.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct CreateProjectRequest {
    /// Project name.
    pub name: String,
    /// Optional project description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional organization name used to disambiguate project creation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Query parameters for listing projects.
///
/// Optional fields are omitted from the query string. `ids` is encoded as
/// repeated `ids` query parameters.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ListProjectsRequest {
    /// Maximum number of projects to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Pagination cursor for fetching records after a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_after: Option<String>,
    /// Pagination cursor for fetching records before a previous result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ending_before: Option<String>,
    /// Project ids to include.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    /// Project name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// Project name filter accepted by some internal callers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Organization name filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// Response returned by [`ProjectsClient::list()`].
#[derive(Debug, Clone, Deserialize)]
pub struct ListProjectsResponse {
    objects: Vec<Project>,
}

impl ListProjectsResponse {
    api_getters! {
        slice objects: Project;
    }
}

/// Response returned by [`ProjectsClient::delete()`].
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteProjectResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    deleted_at: Option<String>,
}

impl DeleteProjectResponse {
    api_getters! {
        option_str id;
        option_str deleted_at;
    }
}

/// Project metadata returned by the project API.
///
/// Unknown backend fields are preserved in [`Project::extra()`].
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Project {
    id: String,
    org_id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deleted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<Value>,
    #[serde(flatten, default)]
    extra: Map<String, Value>,
}

impl Project {
    api_getters! {
        str id;
        str org_id;
        str name;
        option_str description;
        option_str created;
        option_str deleted_at;
        option_str user_id;
        option_ref settings: Value;
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
        api_client, assert_query_value, auth_header, json_body, query_values, request,
        request_with_body,
    };

    #[tokio::test]
    async fn list_get_and_create_send_expected_requests() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("GET"))
            .and(path("/v1/project"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "objects": [{
                    "id": "project-id",
                    "org_id": "org-id",
                    "name": "Project"
                }]
            })))
            .mount(&server)
            .await;

        let list_request = ListProjectsRequest {
            limit: Some(5),
            project_name: Some("Project".to_string()),
            name: Some("Project".to_string()),
            org_name: Some("Org".to_string()),
            ids: vec!["project-id".to_string()],
            ..Default::default()
        };

        let listed = api.projects().list(list_request).await.expect("list");
        assert_eq!(listed.objects()[0].id(), "project-id");

        Mock::given(method("GET"))
            .and(path("/v1/project/project-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "project-id",
                "org_id": "org-id",
                "name": "Project"
            })))
            .mount(&server)
            .await;

        let project = api.projects().get("project-id").await.expect("get");
        assert_eq!(project.name(), "Project");

        Mock::given(method("POST"))
            .and(path("/v1/project"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "created-project-id",
                "org_id": "org-id",
                "name": "Created Project"
            })))
            .mount(&server)
            .await;

        let created = api
            .projects()
            .create(CreateProjectRequest {
                name: "Created Project".to_string(),
                description: Some("description".to_string()),
                org_name: Some("Org".to_string()),
            })
            .await
            .expect("create");
        assert_eq!(created.id(), "created-project-id");

        let requests = server.received_requests().await.expect("requests");
        let list = request(&requests, "GET", "/v1/project");
        assert_eq!(auth_header(list), "Bearer token");
        assert_query_value(list, "limit", "5");
        assert_query_value(list, "project_name", "Project");
        assert_query_value(list, "name", "Project");
        assert_query_value(list, "org_name", "Org");
        assert_eq!(query_values(list, "ids"), vec!["project-id"]);

        let create = request_with_body(&requests, "POST", "/v1/project");
        assert_eq!(auth_header(create), "Bearer token");
        assert_eq!(json_body(create)["name"], "Created Project");
        assert_eq!(json_body(create)["description"], "description");
        assert_eq!(json_body(create)["org_name"], "Org");
    }

    #[tokio::test]
    async fn delete_sends_expected_request() {
        let server = MockServer::start().await;
        let api = api_client(server.uri());

        Mock::given(method("DELETE"))
            .and(path("/v1/project/project-id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "project-id",
                "deleted_at": "2026-01-01T00:00:00Z"
            })))
            .mount(&server)
            .await;

        let deleted = api.projects().delete("project-id").await.expect("delete");
        assert_eq!(deleted.id(), Some("project-id"));

        let requests = server.received_requests().await.expect("requests");
        let request = request(&requests, "DELETE", "/v1/project/project-id");
        assert_eq!(auth_header(request), "Bearer token");
    }
}
