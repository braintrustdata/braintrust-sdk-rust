//! Request admission and bounded retries for the SDK's one-attempt API.
use crate::error::{BraintrustError, Result};
use crate::http::api_result;
use crate::scheduler::{self, Scope, SCOPE};
use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder, Retryable};
use std::{future::Future, time::Duration};
use tracing::warn;

/// Delays between the attempts of queued work, which retries `retries` times.
pub(crate) fn queue_delays(retries: usize) -> ExponentialBackoff {
    ExponentialBuilder::default()
        .with_min_delay(Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(30))
        .with_max_times(retries)
        .build()
}
// The data plane's rate-limit window is 60 seconds, so a longer Retry-After is never useful.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

/// A failed request attempt, with the server's Retry-After hint when it sent one.
struct Attempt {
    error: BraintrustError,
    retry_after: Option<Duration>,
}

async fn attempt<T>(
    result: std::result::Result<T, crate::api::Error<serde_json::Value>>,
) -> std::result::Result<T, Attempt> {
    let retry_after = match &result {
        Err(crate::api::Error::Api(response)) => response
            .headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse().ok())
            .map(Duration::from_secs),
        _ => None,
    };
    api_result(result)
        .await
        .map_err(|error| Attempt { error, retry_after })
}

/// Wait at least as long as the server asked, without retrying once the budget is exhausted.
/// A 429 pauses its scope, even when this request has no retries left, so
/// other requests sharing the limit do not repeat the rejected attempt.
fn retry_delay(attempt: &Attempt, backoff: Option<Duration>) -> Option<Duration> {
    let hint = attempt.retry_after.map(|hint| hint.min(MAX_RETRY_AFTER));
    let delay = backoff.map(|backoff| hint.map_or(backoff, |hint| hint.max(backoff)));
    if matches!(attempt.error, BraintrustError::Api { status: 429, .. }) {
        if let Some(wait) = delay.or(hint) {
            scheduler::shared().cool_down(wait);
        }
    }
    delay
}

/// Issue a request through the shared scheduler within `scope`: it waits out
/// cooldowns, holds a request permit only while in flight, and retries
/// transient failures `retries` times. A 429 pauses the scope even when no
/// retries are left.
pub(crate) async fn scheduled<T, F, Fut>(scope: Scope, retries: usize, operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, crate::api::Error<serde_json::Value>>>,
{
    SCOPE
        .scope(
            scope,
            RetryBudget::new(queue_delays(retries)).run(operation),
        )
        .await
}

// Borrowing the iterator lets backon share retries across overflow stages and splits.
pub(crate) struct RetryBudget {
    delays: ExponentialBackoff,
    requests: usize,
}
impl RetryBudget {
    pub fn new(delays: ExponentialBackoff) -> Self {
        Self {
            delays,
            requests: 0,
        }
    }

    pub fn requests(&self) -> usize {
        self.requests
    }

    pub async fn run<T, F, Fut>(&mut self, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, crate::api::Error<serde_json::Value>>>,
    {
        (|| {
            self.requests += 1;
            let operation = operation();
            async move {
                let _permit = scheduler::shared().acquire().await;
                attempt(operation.await).await
            }
        })
        .retry(&mut self.delays)
        .when(|attempt| retryable(&attempt.error))
        .adjust(retry_delay)
        .notify(|attempt, delay| {
            warn!(error = %attempt.error, ?delay, "ingestion request failed, retrying")
        })
        .await
        .map_err(|attempt| attempt.error)
    }
}

fn retryable(error: &BraintrustError) -> bool {
    match error {
        BraintrustError::Http(error) => !error.is_builder(),
        // A transport failure other than a reqwest error, such as a timeout.
        BraintrustError::Network(_) => true,
        // Request timeout, too early, rate limited, or a server error.
        BraintrustError::Api { status, .. } => {
            matches!(status, 408 | 425 | 429) || (500..600).contains(status)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failures_are_retried_and_permanent_ones_are_not() {
        let api = |status| BraintrustError::Api {
            status,
            message: String::new(),
        };
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert!(retryable(&api(status)), "{status} should be retried");
        }
        for status in [400, 401, 403, 404, 413, 422] {
            assert!(!retryable(&api(status)), "{status} should not be retried");
        }
        // How the API client reports its own timeouts and middleware failures.
        let timeout: crate::api::Error<serde_json::Value> =
            crate::api::Error::Transport(crate::api::HttpError::Timeout);
        let error = futures::executor::block_on(api_result::<()>(Err(timeout))).unwrap_err();
        assert!(retryable(&error), "{error} should be retried");
        assert!(!retryable(&BraintrustError::InvalidConfig(String::new())));
    }

    #[test]
    fn retry_after_never_shortens_backoff_and_is_capped() {
        let rate_limited = |retry_after| Attempt {
            error: BraintrustError::Api {
                status: 429,
                message: String::new(),
            },
            retry_after,
        };
        let backoff = Some(Duration::from_secs(2));
        assert_eq!(
            retry_delay(&rate_limited(Some(Duration::from_secs(20))), backoff),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            retry_delay(&rate_limited(Some(Duration::ZERO)), backoff),
            backoff
        );
        assert_eq!(retry_delay(&rate_limited(None), backoff), backoff);
        assert_eq!(
            retry_delay(&rate_limited(Some(Duration::from_secs(3600))), backoff),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            retry_delay(&rate_limited(Some(Duration::from_secs(20))), None),
            None,
            "an exhausted retry budget must stay exhausted"
        );
    }
}
