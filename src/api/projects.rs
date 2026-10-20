//! Existing project registration endpoint.
use super::{Client, Error, Plane, Transport};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectRegisterRequest {
    pub project_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectRegisterResponse {
    pub project: ProjectInfo,
}
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}
#[derive(Clone)]
pub struct Projects {
    pub(crate) client: Client,
}
impl Projects {
    /// Register one project on the control plane.
    /// Submission is immediate; dropping the token does not cancel accepted work.
    pub fn register(
        &self,
        request: ProjectRegisterRequest,
    ) -> crate::Completion<ProjectRegisterResponse, Error> {
        self.client
            .submit(move |transport| async move { transport.register_project(request).await })
    }
}

impl Transport {
    pub(crate) async fn register_project(
        &self,
        request: ProjectRegisterRequest,
    ) -> Result<ProjectRegisterResponse, Error> {
        self.post_json(Plane::Control, "api/project/register", &request)
            .await
    }
}
