use super::{BraintrustClient, LoginState};
use crate::error::BraintrustError;
use crate::experiments::ExperimentBuilder;
use std::sync::Arc;
use tokio::sync::Notify;

impl BraintrustClient {
    /// Select a project without creating a worker, HTTP pool, or network request.
    pub fn project(&self, name: impl Into<String>) -> crate::Project {
        let name = name.into();
        let client = self.derive_scope(
            self.inner.queue.child(),
            self.inner.login_state.clone(),
            Some(name.clone()),
        );
        crate::Project::new(client, name)
    }

    /// Return a facade for an organization permitted by these credentials.
    /// Existing handles and accepted work retain their organization and endpoints.
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn with_organization(
        &self,
        name: impl Into<String>,
    ) -> crate::Completion<Self, BraintrustError> {
        let client = self.clone();
        let name = name.into();
        crate::operation::submit(async move {
            let state = client.wait_for_login().await?;
            let state_key = state
                .api_key()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let login_state = LoginState::new();
            let mut client = client.derive_scope(
                client.inner.queue.rebind(login_state.clone()),
                login_state,
                client.inner.default_project.clone(),
            );
            let inner = Arc::get_mut(&mut client.inner).unwrap();
            inner.login_notify = Arc::new(Notify::new());
            inner.background_login = Arc::default();
            inner.pending_auth = Arc::default();
            client.perform_login(&state_key, Some(&name)).await?;
            Ok(client)
        })
    }

    /// Create an experiment builder using this facade's authentication and project defaults.
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn experiment_builder(
        &self,
    ) -> crate::Completion<ExperimentBuilder<Self>, BraintrustError> {
        let client = self.clone();
        crate::operation::submit(async move {
            let state = client.wait_for_login().await?;
            let (Some(api_key), Some(org_id)) = (state.api_key(), state.org_id()) else {
                return Err(BraintrustError::InvalidConfig("Not logged in".into()));
            };
            // The credentials builder already applies this facade's default project.
            let mut builder = client.experiment_builder_with_credentials(api_key, org_id);
            if let Some(name) = state.org_name() {
                builder = builder.org_name(name);
            }
            Ok(builder)
        })
    }
}
