use crate::log_queue::{
    config::LogQueueConfig,
    core::LogQueueCore,
    credential::Credential,
    submission::{PartitionDestination, SubmitCommand},
};
use std::sync::Arc;

/// An org at one endpoint: what budgets and request scheduling count against.
#[derive(Hash, PartialEq, Eq, Clone)]
pub(super) struct OrgKey {
    pub(super) endpoint: String,
    /// See [`crate::resolution::OrgIdentity::scope_key`].
    pub(super) org: String,
}

/// One delivery partition: an org's rows that can share requests. Each has at
/// most one job in flight, so an org that is retrying or cooling down never
/// delays another org's uploads.
#[derive(Hash, PartialEq, Eq, Clone)]
pub(super) struct Key {
    pub(super) route: OrgKey,
    /// The organization name sent with requests.
    pub(super) org_name: Option<String>,
    credential: Credential,
    policy: Arc<LogQueueConfig>,
    transport: u64,
    app_url: String,
    destination: PartitionDestination,
}

impl Key {
    pub(super) fn aliases(&self, other: &Self) -> bool {
        self.destination.aliases(&other.destination)
    }

    pub(super) fn merges_spans(&self) -> bool {
        matches!(self.destination, PartitionDestination::Span(_))
    }

    pub(super) fn new(core: &LogQueueCore, command: &SubmitCommand) -> Self {
        let (org_id, org_name) = command.organization();
        let (endpoint, org_name) = core.route(org_id, org_name);
        let credential = command.credential.clone();
        let org = crate::resolution::OrgIdentity::new(
            org_id,
            org_name.as_deref(),
            &credential.stand_in(),
        )
        .request_scope_key(&endpoint);
        Self {
            route: OrgKey { endpoint, org },
            org_name,
            credential,
            policy: core.settings.config.clone(),
            transport: core.settings.client.identity,
            app_url: core.settings.app_url.to_string(),
            destination: command.destination(),
        }
    }
}
