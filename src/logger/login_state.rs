use serde::Deserialize;
use std::sync::Arc;

/// Organization info returned from login.
#[derive(Debug, Clone, Deserialize)]
pub struct OrgInfo {
    id: String,
    name: String,
    #[serde(default)]
    api_url: Option<String>,
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

/// Logged-in state containing API key and org info.
/// Handles internal synchronization for the transition from not-logged-in to logged-in.
#[derive(Clone)]
pub struct LoginState {
    inner: Arc<std::sync::OnceLock<LoginStateInner>>,
}

#[derive(Debug, Clone)]
struct LoginStateInner {
    api_key: String,
    org_id: String,
    org_name: String,
    api_url: String,
    app_url: String,
}

impl Default for LoginState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginState {
    /// Create a new empty login state.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Set the login state (can only be called once).
    /// Returns true if set successfully, false if already set.
    pub fn set(
        &self,
        api_key: String,
        org_id: String,
        org_name: String,
        api_url: String,
        app_url: String,
    ) -> bool {
        self.inner
            .set(LoginStateInner {
                api_key,
                org_id,
                org_name,
                api_url,
                app_url,
            })
            .is_ok()
    }

    /// Check if logged in.
    pub fn is_logged_in(&self) -> bool {
        self.inner.get().is_some()
    }

    /// Get the API key if logged in.
    pub fn api_key(&self) -> Option<String> {
        self.inner.get().map(|s| s.api_key.clone())
    }

    /// Get the org ID if logged in.
    pub fn org_id(&self) -> Option<String> {
        self.inner.get().map(|s| s.org_id.clone())
    }

    /// Get the org name if logged in.
    pub fn org_name(&self) -> Option<String> {
        self.inner.get().map(|s| s.org_name.clone())
    }

    /// Get the API URL if logged in.
    pub fn api_url(&self) -> Option<String> {
        self.inner.get().map(|s| s.api_url.clone())
    }

    /// Get the app URL if logged in.
    pub fn app_url(&self) -> Option<String> {
        self.inner.get().map(|s| s.app_url.clone())
    }
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.get() {
            Some(inner) => f.debug_struct("LoginState").field("inner", inner).finish(),
            None => f
                .debug_struct("LoginState")
                .field("inner", &"<not logged in>")
                .finish(),
        }
    }
}
