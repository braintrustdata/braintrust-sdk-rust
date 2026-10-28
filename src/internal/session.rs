//! Immutable credentials and endpoint routes captured by resource handles.
use std::{fmt, sync::Arc};
use url::Url;

/// Full request identity. Equality includes credentials, both routes and the org
/// header, so caches and upload groups cannot cross authentication boundaries.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct CredentialScope(Arc<Identity>);
#[derive(Clone, PartialEq, Eq, Hash)]
struct Identity {
    token: String,
    org_id: String,
    org_name: Option<String>,
    api_url: Url,
    app_url: Url,
}
impl CredentialScope {
    pub fn new(
        token: String,
        org_id: String,
        org_name: Option<String>,
        api_url: Url,
        app_url: Url,
    ) -> Self {
        Self(Arc::new(Identity {
            token,
            org_id,
            org_name,
            api_url,
            app_url,
        }))
    }
    pub fn token(&self) -> &str {
        &self.0.token
    }
    pub fn org_id(&self) -> &str {
        &self.0.org_id
    }
    pub fn org_name(&self) -> Option<&str> {
        self.0.org_name.as_deref()
    }
    pub fn api_url(&self) -> &Url {
        &self.0.api_url
    }
    pub fn with_org_name(self, org_name: String) -> Self {
        let mut identity = (*self.0).clone();
        identity.org_name = Some(org_name);
        Self(Arc::new(identity))
    }
    pub fn api(&self, http: &reqwest::Client) -> crate::error::Result<crate::api::Transport> {
        crate::api::Transport::for_scope(
            http.clone(),
            &self.0.api_url,
            &self.0.app_url,
            self.token(),
            self.org_name(),
        )
        .map_err(Into::into)
    }
}
impl fmt::Debug for CredentialScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialScope")
            .field("org_id", &self.org_id())
            .field("org_name", &self.org_name())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
impl CredentialScope {
    pub fn test(token: &str, org_id: &str) -> Self {
        Self::new(
            token.into(),
            org_id.into(),
            None,
            Url::parse("http://localhost").unwrap(),
            Url::parse("http://localhost").unwrap(),
        )
    }
}
