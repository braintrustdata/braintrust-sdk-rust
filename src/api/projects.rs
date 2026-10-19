//! Existing project registration endpoint.
use super::{Client, Error, Plane};
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
    pub async fn register(
        &self,
        request: ProjectRegisterRequest,
    ) -> Result<ProjectRegisterResponse, Error> {
        self.client
            .post_json(Plane::Control, "api/project/register", &request)
            .await
    }
}
