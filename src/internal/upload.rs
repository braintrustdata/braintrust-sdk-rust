//! SDK ingestion orchestration, built on the one-attempt API.
use super::ingest::{EncodedBatch, UploadPlan, UploadPlanner};
use super::object_store::ObjectStoreTransport;
use super::retry::{exponential, jittered_exponential, RetryExecutor};
use crate::api::{
    self,
    logs3::{Logs3OverflowReference, Logs3OverflowRequest, Logs3OverflowUploadRequest},
};
use crate::error::{BraintrustError, Result};
use crate::types::LOGS_API_VERSION;
use std::{future::Future, time::Duration};
use tracing::warn;
use url::Url;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Capabilities {
    pub max_request_size: usize,
    pub overflow: bool,
}

/// Existing schedules remain caller policy; orchestration and classification
/// are shared. Retries are shared across all stages and splits of one batch.
#[derive(Clone, Copy)]
pub(crate) enum UploadPolicy {
    Queue { retries: usize },
    Standalone,
}
impl UploadPolicy {
    fn budget(self) -> RetryBudget {
        let (retries, delays): (_, Box<dyn Iterator<Item = Duration> + Send>) = match self {
            Self::Queue { retries } => (
                retries,
                Box::new(exponential(Duration::from_secs(1), Duration::from_secs(30))),
            ),
            Self::Standalone => (
                4,
                Box::new(jittered_exponential(
                    Duration::from_millis(300),
                    Duration::from_secs(8),
                    0.2,
                )),
            ),
        };
        RetryBudget {
            executor: RetryExecutor::new(retries, delays),
            requests: 0,
        }
    }
}
struct RetryBudget {
    executor: RetryExecutor<Box<dyn Iterator<Item = Duration> + Send>>,
    requests: usize,
}
impl RetryBudget {
    async fn run<T, F, Fut>(&mut self, mut operation: F) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let outcome = self.executor.run_shared(
            |_| operation(), retryable,
            |attempt, error, _| warn!(%error, retry = attempt.index + 1, "ingestion request failed, retrying"),
        ).await;
        self.requests += outcome.attempts;
        outcome.result
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

pub(crate) struct UploadCoordinator {
    logs: api::Transport,
    objects: ObjectStoreTransport,
    policy: UploadPolicy,
}
impl UploadCoordinator {
    pub fn new(
        http: reqwest::Client,
        api_url: &Url,
        token: &str,
        org_name: Option<&str>,
        policy: UploadPolicy,
    ) -> Result<Self> {
        let logs = api::Transport::for_scope(http.clone(), api_url, api_url, token, org_name)
            .map_err(BraintrustError::from)?;
        Ok(Self {
            logs,
            objects: ObjectStoreTransport { http },
            policy,
        })
    }

    pub async fn capabilities(
        &self,
        fallback: usize,
        override_bytes: Option<usize>,
    ) -> Capabilities {
        let server_limit = match self.logs.logs3_version().await {
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
        let planner = UploadPlanner {
            max_request_bytes: capabilities.max_request_size,
            overflow: capabilities.overflow,
        };
        let mut pending = vec![planner.plan(batch)?];
        let mut budget = self.policy.budget();
        let mut result = UploadResult::default();
        while let Some(plan) = pending.pop() {
            match plan {
                UploadPlan::Split(left, right) => {
                    pending.push(planner.plan(right)?);
                    pending.push(planner.plan(left)?);
                }
                UploadPlan::Direct(batch) => {
                    match budget
                        .run(|| async {
                            self.logs
                                .logs3_post_raw(batch.data.to_vec())
                                .await
                                .map_err(BraintrustError::from)
                        })
                        .await
                    {
                        Ok(()) => {
                            result.rows_uploaded += batch.len();
                            result.bytes_processed += batch.data.len();
                        }
                        Err(BraintrustError::Api { status: 413, .. }) => {
                            pending.push(planner.rejected(batch)?)
                        }
                        Err(error) => return Err(error),
                    }
                }
                UploadPlan::Overflow(batch) => {
                    self.overflow(&batch, &mut budget).await?;
                    result.rows_uploaded += batch.len();
                    result.bytes_processed += batch.data.len();
                }
            }
        }
        result.requests_sent = budget.requests;
        Ok(result)
    }

    async fn overflow(&self, batch: &EncodedBatch, budget: &mut RetryBudget) -> Result<()> {
        let grant = budget
            .run(|| async {
                self.logs
                    .logs3_request_overflow_upload(Logs3OverflowUploadRequest {
                        content_type: "application/json".into(),
                        size_bytes: batch.data.len(),
                        rows: batch.overflow_rows.clone(),
                    })
                    .await
                    .map_err(BraintrustError::from)
            })
            .await?;
        budget
            .run(|| self.objects.upload(&grant, batch.data.clone()))
            .await?;
        budget
            .run(|| async {
                self.logs
                    .logs3_commit_overflow(Logs3OverflowRequest {
                        rows: Logs3OverflowReference {
                            reference_type: "logs3_overflow",
                            key: grant.key.clone(),
                        },
                        api_version: LOGS_API_VERSION,
                    })
                    .await
                    .map_err(BraintrustError::from)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::logs3::Logs3OverflowUpload;
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
            crate::http::build_http_client(Duration::from_secs(3), None, None).unwrap(),
            &Url::parse(&server.uri()).unwrap(),
            "secret",
            Some("tenant"),
            policy,
        )
        .unwrap()
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
            let grant_request = requests
                .iter()
                .find(|request| request.url.path() == "/logs3/overflow")
                .expect("overflow grant request");
            let grant: Value = serde_json::from_slice(&grant_request.body).unwrap();
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
        ObjectStoreTransport {
            http: reqwest::Client::new(),
        }
        .upload(
            &Logs3OverflowUpload {
                method: "POST".into(),
                signed_url: format!("{}/object", server.uri()),
                headers: Some(
                    [
                        ("content-type".into(), "must-be-replaced".into()),
                        ("x-signed".into(), "value".into()),
                    ]
                    .into(),
                ),
                fields: Some(
                    [
                        ("Content-Type".into(), "application/json".into()),
                        ("key".into(), "object-key".into()),
                    ]
                    .into(),
                ),
                key: "key".into(),
            },
            payload.clone(),
        )
        .await
        .unwrap();
        let requests = server.received_requests().await.unwrap();
        let content_type = requests[0]
            .headers
            .iter()
            .find(|(key, _)| key.as_str() == "content-type")
            .unwrap()
            .1
            .get(0)
            .unwrap()
            .as_str();
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
}
