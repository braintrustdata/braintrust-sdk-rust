use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Supplies the bearer token for queued uploads when they are sent.
///
/// The queue calls [`TokenProvider::token`] on the SDK service's runtime. Cache
/// tokens that are expensive to mint. Each
/// call is bounded by [`crate::ServiceConfig::token_provider_timeout`], and a
/// failed call is retried like an upload. Once retries run out, the rows
/// waiting on it are dropped and counted in [`crate::DroppedRows::upload_failed`].
#[async_trait::async_trait]
pub trait TokenProvider: Send + Sync {
    /// Return the token for the next upload.
    async fn token(&self) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>>;
}

/// How a queued item authenticates its upload.
#[derive(Clone)]
pub(crate) enum Credential {
    Token(Arc<str>),
    /// Fetched when the item is sent. Items share a credential only if they share
    /// the provider `Arc`.
    Provider(Arc<dyn TokenProvider>),
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Token(_) => "Token([redacted])",
            Self::Provider(_) => "TokenProvider",
        })
    }
}

impl Credential {
    pub(crate) fn token_value(token: String) -> Self {
        Self::Token(Arc::from(token))
    }

    /// Identifies the credential when no organization is known. Providers are
    /// identified by instance, matching how they partition work.
    pub(crate) fn stand_in(&self) -> String {
        match self {
            Self::Token(token) => token.to_string(),
            Self::Provider(provider) => format!("provider:{:p}", Arc::as_ptr(provider)),
        }
    }

    pub(crate) async fn token(&self) -> std::result::Result<Cow<'_, str>, anyhow::Error> {
        match self {
            Self::Token(token) => Ok(Cow::Borrowed(token.as_ref())),
            Self::Provider(provider) => tokio::time::timeout(
                crate::service::config().token_provider_timeout,
                provider.token(),
            )
            .await
            .map_err(|_| anyhow::anyhow!("token provider timed out"))?
            .map(Cow::Owned)
            .map_err(|error| anyhow::anyhow!("token provider failed: {error}")),
        }
    }
}

impl PartialEq for Credential {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Token(a), Self::Token(b)) => a == b,
            (Self::Provider(a), Self::Provider(b)) => {
                std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
            }
            _ => false,
        }
    }
}

impl Eq for Credential {}

impl Hash for Credential {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Token(token) => token.hash(state),
            Self::Provider(provider) => Arc::as_ptr(provider).cast::<()>().hash(state),
        }
    }
}
