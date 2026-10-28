//! Existing project registration endpoint.
use super::{Client, Error, Plane};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ProjectRegisterRequest {
    pub(crate) project_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) org_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) org_name: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProjectRegisterResponse {
    pub(crate) project: ProjectInfo,
}
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ProjectInfo {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: Option<String>,
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

impl ProjectRegisterRequest {
    /// Construct a registration request with the required project name.
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            org_id: None,
            org_name: None,
        }
    }
}

impl ProjectRegisterRequest {
    /// Return the project name.
    pub fn project_name(&self) -> &str {
        &self.project_name
    }
    /// Set the project name.
    pub fn set_project_name(&mut self, value: impl Into<String>) -> &mut Self {
        self.project_name = value.into();
        self
    }
    /// Return the org id.
    pub fn org_id(&self) -> Option<&str> {
        self.org_id.as_deref()
    }
    /// Set the org id.
    pub fn set_org_id(&mut self, value: Option<String>) -> &mut Self {
        self.org_id = value;
        self
    }
    /// Return the org name.
    pub fn org_name(&self) -> Option<&str> {
        self.org_name.as_deref()
    }
    /// Set the org name.
    pub fn set_org_name(&mut self, value: Option<String>) -> &mut Self {
        self.org_name = value;
        self
    }
}

impl ProjectRegisterResponse {
    /// Return the project.
    pub fn project(&self) -> &ProjectInfo {
        &self.project
    }
}

impl ProjectInfo {
    /// Return the id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}
