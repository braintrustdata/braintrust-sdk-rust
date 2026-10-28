//! Login response values. Login never mutates SDK session state.
use serde::Deserialize;

/// Organization info returned from login.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) api_url: Option<String>,
}

impl OrgInfo {
    /// The organization's unique ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The organization's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The organization's API URL, if set.
    pub fn api_url(&self) -> Option<&str> {
        self.api_url.as_deref()
    }
}

/// Response from the login endpoint.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct LoginResponse {
    pub(crate) org_info: Vec<OrgInfo>,
}

impl LoginResponse {
    /// Return the org info.
    pub fn org_info(&self) -> &[OrgInfo] {
        &self.org_info
    }
}
