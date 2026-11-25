//! SDK ingestion orchestration, built on the one-attempt API.
use super::ingest::EncodedBatch;
use crate::api::{self, transport::Logs3OverflowRequest};
use crate::error::{BraintrustError, Result};
use crate::http::api_result;
use crate::scheduler::{Pause, Scope, SCOPE};
use crate::types::LOGS_API_VERSION;
use backon::{BackoffBuilder, ExponentialBackoff, ExponentialBuilder, Retryable};
use std::{future::Future, time::Duration};
use tracing::warn;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capabilities {
    pub max_request_size: usize,
    pub overflow: bool,
}

/// Queue and bulk uploads use bounded backoff policies. Retries are shared across all stages and splits of one batch.
#[derive(Clone, Copy, Debug)]
pub(crate) enum UploadPolicy {
    Queue { retries: usize },
    Standalone,
}
impl UploadPolicy {
    fn budget(self) -> RetryBudget {
        let delays = match self {
            Self::Queue { retries } => ExponentialBuilder::default()
                .with_min_delay(Duration::from_secs(1))
                .with_max_delay(Duration::from_secs(30))
                .with_max_times(retries),
            Self::Standalone => ExponentialBuilder::default()
                .with_min_delay(Duration::from_millis(300))
                .with_max_delay(Duration::from_secs(8))
                .with_max_times(4)
                .with_jitter(),
        }
        .build();
        RetryBudget {
            delays,
            requests: 0,
            bytes: 1,
            // The 429 does not say whether a project or org limit was hit, so
            // pause the whole org rather than let sibling destinations repeat it.
            pause: Pause::Org,
        }
    }
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
/// Every 429 pauses its scope, even when this request has no retries left, so
/// other requests sharing the limit do not repeat the rejected attempt.
fn retry_delay(attempt: &Attempt, backoff: Option<Duration>, pause: Pause) -> Option<Duration> {
    let hint = attempt.retry_after.map(|hint| hint.min(MAX_RETRY_AFTER));
    let delay = backoff.map(|backoff| hint.map_or(backoff, |hint| hint.max(backoff)));
    if matches!(attempt.error, BraintrustError::Api { status: 429, .. }) {
        if let (Ok(service), Some(wait)) = (crate::service::global(), delay.or(hint)) {
            service.attempts.cool_down(wait, pause);
        }
    }
    delay
}

/// How a non-upload request is retried and how widely its 429s pause others.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RequestPolicy {
    pub retries: usize,
    pub pause: Pause,
}
impl RequestPolicy {
    /// One attempt; a 429 still pauses the org for other requests.
    pub const ONCE: Self = Self {
        retries: 0,
        pause: Pause::Org,
    };
}

/// Issue a request through the shared scheduler within `scope`: it waits out
/// cooldowns, holds a request permit only while in flight, and retries per `policy`.
pub(crate) async fn scheduled<T, F, Fut>(
    scope: Scope,
    policy: RequestPolicy,
    mut operation: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, crate::api::Error<serde_json::Value>>>,
{
    let mut budget = UploadPolicy::Queue {
        retries: policy.retries,
    }
    .budget();
    budget.pause = policy.pause;
    SCOPE
        .scope(
            scope,
            budget.run(|| {
                let request = operation();
                async move { attempt(request.await).await }
            }),
        )
        .await
}

/// Hold a request permit for an operation whose errors are not API responses,
/// such as login, after waiting out any cooldown for `scope`.
pub(crate) async fn admitted<T>(scope: Scope, operation: impl Future<Output = T>) -> Result<T> {
    let service = crate::service::global()?;
    let admitted = SCOPE.scope(scope, async {
        let _permit = service.attempts.acquire(1).await;
        operation.await
    });
    Ok(admitted.await)
}

// Borrowing the iterator lets backon share retries across overflow stages and splits.
struct RetryBudget {
    delays: ExponentialBackoff,
    requests: usize,
    bytes: usize,
    pause: Pause,
}
impl RetryBudget {
    async fn run<T, F, Fut>(&mut self, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, Attempt>>,
    {
        let pause = self.pause;
        (|| {
            self.requests += 1;
            let operation = operation();
            let bytes = self.bytes;
            async move {
                let service = crate::service::global().map_err(|error| Attempt { error, retry_after: None })?;
                let _permit = service.attempts.acquire(bytes).await;
                operation.await
            }
        })
        .retry(&mut self.delays)
        .when(|attempt| retryable(&attempt.error))
        .adjust(|attempt, backoff| retry_delay(attempt, backoff, pause))
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
        BraintrustError::Api { status, .. } => *status == 429 || (500..600).contains(status),
        _ => false,
    }
}
#[derive(Default)]
pub(crate) struct UploadResult {
    pub rows_uploaded: usize,
    pub bytes_processed: usize,
    pub requests_sent: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct UploadCoordinator {
    logs: api::Client,
    policy: UploadPolicy,
}
impl UploadCoordinator {
    pub fn new(logs: api::Client, policy: UploadPolicy) -> Self {
        Self { logs, policy }
    }

    /// The caller's scope, or this client's endpoint and organization.
    fn scope(&self) -> Scope {
        SCOPE.try_with(Clone::clone).unwrap_or_else(|_| {
            let org = self
                .logs
                .organization()
                .map(|org| org.id.clone())
                .or_else(|| self.logs.organization_name().map(str::to_owned))
                .unwrap_or_default();
            Scope::new(self.logs.baseurl(), org)
        })
    }

    pub async fn capabilities(
        &self,
        fallback: usize,
        override_bytes: Option<usize>,
    ) -> Capabilities {
        let version = scheduled(self.scope(), RequestPolicy::ONCE, || self.logs.version());
        let server_limit = match version.await.map(|v| v.into_inner()) {
            Ok(version) => version.logs3_payload_max_bytes.filter(|limit| *limit > 0),
            Err(error) => {
                warn!(%error, "failed to fetch ingestion capabilities");
                None
            }
        };
        let max_request_size = match (server_limit, override_bytes) {
            (Some(server), Some(local)) => server.min(local),
            (Some(server), None) => server,
            (None, Some(local)) => local,
            (None, None) => fallback,
        }
        .max(1);
        Capabilities {
            max_request_size,
            overflow: server_limit.is_some(),
        }
    }

    pub async fn upload(
        &self,
        batch: EncodedBatch,
        capabilities: Capabilities,
    ) -> Result<UploadResult> {
        let (result, outcome) = self.upload_counting(batch, capabilities).await;
        outcome.map(|()| result)
    }

    /// Upload a batch, reporting what was delivered even when a later split fails.
    pub async fn upload_counting(
        &self,
        batch: EncodedBatch,
        capabilities: Capabilities,
    ) -> (UploadResult, Result<()>) {
        let mut budget = self.policy.budget();
        budget.bytes = batch.data.len().max(1);
        let mut result = UploadResult::default();
        // Resolution may have replaced a project name with its canonical ID.
        let scope = self.scope().with_destination(batch.destination());
        let outcome = SCOPE
            .scope(
                scope,
                self.upload_into(batch, capabilities, &mut budget, &mut result),
            )
            .await;
        result.requests_sent = budget.requests;
        (result, outcome)
    }

    async fn upload_into(
        &self,
        batch: EncodedBatch,
        capabilities: Capabilities,
        budget: &mut RetryBudget,
        result: &mut UploadResult,
    ) -> Result<()> {
        let mut pending = vec![batch];
        while let Some(batch) = pending.pop() {
            if batch.data.len() <= capabilities.max_request_size {
                match budget
                    .run(|| async {
                        attempt(self.logs.logs3_raw(batch.data.clone()).await)
                            .await
                            .map(|_| ())
                    })
                    .await
                {
                    Ok(()) => {
                        result.rows_uploaded += batch.len();
                        result.bytes_processed += batch.data.len();
                        continue;
                    }
                    Err(BraintrustError::Api { status: 413, .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            if capabilities.overflow {
                self.overflow(&batch, budget).await?;
                result.rows_uploaded += batch.len();
                result.bytes_processed += batch.data.len();
            } else if batch.len() > 1 {
                let (left, right) = batch.split();
                pending.push(right);
                pending.push(left);
            } else {
                return Err(BraintrustError::Api {
                    status: 413,
                    message: "single row exceeds server payload limit and overflow is unavailable"
                        .into(),
                });
            }
        }
        Ok(())
    }

    async fn overflow(&self, batch: &EncodedBatch, budget: &mut RetryBudget) -> Result<()> {
        let grant = budget
            .run(|| async {
                let mut request = Logs3OverflowRequest::new(batch.overflow_rows());
                request.content_type = Some("application/json".into());
                request.size_bytes = Some(batch.data.len());
                attempt(
                    self.logs
                        .logs3_overflow_with_org_name(&request, self.logs.organization_name())
                        .await,
                )
                .await
                .map(|v| v.into_inner())
            })
            .await?;
        budget
            .run(|| async {
                attempt(
                    self.logs
                        .upload_logs3_overflow(&grant, batch.data.clone())
                        .await,
                )
                .await
                .map(|_| ())
            })
            .await?;
        budget
            .run(|| async {
                attempt(self.logs.logs3(&serde_json::json!({"rows":grant.reference(), "api_version":LOGS_API_VERSION})).await).await.map(|_| ())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transport::Logs3OverflowUpload;
    use crate::internal::ingest::EncodedRow;
    use serde_json::{json, Value};
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn batch() -> EncodedBatch {
        EncodedBatch::partition(
            vec![EncodedRow::from_map(
                json!({"id":"row", "project_id":"project", "log_id":"g", "input":"é"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap()],
            100,
            usize::MAX,
        )
        .pop()
        .unwrap()
    }
    fn coordinator(server: &MockServer, policy: UploadPolicy) -> UploadCoordinator {
        UploadCoordinator::new(
            crate::http::api_client(
                &crate::http::build_http_client(Duration::from_secs(3), None, None).unwrap(),
                &server.uri(),
                &server.uri(),
                "secret",
                Some("tenant"),
            )
            .unwrap(),
            policy,
        )
    }

    fn capabilities() -> Capabilities {
        Capabilities {
            max_request_size: 1,
            overflow: true,
        }
    }

    #[tokio::test]
    async fn both_callers_reuse_successful_overflow_stages_and_preserve_scopes() {
        for policy in [UploadPolicy::Queue { retries: 2 }, UploadPolicy::Standalone] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/logs3/overflow"))
                .and(header("authorization", "Bearer secret")).and(header("x-bt-org-name", "tenant"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"method":"PUT", "signedUrl":format!("{}/object", server.uri()), "key":"key"})))
                .expect(1).mount(&server).await;
            Mock::given(method("PUT"))
                .and(path("/object"))
                .respond_with(ResponseTemplate::new(503))
                .with_priority(1)
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("PUT"))
                .and(path("/object"))
                .respond_with(ResponseTemplate::new(200))
                .with_priority(2)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/logs3"))
                .respond_with(ResponseTemplate::new(503))
                .with_priority(1)
                .up_to_n_times(1)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/logs3"))
                .respond_with(ResponseTemplate::new(200))
                .with_priority(2)
                .expect(1)
                .mount(&server)
                .await;
            let batch = batch();
            let data = batch.data.clone();
            let result = coordinator(&server, policy)
                .upload(batch, capabilities())
                .await
                .unwrap();
            assert_eq!(result.requests_sent, 5);
            assert_eq!(result.rows_uploaded, 1);
            assert_eq!(result.bytes_processed, data.len());
            let requests = server.received_requests().await.unwrap();
            for request in requests
                .iter()
                .filter(|request| request.url.path() == "/object")
            {
                assert!(!request
                    .headers
                    .keys()
                    .any(|key| key.as_str() == "authorization"));
                assert!(!request
                    .headers
                    .keys()
                    .any(|key| key.as_str() == "x-bt-org-name"));
                assert_eq!(request.body, data);
            }
            let grant: Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(grant["size_bytes"], data.len());
            assert_eq!(grant["rows"][0]["object_ids"]["project_id"], "project");
        }
    }

    #[tokio::test]
    async fn retry_budget_is_shared_across_grant_object_and_reference() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3/overflow"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3/overflow"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"method":"PUT", "signedUrl":format!("{}/object",server.uri()), "key":"key"}),
            ))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/object"))
            .respond_with(ResponseTemplate::new(503))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/object"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let error = coordinator(&server, UploadPolicy::Queue { retries: 2 })
            .upload(batch(), capabilities())
            .await
            .err()
            .unwrap();
        assert!(matches!(error, BraintrustError::Api { status: 503, .. }));
        assert_eq!(server.received_requests().await.unwrap().len(), 5);
    }

    #[tokio::test]
    async fn permanent_errors_stop_at_each_overflow_stage() {
        for stage in ["grant", "object", "reference"] {
            let server = MockServer::start().await;
            let grant = if stage == "grant" {
                ResponseTemplate::new(400)
            } else {
                ResponseTemplate::new(200).set_body_json(json!({"method":"PUT","signedUrl":format!("{}/object", server.uri()),"key":"key"}))
            };
            Mock::given(method("POST"))
                .and(path("/logs3/overflow"))
                .respond_with(grant)
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("PUT"))
                .and(path("/object"))
                .respond_with(ResponseTemplate::new(if stage == "object" {
                    400
                } else {
                    200
                }))
                .expect(if stage == "grant" { 0 } else { 1 })
                .mount(&server)
                .await;
            Mock::given(method("POST"))
                .and(path("/logs3"))
                .respond_with(ResponseTemplate::new(400))
                .expect(if stage == "reference" { 1 } else { 0 })
                .mount(&server)
                .await;
            assert!(matches!(
                coordinator(&server, UploadPolicy::Queue { retries: 3 })
                    .upload(batch(), capabilities())
                    .await,
                Err(BraintrustError::Api { status: 400, .. })
            ));
        }
    }

    #[tokio::test]
    async fn signed_post_uses_multipart_boundary_and_preserves_grant_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/object"))
            .and(header("x-signed", "value"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let payload = batch().data;
        let mut grant = Logs3OverflowUpload::new("POST", format!("{}/object", server.uri()), "key");
        grant.headers = Some(
            [
                ("content-type".into(), "must-be-replaced".into()),
                ("x-signed".into(), "value".into()),
            ]
            .into(),
        );
        grant.fields = Some(
            [
                ("Content-Type".into(), "application/json".into()),
                ("key".into(), "object-key".into()),
            ]
            .into(),
        );
        coordinator(&server, UploadPolicy::Standalone)
            .logs
            .upload_logs3_overflow(&grant, payload.clone())
            .await
            .unwrap();
        let requests = server.received_requests().await.unwrap();
        let content_type = requests[0]
            .headers
            .iter()
            .find(|(key, _)| key.as_str() == "content-type")
            .unwrap()
            .1
            .to_str()
            .unwrap();
        assert!(content_type.starts_with("multipart/form-data; boundary="));
        let body = String::from_utf8(requests[0].body.clone()).unwrap();
        assert!(body.contains("name=\"file\""));
        assert!(body.contains(std::str::from_utf8(&payload).unwrap()));
        assert!(body.contains("object-key"));
    }

    #[tokio::test]
    async fn redirects_do_not_create_hidden_attempts_or_forward_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/redirected"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/redirected"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let result = coordinator(&server, UploadPolicy::Queue { retries: 2 })
            .upload(
                batch(),
                Capabilities {
                    max_request_size: usize::MAX,
                    overflow: false,
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(BraintrustError::Api { status: 307, .. })
        ));
    }

    #[tokio::test]
    async fn rate_limited_queue_upload_waits_for_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3"))
            .with_priority(1)
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200))
            .with_priority(2)
            .expect(1)
            .mount(&server)
            .await;
        let started = std::time::Instant::now();
        let result = coordinator(&server, UploadPolicy::Queue { retries: 2 })
            .upload(
                batch(),
                Capabilities {
                    max_request_size: usize::MAX,
                    overflow: false,
                },
            )
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(result.requests_sent, 2);
        assert!(
            elapsed >= Duration::from_secs(3),
            "retry should wait for Retry-After, waited {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn an_upload_429_pauses_the_whole_org_but_not_other_orgs() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
            .expect(1)
            .mount(&server)
            .await;
        let org = Scope::new(server.uri(), "throttled-org");
        let outcome = SCOPE
            .scope(
                org.clone(),
                coordinator(&server, UploadPolicy::Queue { retries: 0 }).upload(
                    batch(),
                    Capabilities {
                        max_request_size: usize::MAX,
                        overflow: false,
                    },
                ),
            )
            .await;
        assert!(outcome.is_err());
        let sibling = crate::destination::Destination::row(
            serde_json::json!({"project_id":"sibling"})
                .as_object()
                .unwrap(),
        );
        let scheduler = &crate::service::global().unwrap().attempts;
        assert!(scheduler.paused(&org.clone().with_destination(sibling.clone())));
        assert!(scheduler.paused(&org));
        assert!(!scheduler.paused(&Scope::new(server.uri(), "other-org").with_destination(sibling)));
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
            retry_delay(
                &rate_limited(Some(Duration::from_secs(20))),
                backoff,
                Pause::Destination
            ),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            retry_delay(
                &rate_limited(Some(Duration::ZERO)),
                backoff,
                Pause::Destination
            ),
            backoff
        );
        assert_eq!(
            retry_delay(&rate_limited(None), backoff, Pause::Destination),
            backoff
        );
        assert_eq!(
            retry_delay(
                &rate_limited(Some(Duration::from_secs(3600))),
                backoff,
                Pause::Destination
            ),
            Some(MAX_RETRY_AFTER)
        );
        assert_eq!(
            retry_delay(
                &rate_limited(Some(Duration::from_secs(20))),
                None,
                Pause::Destination
            ),
            None,
            "an exhausted retry budget must stay exhausted"
        );
    }
}
