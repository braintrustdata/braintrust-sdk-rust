use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Supplies the bearer token for queued uploads when they are sent.
///
/// The queue calls [`TokenProvider::token`] once per upload group each time it
/// flushes. Resolving an uncached project name can also request a token.
/// Cache tokens that are expensive to mint, and bound how long a call
/// can take: a call that never returns stalls this client's uploads. When it
/// returns an error, the rows waiting on it are dropped and counted in
/// [`crate::DroppedRows::upload_failed`].
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

    pub(super) async fn token(&self) -> std::result::Result<Cow<'_, str>, anyhow::Error> {
        match self {
            Self::Token(token) => Ok(Cow::Borrowed(token)),
            Self::Provider(provider) => provider
                .token()
                .await
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
