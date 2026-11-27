/// Who a request counts against. Every SDK request runs inside one, so permits
/// rotate fairly across orgs and cooldowns can cover an org or a destination.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct Scope {
    endpoint: String,
    /// The org's identity; see [`crate::resolution::OrgIdentity::scope_key`].
    org: String,
    destination: Option<String>,
}
impl Scope {
    pub fn new(endpoint: impl Into<String>, org: impl Into<String>) -> Self {
        let mut endpoint = endpoint.into();
        endpoint.truncate(endpoint.trim_end_matches('/').len());
        Self {
            endpoint,
            org: org.into(),
            destination: None,
        }
    }

    pub(super) fn for_destination(&self, destination: Option<String>) -> Self {
        Self {
            destination,
            ..self.clone()
        }
    }

    pub(super) fn has_destination(&self) -> bool {
        self.destination.is_some()
    }

    pub(super) fn org_scope(&self) -> Self {
        self.for_destination(None)
    }
}
tokio::task_local! { pub(crate) static SCOPE: Scope; }
