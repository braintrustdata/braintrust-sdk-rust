mod pending_auth;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::Notify;
use url::Url;

use crate::dataset::{
    BTQLQuery, BTQLResponse, DatasetBuilder, DatasetFetcher, DatasetRegisterRequest,
    DatasetRegisterResponse, DatasetRegistrar, DatasetSummarizer, DatasetSummaryResponse,
};
use crate::error::{BraintrustError, Result};
use crate::experiments::api::{
    BaseExperimentFetcher, BaseExperimentResponse, ExperimentComparisonFetcher,
    ExperimentComparisonResponse, ExperimentRegisterRequest, ExperimentRegisterResponse,
    ExperimentRegistrar,
};
use crate::experiments::{BaseExperimentInfo, ExperimentBuilder};
use crate::http::{api_client, api_result};
use crate::log_queue::{Credential, DroppedRows, LogQueue, TokenProvider};
use crate::span::{merge_span_origin_context, SpanOrigin, SpanOriginEnvironment, SpanSubmitter};
use crate::span_components::SpanComponents;
use crate::types::{ParentSpanInfo, SpanAttributes, SpanEventData, SpanObjectType, SpanPayload};
use backon::{ExponentialBuilder, Retryable};

mod builder;
mod login_state;

pub use builder::BraintrustClientBuilder;
pub use login_state::{LoginState, OrgInfo};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_APP_URL: &str = "https://www.braintrust.dev";
pub const DEFAULT_API_URL: &str = "https://api.braintrust.dev";

#[derive(Clone, Debug)]
pub struct BraintrustClient {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    pending_auth: Arc<pending_auth::PendingAuth>,
    #[allow(dead_code)]
    api_url: Url,
    explicit_api_url: bool,
    app_url: Url,
    queue: LogQueue,
    login_state: LoginState,
    login_notify: Notify,
    http_client: crate::http::SharedHttp,
    default_project: Option<String>,
    login_skipped: bool,
    environment: Option<SpanOriginEnvironment>,
    span_origin: Option<SpanOrigin>,
}

impl std::fmt::Debug for ClientInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientInner")
            .field("api_url", &self.api_url)
            .field("app_url", &self.app_url)
            .field("default_project", &self.default_project)
            .finish_non_exhaustive()
    }
}

impl BraintrustClient {
    /// Create a new builder for configuring the client.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use braintrust_sdk_rust::BraintrustClient;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = BraintrustClient::builder()
    ///     .api_key("sk-...")
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> BraintrustClientBuilder {
        BraintrustClientBuilder::new()
    }

    /// Check if the client is logged in.
    pub fn is_logged_in(&self) -> bool {
        self.inner.login_state.is_logged_in()
    }

    /// Wait for login to complete (useful if using background login).
    ///
    /// Returns the login state once available, or an error if login times out.
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn wait_for_login(&self) -> crate::Completion<LoginState, BraintrustError> {
        let client = self.clone();
        crate::operation::submit(async move {
            if client.inner.login_skipped {
                return Err(BraintrustError::InvalidConfig(
                    "wait_for_login() not available when skip_login is enabled".into(),
                ));
            }
            client.wait_for_login_state().await
        })
    }

    /// Get the current login state.
    pub fn login_state(&self) -> LoginState {
        self.inner.login_state.clone()
    }

    /// Create a span builder using the logged-in state and default project.
    ///
    /// This waits for login to complete if it hasn't already.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use braintrust_sdk_rust::BraintrustClient;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let client = BraintrustClient::builder()
    ///     .api_key("sk-...")
    ///     .default_project("my-project")
    ///     .build()
    ///     .await?;
    ///
    /// let span = client.span_builder().await?.build();
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn span_builder(
        &self,
    ) -> crate::Completion<crate::span::SpanBuilder<Self>, BraintrustError> {
        let client = self.clone();
        crate::operation::submit(async move {
            if client.inner.login_skipped {
                return Err(BraintrustError::InvalidConfig(
                "span_builder() requires login; use span_builder_with_credentials() when skip_login is enabled".into(),
            ));
            }
            let state = client.wait_for_login_state().await?;
            let api_key = state
                .api_key()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let org_id = state
                .org_id()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let mut builder =
                crate::span::SpanBuilder::new(Arc::new(client.clone()), &api_key, &org_id);
            if let Some(environment) = &client.inner.environment {
                builder = builder.span_origin_environment(environment.clone());
            }
            if let Some(span_origin) = &client.inner.span_origin {
                builder = builder.span_origin(span_origin.clone());
            }
            if let Some(ref project) = client.inner.default_project {
                builder = builder.project_name(project);
            }
            Ok(builder)
        })
    }

    /// Create a dataset builder using the logged-in state.
    ///
    /// This waits for login to complete if it hasn't already.
    #[must_use = "await or call blocking_wait to obtain the result"]
    pub fn dataset_builder(&self) -> crate::Completion<DatasetBuilder<Self>, BraintrustError> {
        let client = self.clone();
        crate::operation::submit(async move {
            let state = client.wait_for_login_state().await?;
            let api_key = state
                .api_key()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let org_name = state
                .org_name()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            Ok(DatasetBuilder::new(
                Arc::new(client.clone()),
                api_key,
                org_name,
            ))
        })
    }

    /// Create a span builder with explicit token and org_id.
    ///
    /// Use this for per-session / multi-tenant logging: the supplied credentials
    /// travel with each span through the upload pipeline and are NOT written into
    /// the client's shared login state. Spans created from the same client with
    /// different `(token, org_id)` pairs each upload with their own credentials.
    pub fn span_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder<Self> {
        self.span_builder_for(Credential::token_value(token.into()), org_id)
    }

    /// Create a span builder that resolves its credential when spans are uploaded.
    ///
    /// Reuse the same provider for spans with the same authorization scope to
    /// batch their uploads. See [`TokenProvider`] for lookup timing.
    pub fn span_builder_with_token_provider(
        &self,
        provider: Arc<dyn TokenProvider>,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder<Self> {
        self.span_builder_for(Credential::Provider(provider), org_id)
    }

    fn span_builder_for(
        &self,
        credential: Credential,
        org_id: impl Into<String>,
    ) -> crate::span::SpanBuilder<Self> {
        let submitter = Arc::new(self.clone());
        let mut builder =
            crate::span::SpanBuilder::new_with_credential(submitter, credential, org_id);
        if let Some(environment) = &self.inner.environment {
            builder = builder.span_origin_environment(environment.clone());
        }
        if let Some(span_origin) = &self.inner.span_origin {
            builder = builder.span_origin(span_origin.clone());
        }
        builder
    }

    /// Update an existing span using the output of `SpanHandle::export()`.
    ///
    /// Validation happens before queuing. The actual upload remains asynchronous.
    pub fn update_span(
        &self,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> crate::Completion<(), BraintrustError> {
        let reservation = match self.inner.pending_auth.reserve() {
            Ok(reservation) => reservation,
            Err(error) => return crate::Completion::finished(Err(error)),
        };
        let client = self.clone();
        let exported = exported.to_owned();
        crate::operation::submit(async move {
            let auth = async {
                let login_state = if client.inner.login_state.is_logged_in() {
                client.inner.login_state.clone()
            } else if client.inner.login_skipped {
                return Err(BraintrustError::InvalidConfig(
                "update_span() requires credentials; call span_builder_with_credentials() or experiment_builder_with_credentials() first when skip_login is enabled".into(),
            ));
            } else {
                client.wait_for_login_state().await?
            };

            let api_key = login_state
                .api_key()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let org_id = login_state
                .org_id()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
                Ok((api_key, org_id, login_state.org_name()))
            }.await;
            reservation.publish(|| {
                let (api_key, org_id, org_name) = auth?;
                client
                    .update_span_internal(api_key, org_id, org_name, &exported, event)
                    .map(|write| vec![write])
            })
        })
    }

    /// Update an existing span using explicit credentials instead of shared login state.
    ///
    /// This is the safe entrypoint for multi-tenant `skip_login` clients: the
    /// supplied credentials travel with the queued row and are NOT written into
    /// the client's shared login state.
    pub fn update_span_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> Result<()> {
        let token = token.into();
        let org_id = org_id.into();

        self.update_span_with_credentials_tracked(token, org_id, exported, event)
            .map(|_| ())
    }

    /// Update an existing span and retain its eventual delivery result.
    ///
    /// Validation and admission are performed before this method returns. The
    /// returned completion observes delivery even if it finishes before a flush.
    pub fn update_span_with_credentials_tracked(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> Result<crate::Completion<crate::DeliveryReceipt, crate::WriteError>> {
        self.inner
            .pending_auth
            .direct(|| {
                self.update_span_internal(token.into(), org_id.into(), None, exported, event)
            })
            .unwrap_or_else(|| Err(pending_auth::closed()))
    }

    fn update_span_internal(
        &self,
        token: String,
        org_id: String,
        org_name: Option<String>,
        exported: &str,
        event: crate::span::SpanLog,
    ) -> Result<crate::Completion<crate::DeliveryReceipt, crate::WriteError>> {
        let components = SpanComponents::parse(exported)?;
        let row_id = components.row_id.clone().ok_or_else(|| {
            BraintrustError::InvalidConfig("Exported span must have a row_id".into())
        })?;

        if components.root_span_id.is_some() != components.span_id.is_some() {
            return Err(BraintrustError::InvalidConfig(
                "both root_span_id and span_id must be set, or neither".into(),
            ));
        }

        if components.object_id.is_none() {
            match components.object_type {
                SpanObjectType::ProjectLogs => {
                    if components.project_logs_identifier().is_none() {
                        return Err(BraintrustError::InvalidConfig(
                            "project-log compute_object_metadata_args must include project_id or project_name".into(),
                        ));
                    }
                }
                SpanObjectType::Experiment => {
                    return Err(BraintrustError::InvalidConfig(
                        "Exported experiment span must include object_id".into(),
                    ));
                }
                SpanObjectType::PlaygroundLogs => {
                    return Err(BraintrustError::InvalidConfig(
                        "Exported playground span must include object_id".into(),
                    ));
                }
            }
        }

        let span_id = components.span_id.clone().unwrap_or_else(|| row_id.clone());
        let mut event_data = SpanEventData {
            input: event.input,
            output: event.output,
            expected: event.expected,
            error: event.error,
            scores: event.scores,
            metadata: event.metadata,
            metrics: event.metrics,
            tags: event.tags,
            context: merge_span_origin_context(
                event.context,
                self.inner.environment.as_ref(),
                event
                    .span_origin
                    .as_ref()
                    .or(self.inner.span_origin.as_ref()),
                event.skip_span_origin,
            ),
            span_attributes: {
                let extra = event.span_attributes_extra.unwrap_or_default();
                if event.name.is_some() || !extra.is_empty() {
                    Some(SpanAttributes {
                        name: event.name,
                        span_type: None,
                        purpose: None,
                        extra,
                    })
                } else {
                    None
                }
            },
            extra: HashMap::new(),
        };
        if let Some(propagated_event) = components.propagated_event.as_ref() {
            event_data.apply_propagated_event(propagated_event);
        }

        let payload = SpanPayload {
            row_id,
            span_id,
            is_merge: true,
            span_components: Some(components),
            org_id,
            org_name,
            project_name: None,
            input: event_data.input,
            output: event_data.output,
            expected: event_data.expected,
            error: event_data.error,
            scores: event_data.scores,
            metadata: event_data.metadata,
            metrics: event_data.metrics,
            tags: event_data.tags,
            context: event_data.context,
            span_attributes: event_data.span_attributes,
            extra: event_data.extra,
        };

        let completion = self
            .inner
            .queue
            .submit(Credential::token_value(token), payload, None);
        if let Err(error) = completion.submission() {
            return Err(BraintrustError::Background(error.to_string()));
        }
        Ok(completion)
    }

    fn request_client(&self, token: &str) -> Result<crate::api::Client> {
        let api_url = self
            .inner
            .login_state
            .api_url()
            .unwrap_or_else(|| self.inner.api_url.to_string());
        let app_url = self
            .inner
            .login_state
            .app_url()
            .unwrap_or_else(|| self.inner.app_url.to_string());
        api_client(
            &self.inner.http_client.client(),
            &api_url,
            &app_url,
            token,
            None,
        )
    }

    /// Log in and discover the selected organization's API endpoint.
    async fn perform_login(&self, api_key: &str, org_name: Option<&str>) -> Result<()> {
        let http = self.inner.http_client.client();
        let mut builder = crate::api::Client::builder()
            .api_key(api_key)
            .app_url(self.inner.app_url.as_str())
            .http_client(http.clone())
            .upload_http_client(http);
        if self.inner.explicit_api_url {
            builder = builder.api_url(self.inner.api_url.as_str());
        }
        if let Some(org_name) = org_name {
            builder = builder.org_name(org_name);
        }
        let result = builder.build_with_login().await;
        let client = match result {
            Ok(client) => client,
            Err(crate::api::ClientInitError::Login(error)) => {
                return api_result::<()>(Err(*error)).await;
            }
            Err(error) => return Err(BraintrustError::InvalidConfig(error.to_string())),
        };
        let org = client.organization().ok_or_else(|| {
            BraintrustError::InvalidConfig("login did not select an organization".into())
        })?;
        let did_set = self.inner.login_state.set(
            api_key.to_owned(),
            org.id.clone(),
            org.name.clone(),
            client.baseurl().to_owned(),
            client.app_url().to_owned(),
        );
        if !did_set {
            return Err(BraintrustError::InvalidConfig(
                "Login state already set".to_string(),
            ));
        }
        self.inner.login_notify.notify_waiters();
        Ok(())
    }

    /// Start login in a background task with retry logic.
    fn start_background_login(&self, api_key: String, org_name: Option<String>) {
        let client = self.clone();
        tokio::spawn(async move {
            const MAX_ATTEMPTS: usize = 10;
            let result = (|| client.perform_login(&api_key, org_name.as_deref()))
                .retry(ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(100))
                    .with_max_delay(Duration::from_secs(5))
                    .with_max_times(MAX_ATTEMPTS - 1))
                .notify(|error, delay| tracing::warn!(%error, ?delay, "Background login failed, retrying"))
                .await;
            match result {
                Ok(()) => tracing::debug!("Background login completed successfully"),
                Err(error) => tracing::error!(
                    "Background login failed after {} attempts: {}. \
                     Spans will not be uploaded until the client is re-created.",
                    MAX_ATTEMPTS,
                    error
                ),
            }
        });
    }

    /// Wait for login state to be available.
    async fn wait_for_login_state(&self) -> Result<LoginState> {
        // Check if already logged in
        if self.inner.login_state.is_logged_in() {
            return Ok(self.inner.login_state.clone());
        }

        // Get notification future BEFORE checking state to avoid race condition
        let notified = self.inner.login_notify.notified();

        // Check state again (may have been set between our first check and now)
        if self.inner.login_state.is_logged_in() {
            return Ok(self.inner.login_state.clone());
        }

        // Wait for notification or timeout
        tokio::select! {
            _ = notified => {
                // Login completed - return the login state
                if self.inner.login_state.is_logged_in() {
                    Ok(self.inner.login_state.clone())
                } else {
                    Err(BraintrustError::InvalidConfig(
                        "Login notification received but state not set".into(),
                    ))
                }
            }
            _ = tokio::time::sleep(LOGIN_TIMEOUT) => {
                Err(BraintrustError::InvalidConfig(
                    "Timeout waiting for login to complete".into(),
                ))
            }
        }
    }

    /// Create an experiment builder with explicit credentials.
    ///
    /// Note: This will be deprecated in favor of `experiment_builder()` once
    /// the login-flow branch lands and credentials are stored in the client.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let experiment = client
    ///     .experiment_builder_with_credentials(&api_key, &org_id)
    ///     .project_name("my-project")
    ///     .experiment_name("baseline")
    ///     .build()?;
    /// ```
    pub fn experiment_builder_with_credentials(
        &self,
        token: impl Into<String>,
        org_id: impl Into<String>,
    ) -> ExperimentBuilder<Self> {
        let token = token.into();
        let org_id = org_id.into();

        // Populate login state with explicit credentials so flush_internal() can send data.
        let _ = self.inner.login_state.set(
            token.clone(),
            org_id.clone(),
            String::new(), // org_name unknown without login
            self.inner.api_url.to_string(),
            self.inner.app_url.to_string(),
        );

        let submitter = Arc::new(self.clone());
        ExperimentBuilder::new(submitter, token, org_id)
    }

    /// Submit immediately; the token observes admission and full batch delivery.
    fn submit_payload(
        &self,
        credential: Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.inner
            .pending_auth
            .direct(|| self.inner.queue.submit(credential, payload, parent_info))
            .unwrap_or_else(|| crate::Completion::finished(Err(crate::EnqueueError::Closed.into())))
    }

    /// Queue caller-built rows for upload to `/logs3` using the logged-in credentials.
    ///
    /// See [`BraintrustClient::log_rows_with_credentials`] for how rows are uploaded.
    /// This waits for login to complete if it hasn't already.
    pub fn log_rows(
        &self,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) -> crate::Completion<(), BraintrustError> {
        let reservation = match self.inner.pending_auth.reserve() {
            Ok(reservation) => reservation,
            Err(error) => return crate::Completion::finished(Err(error)),
        };
        let client = self.clone();
        let rows: Vec<_> = rows.into_iter().collect();
        crate::operation::submit(async move {
            let auth = async {
            if client.inner.login_skipped {
                return Err(BraintrustError::InvalidConfig(
                "log_rows() requires login; use log_rows_with_credentials() when skip_login is enabled".into(),
            ));
            }
            let state = client.wait_for_login_state().await?;
            let api_key = state
                .api_key()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
            let org_id = state
                .org_id()
                .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
                Ok((api_key, org_id, state.org_name()))
            }.await;
            reservation.publish(|| {
                let (api_key, org_id, org_name) = auth?;
                Ok(client.inner.queue.submit_rows_tracked(
                    &api_key,
                    &org_id,
                    org_name.as_deref(),
                    rows,
                ))
            })
        })
    }

    /// Queue caller-built rows for upload to `/logs3` with explicit credentials.
    ///
    /// Rows are uploaded as given: the SDK does not register projects, resolve
    /// destinations, or merge rows that share an id, so each row must already carry
    /// the fields the server expects. Like
    /// [`BraintrustClient::span_builder_with_credentials`], the credentials travel
    /// with the rows and never touch the shared login state, and rows with different
    /// `token`/`org_id` pairs are never sent in the same request. `org_name`, when
    /// set, is sent as the `x-bt-org-name` header.
    ///
    /// Returns immediately; the background worker batches and uploads the rows. Use
    /// [`BraintrustClient::flush`] to wait for delivery and
    /// [`BraintrustClient::dropped_rows`] to observe rows that were discarded.
    pub fn log_rows_with_credentials(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        let rows: Vec<_> = rows.into_iter().collect();
        self.inner
            .pending_auth
            .direct(|| self.inner.queue.submit_rows(token, org_id, org_name, rows));
    }

    /// Queue caller-built rows for upload to `/logs3`, fetching the token from
    /// `provider` when they are sent.
    ///
    /// Behaves like [`BraintrustClient::log_rows_with_credentials`], except that the
    /// token is requested when the rows are uploaded rather than when they are
    /// queued, so rows that wait in the queue still go out with a current token.
    /// Rows are batched together only if they share the provider `Arc` and
    /// `org_id`. See [`TokenProvider`] for how the provider is called.
    pub fn log_rows_with_token_provider(
        &self,
        provider: Arc<dyn TokenProvider>,
        org_id: &str,
        org_name: Option<&str>,
        rows: impl IntoIterator<Item = Map<String, Value>>,
    ) {
        let rows: Vec<_> = rows.into_iter().collect();
        self.inner.pending_auth.direct(|| {
            self.inner
                .queue
                .submit_rows_with_provider(provider, org_id, org_name, rows)
        });
    }

    /// Cumulative counts of rows this client discarded instead of uploading.
    pub fn dropped_rows(&self) -> DroppedRows {
        self.inner.queue.dropped_rows()
    }

    /// Stop accepting writes, drain accepted work and stop the delivery worker.
    /// A timeout fails remaining completions and discards active SDK state.
    pub fn shutdown(
        &self,
        timeout: std::time::Duration,
    ) -> crate::Completion<crate::ShutdownReport, crate::ShutdownError> {
        let started = tokio::time::Instant::now();
        let (result, pending) = self.inner.pending_auth.snapshot(true, |empty| {
            if empty {
                Ok(self.inner.queue.shutdown(timeout))
            } else {
                Err(self.inner.queue.flush())
            }
        });
        let barrier = match result {
            Ok(shutdown) => return shutdown,
            Err(barrier) => barrier,
        };
        let client = self.clone();
        crate::operation::submit(async move {
            match tokio::time::timeout(
                timeout.saturating_sub(started.elapsed()),
                pending_auth::join(barrier, pending),
            )
            .await
            {
                Ok(result) => {
                    client
                        .inner
                        .queue
                        .shutdown(timeout.saturating_sub(started.elapsed()))
                        .await?;
                    result
                        .map(|report| crate::ShutdownReport {
                            delivered: report.delivered,
                        })
                        .map_err(Into::into)
                }
                Err(_) => {
                    client.inner.pending_auth.abort();
                    let _ = client.inner.queue.shutdown(std::time::Duration::ZERO).await;
                    Err(crate::ExecutionError::TimedOut.into())
                }
            }
        })
    }

    /// Capture a cutoff immediately and observe delivery of the covered batch.
    /// New writes belong to later work; completed failures are never replayed.
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        let (barrier, pending) = self
            .inner
            .pending_auth
            .snapshot(false, |_| self.inner.queue.flush());
        if pending.is_empty() {
            barrier
        } else {
            crate::operation::submit(pending_auth::join(barrier, pending))
        }
    }

    /// Trigger a non-blocking background flush.
    /// Does not wait for completion - useful for streaming writes.
    pub fn trigger_flush(&self) -> crate::Completion<(), BraintrustError> {
        let client = self.clone();
        crate::operation::submit(async move { client.inner.queue.trigger_flush_command().await })
    }
}

#[async_trait::async_trait]
impl SpanSubmitter for BraintrustClient {
    fn submit(
        &self,
        credential: Credential,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    ) -> crate::Completion<crate::DeliveryReceipt, crate::WriteError> {
        self.submit_payload(credential, payload, parent_info)
    }

    fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        BraintrustClient::flush(self)
    }

    async fn trigger_flush(&self) -> Result<()> {
        BraintrustClient::trigger_flush(self).await
    }
}

#[async_trait::async_trait]
impl ExperimentRegistrar for BraintrustClient {
    async fn register_experiment(
        &self,
        token: &str,
        request: ExperimentRegisterRequest,
    ) -> Result<ExperimentRegisterResponse> {
        let client = self.request_client(token)?;
        Ok(api_result(client.register_experiment(&request).await)
            .await?
            .into_inner())
    }
}

#[async_trait::async_trait]
impl BaseExperimentFetcher for BraintrustClient {
    async fn fetch_base_experiment(
        &self,
        token: &str,
        experiment_id: &str,
    ) -> Result<Option<BaseExperimentInfo>> {
        let client = self.request_client(token)?;
        let response = api_result(client.base_experiment(experiment_id).await).await;
        let base_response: BaseExperimentResponse = match response {
            Ok(response) => response.into_inner(),
            Err(BraintrustError::Api { status: 400, .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        Ok(
            match (base_response.base_exp_id, base_response.base_exp_name) {
                (Some(id), Some(name)) => Some(BaseExperimentInfo { id, name }),
                _ => None,
            },
        )
    }
}

#[async_trait::async_trait]
impl ExperimentComparisonFetcher for BraintrustClient {
    async fn fetch_experiment_comparison(
        &self,
        token: &str,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> Result<ExperimentComparisonResponse> {
        let client = self.request_client(token)?;
        let response = api_result(
            client
                .experiment_comparison(experiment_id, base_experiment_id)
                .await,
        )
        .await?;
        Ok(response.into_inner())
    }
}

#[async_trait::async_trait]
impl DatasetRegistrar for BraintrustClient {
    async fn register_dataset(
        &self,
        token: &str,
        request: DatasetRegisterRequest,
    ) -> Result<DatasetRegisterResponse> {
        let client = self.request_client(token)?;
        Ok(api_result(client.register_dataset(&request).await)
            .await?
            .into_inner())
    }
}

#[async_trait::async_trait]
impl DatasetFetcher for BraintrustClient {
    async fn fetch_dataset_records(&self, token: &str, query: BTQLQuery) -> Result<BTQLResponse> {
        let client = self.request_client(token)?;
        let response = api_result(client.btql_json_as::<_, BTQLResponse>(&query).await)
            .await?
            .into_inner();
        Ok(response)
    }
}

#[async_trait::async_trait]
impl DatasetSummarizer for BraintrustClient {
    async fn fetch_dataset_summary(
        &self,
        token: &str,
        dataset_id: &str,
    ) -> Result<DatasetSummaryResponse> {
        let client = self.request_client(token)?;
        let response = api_result(client.dataset_summary(dataset_id).await).await?;
        Ok(response.into_inner())
    }
}

#[allow(dead_code)]
#[derive(Debug, Default)]
struct MutableSpanEvent {
    input: Option<Value>,
    output: Option<Value>,
    expected: Option<Value>,
    error: Option<Value>,
    scores: Option<HashMap<String, f64>>,
    metadata: Option<Map<String, Value>>,
    metrics: Option<HashMap<String, f64>>,
    tags: Option<Vec<String>>,
    context: Option<Value>,
    span_attributes: Option<crate::types::SpanAttributes>,
    extra: HashMap<String, Value>,
}

#[allow(dead_code)]
#[derive(Serialize)]
struct SerializableSpanEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scores: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metadata: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_attributes: Option<Value>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

impl MutableSpanEvent {
    fn to_event_map(&self) -> Map<String, Value> {
        let serialized = SerializableSpanEvent {
            input: self.input.clone(),
            output: self.output.clone(),
            expected: self.expected.clone(),
            error: self.error.clone(),
            scores: self.scores.as_ref().map(numeric_map_to_value),
            metadata: self.metadata.clone(),
            metrics: self.metrics.as_ref().map(numeric_map_to_value),
            tags: self.tags.clone(),
            context: self.context.clone(),
            span_attributes: self
                .span_attributes
                .clone()
                .and_then(|value| serde_json::to_value(value).ok()),
            extra: self.extra.clone(),
        };
        serde_json::to_value(serialized)
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    fn apply_merged_event_map(&mut self, mut event: Map<String, Value>) {
        self.input = event.remove("input");
        self.output = event.remove("output");
        self.expected = event.remove("expected");
        self.error = event.remove("error");
        self.scores = event.remove("scores").and_then(parse_numeric_map);
        self.metadata = event.remove("metadata").and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        });
        self.metrics = event.remove("metrics").and_then(parse_numeric_map);
        self.tags = event.remove("tags").and_then(parse_string_array);
        self.context = event.remove("context");
        self.span_attributes = event
            .remove("span_attributes")
            .and_then(|value| serde_json::from_value(value).ok());
        self.extra = HashMap::from_iter(event);
    }
}

#[allow(dead_code)]
fn numeric_map_to_value(map: &HashMap<String, f64>) -> Value {
    Value::Object(Map::from_iter(
        map.iter()
            .map(|(key, value)| (key.clone(), Value::from(*value))),
    ))
}

#[allow(dead_code)]
fn apply_propagated_event(
    propagated_event: &Map<String, Value>,
    event_fields: &mut MutableSpanEvent,
) {
    let mut event = event_fields.to_event_map();
    merge_event_maps(&mut event, propagated_event);
    event_fields.apply_merged_event_map(event);
}

#[allow(dead_code)]
fn merge_event_maps(merge_into: &mut Map<String, Value>, merge_from: &Map<String, Value>) {
    for (key, merge_from_value) in merge_from {
        match (merge_into.get_mut(key), merge_from_value) {
            (Some(Value::Object(merge_into_object)), Value::Object(merge_from_object)) => {
                merge_event_maps(merge_into_object, merge_from_object);
            }
            (Some(Value::Array(merge_into_array)), Value::Array(merge_from_array))
                if key == "tags" =>
            {
                for item in merge_from_array {
                    if !merge_into_array.iter().any(|existing| existing == item) {
                        merge_into_array.push(item.clone());
                    }
                }
            }
            _ => {
                merge_into.insert(key.clone(), merge_from_value.clone());
            }
        }
    }
}

#[allow(dead_code)]
fn parse_numeric_map(value: Value) -> Option<HashMap<String, f64>> {
    match value {
        Value::Object(map) => Some(HashMap::from_iter(
            map.into_iter()
                .filter_map(|(key, value)| value.as_f64().map(|number| (key, number))),
        )),
        _ => None,
    }
}

#[allow(dead_code)]
fn parse_string_array(value: Value) -> Option<Vec<String>> {
    match value {
        Value::Array(values) => Some(
            values
                .into_iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::SpanLog;
    use serde_json::Value;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Helper to create a mock login response
    fn mock_login_response(orgs: &[(&str, &str)]) -> ResponseTemplate {
        let org_info: Vec<_> = orgs
            .iter()
            .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
            .collect();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "org_info": org_info }))
    }

    #[tokio::test]
    async fn builder_capacity_is_enforced_even_with_the_legacy_unbounded_setting() {
        let client = BraintrustClient::builder()
            .app_url("https://example.com")
            .api_url("https://example.com")
            .skip_login(true)
            .with_queue_max_size(1)
            .enforce_queue_size_limit(false)
            .build()
            .await
            .expect("client");

        let span = client.span_builder_with_credentials("token", "org").build();
        assert!(span.log(crate::SpanLog::default()).is_ok());
        assert!(matches!(
            &*span.log(crate::SpanLog::default()),
            Err(crate::WriteError::Admission(crate::EnqueueError::Full))
        ));
    }

    #[tokio::test]
    async fn project_registration_is_cached() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "test-project-id" }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("token")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        for _ in 0..2 {
            let span = client
                .span_builder()
                .await
                .expect("span_builder")
                // Other tests register "demo-project" in the same org, and a reused
                // mock-server port would share their cache entry.
                .project_name("project-registration-is-cached")
                .build();
            span.log(
                SpanLog::builder()
                    .input(Value::String("hello".into()))
                    .build()
                    .expect("build"),
            );
            span.flush().await.expect("flush");
            client.flush().await.expect("client flush");
        }

        let register_calls = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/api/project/register")
            .count();

        assert_eq!(register_calls, 1);
    }

    #[tokio::test]
    async fn logs_request_contains_span_rows() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("token")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let span = client
            .span_builder()
            .await
            .expect("span_builder")
            .project_name("demo-project")
            .build();
        span.log(
            SpanLog::builder()
                .input(Value::String("input".into()))
                .build()
                .expect("build"),
        );
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        assert!(body.get("rows").is_some());
    }

    #[tokio::test]
    async fn blocking_login_returns_first_org_by_default() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[
                ("org-1", "First Org"),
                ("org-2", "Second Org"),
            ]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let login_state = client.login_state();
        assert!(login_state.is_logged_in(), "should be logged in");
        assert_eq!(login_state.org_id().as_deref(), Some("org-1"));
        assert_eq!(login_state.org_name().as_deref(), Some("First Org"));
        assert_eq!(login_state.api_key().as_deref(), Some("test-api-key"));
    }

    #[tokio::test]
    async fn blocking_login_selects_org_by_name() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[
                ("org-1", "First Org"),
                ("org-2", "Second Org"),
            ]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .org_name("Second Org")
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let login_state = client.login_state();
        assert!(login_state.is_logged_in(), "should be logged in");
        assert_eq!(login_state.org_id().as_deref(), Some("org-2"));
        assert_eq!(login_state.org_name().as_deref(), Some("Second Org"));
    }

    #[tokio::test]
    async fn blocking_login_errors_when_org_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-1", "First Org")]))
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .org_name("Nonexistent Org")
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn blocking_login_errors_when_no_orgs_returned() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "org_info": [] })),
            )
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn blocking_login_errors_on_api_failure() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let result = BraintrustClient::builder()
            .api_key("bad-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .blocking_login(true)
            .build()
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BraintrustError::Api { status: 401, .. }));
    }

    #[tokio::test]
    async fn is_logged_in_returns_false_initially_with_background_login() {
        let server = MockServer::start().await;

        // Don't mount any mock - login will fail and retry in background
        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            // blocking_login defaults to false
            .build()
            .await
            .expect("client");

        // Initially should not be logged in (background login hasn't completed)
        let is_logged_in = client.is_logged_in();
        assert!(!is_logged_in);
    }

    #[tokio::test]
    async fn wait_for_login_succeeds_after_background_login() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-1", "Test Org")]))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .build()
            .await
            .expect("client");

        // Wait for background login to complete
        let login_state = client.wait_for_login().await.expect("login should succeed");
        assert_eq!(login_state.org_id().as_deref(), Some("org-1"));
        assert!(client.is_logged_in());
    }

    #[tokio::test]
    async fn span_builder_uses_login_state_and_default_project() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-123", "Test Org")]))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .default_project("test-project")
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        // span_builder() should use login state and default project
        let span = client.span_builder().await.expect("span_builder").build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        });
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify the logs request was made with the correct org_id
        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        let rows = body.get("rows").and_then(|r| r.as_array()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("org_id").and_then(|v| v.as_str()),
            Some("org-123")
        );
    }

    #[tokio::test]
    async fn span_builder_with_credentials_bypasses_login() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        // Create client without any login mock - background login will fail
        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(server.uri())
            .api_url(server.uri())
            .build()
            .await
            .expect("client");

        // Use span_builder_with_credentials to bypass login
        let span = client
            .span_builder_with_credentials("explicit-token", "explicit-org-id")
            .project_name("demo-project")
            .build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        });
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify the logs request was made with the explicit org_id
        let logs_request = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.url.path() == "/logs3")
            .expect("logs request present");
        let body: Value = serde_json::from_slice(&logs_request.body).expect("json");
        let rows = body.get("rows").and_then(|r| r.as_array()).unwrap();
        assert_eq!(
            rows[0].get("org_id").and_then(|v| v.as_str()),
            Some("explicit-org-id")
        );
    }

    #[tokio::test]
    async fn per_session_credentials_do_not_leak_and_batch_separately() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;

        // skip_login: the client never logs in, so any credential leak into the
        // shared login state would be observable via is_logged_in().
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        // Two sessions with different (token, org_id). Use ProjectLogs parents so no
        // project registration HTTP call is needed.
        let span_a = client
            .span_builder_with_credentials("token-a", "org-a")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-a".into(),
            })
            .build();
        let span_b = client
            .span_builder_with_credentials("token-b", "org-b")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "proj-b".into(),
            })
            .build();

        span_a.log(SpanLog {
            input: Some(Value::String("a".into())),
            ..Default::default()
        });
        span_b.log(SpanLog {
            input: Some(Value::String("b".into())),
            ..Default::default()
        });
        span_a.flush().await.expect("flush a");
        span_b.flush().await.expect("flush b");
        client.flush().await.expect("client flush");

        // No credential leak into shared global state.
        assert!(
            !client.is_logged_in(),
            "per-request credentials must not populate the shared login state"
        );

        // Each /logs3 request must carry its own bearer token and matching org_id,
        // and a single request must never mix the two orgs (batched by credentials).
        let requests = server.received_requests().await.unwrap();
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for request in requests.iter().filter(|r| r.url.path() == "/logs3") {
            let auth = request
                .headers
                .get("authorization")
                .map(|value| {
                    value
                        .to_str()
                        .expect("valid authorization header")
                        .to_owned()
                })
                .expect("authorization header");
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            let rows = body.get("rows").and_then(|r| r.as_array()).expect("rows");
            let orgs: std::collections::HashSet<&str> = rows
                .iter()
                .filter_map(|row| row.get("org_id").and_then(Value::as_str))
                .collect();
            assert_eq!(orgs.len(), 1, "one request must not mix orgs: {orgs:?}");
            seen.insert(auth, orgs.into_iter().next().unwrap().to_string());
        }

        // Each token uploaded exactly its own org's rows, and never the other's.
        let org_for = |token: &str| -> Option<&String> {
            seen.iter()
                .find(|(auth, _)| auth.contains(token))
                .map(|(_, org)| org)
        };
        assert_eq!(org_for("token-a").map(String::as_str), Some("org-a"));
        assert_eq!(org_for("token-b").map(String::as_str), Some("org-b"));
    }

    #[tokio::test]
    async fn log_rows_with_credentials_uploads_rows_as_given() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        let row = |value: Value| value.as_object().expect("object").clone();
        let rows_a = vec![
            row(serde_json::json!({"id": "same", "project_id": "p", "metrics": {"n": 1}})),
            row(serde_json::json!({"id": "same", "project_id": "p", "metrics": {"n": 2}})),
        ];
        let rows_b = vec![row(serde_json::json!({"id": "other", "custom": [1, 2]}))];
        client.log_rows_with_credentials("token-a", "org-a", Some("org-name-a"), rows_a.clone());
        client.log_rows_with_credentials("token-b", "org-b", None, rows_b.clone());
        client.flush().await.expect("flush");

        assert!(!client.is_logged_in());
        assert_eq!(client.dropped_rows(), DroppedRows::default());
        let requests = server.received_requests().await.unwrap();
        let mut uploaded = HashMap::new();
        for request in requests.iter().filter(|r| r.url.path() == "/logs3") {
            let auth = request.headers["authorization"]
                .to_str()
                .unwrap()
                .to_owned();
            let org_name = request
                .headers
                .get("x-bt-org-name")
                .map(|value| value.to_str().unwrap().to_owned());
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            let rows: Vec<Value> = body["rows"].as_array().expect("rows").clone();
            uploaded
                .entry((auth, org_name))
                .or_insert_with(Vec::new)
                .extend(rows);
        }
        let as_values = |rows: Vec<Map<String, Value>>| -> Vec<Value> {
            rows.into_iter().map(Value::Object).collect()
        };
        assert_eq!(uploaded.len(), 2, "one upload per credential: {uploaded:?}");
        assert_eq!(
            uploaded[&("Bearer token-a".to_owned(), Some("org-name-a".to_owned()))],
            as_values(rows_a),
            "rows sharing an id are not merged and are not modified"
        );
        assert_eq!(
            uploaded[&("Bearer token-b".to_owned(), None)],
            as_values(rows_b)
        );
    }

    #[tokio::test]
    async fn build_without_login_uploads_with_per_call_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .build_without_login()
            .expect("client");

        let row = serde_json::json!({"id": "row"})
            .as_object()
            .unwrap()
            .clone();
        client.log_rows_with_credentials("token", "org", None, [row]);
        client.flush().await.expect("flush");

        assert!(!client.is_logged_in());
        let auths: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/logs3")
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(auths, ["Bearer token"]);
    }

    #[tokio::test]
    async fn log_rows_with_token_provider_fetches_the_token_when_sending() {
        // Delivery starts immediately, so the gate holds sends until the token rotates.
        struct Rotating {
            token: std::sync::Mutex<String>,
            calls: std::sync::atomic::AtomicUsize,
            gate: tokio::sync::Semaphore,
        }
        #[async_trait::async_trait]
        impl TokenProvider for Rotating {
            async fn token(
                &self,
            ) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
                drop(self.gate.acquire().await?);
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self.token.lock().unwrap().clone())
            }
        }
        let provider = |token: &str| {
            Arc::new(Rotating {
                token: std::sync::Mutex::new(token.to_owned()),
                calls: Default::default(),
                gate: tokio::sync::Semaphore::new(0),
            })
        };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .checked_delivery(true)
            .build()
            .await
            .expect("client");

        let row = |id: &str| serde_json::json!({ "id": id }).as_object().unwrap().clone();
        let rotating = provider("first");
        let other = provider("other");
        // One call is admitted atomically, so its rows share a job and a token fetch.
        client.log_rows_with_token_provider(
            rotating.clone(),
            "org",
            Some("org-name"),
            [row("a"), row("b")],
        );
        client.log_rows_with_token_provider(other.clone(), "org", Some("org-name"), [row("c")]);
        assert_eq!(rotating.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        *rotating.token.lock().unwrap() = "rotated".to_owned();
        rotating.gate.add_permits(1);
        other.gate.add_permits(1);
        client.flush().await.expect("flush");

        assert_eq!(client.dropped_rows(), DroppedRows::default());
        assert_eq!(rotating.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let mut uploaded = HashMap::new();
        for request in server.received_requests().await.unwrap() {
            if request.url.path() != "/logs3" {
                continue;
            }
            let body: Value = serde_json::from_slice(&request.body).expect("json");
            let ids: Vec<String> = body["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .map(|row| row["id"].as_str().unwrap().to_owned())
                .collect();
            let auth = request.headers["authorization"]
                .to_str()
                .unwrap()
                .to_owned();
            assert!(
                uploaded.insert(auth, ids).is_none(),
                "one request per provider"
            );
        }
        assert_eq!(
            uploaded,
            HashMap::from([
                (
                    "Bearer rotated".to_owned(),
                    vec!["a".to_owned(), "b".to_owned()]
                ),
                ("Bearer other".to_owned(), vec!["c".to_owned()]),
            ])
        );
    }

    // Spans resolve their shared credential when uploaded, once per batch.
    #[tokio::test]
    async fn span_builder_with_token_provider_resolves_at_upload() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        // Delivery starts immediately, so the gate holds sends until the token rotates.
        struct State {
            rotated: AtomicBool,
            calls: AtomicUsize,
            gate: tokio::sync::Semaphore,
        }
        struct Rotating(Arc<State>);
        #[async_trait::async_trait]
        impl TokenProvider for Rotating {
            async fn token(
                &self,
            ) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
                drop(self.0.gate.acquire().await?);
                self.0.calls.fetch_add(1, Ordering::SeqCst);
                Ok(if self.0.rotated.load(Ordering::SeqCst) {
                    "refreshed"
                } else {
                    "expired"
                }
                .into())
            }
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer refreshed",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1..=2)
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .api_url(server.uri())
            .checked_delivery(true)
            .build_without_login()
            .unwrap();
        let state = Arc::new(State {
            rotated: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            gate: tokio::sync::Semaphore::new(0),
        });
        let builder = client
            .span_builder_with_token_provider(Arc::new(Rotating(state.clone())), "org")
            .parent_info(ParentSpanInfo::ProjectLogs {
                object_id: "project".into(),
            });
        for id in ["first", "second"] {
            let span = builder.clone().row_id(id).build();
            span.log(crate::SpanLog::builder().name(id).build().unwrap());
        }

        assert_eq!(state.calls.load(Ordering::SeqCst), 0);
        state.rotated.store(true, Ordering::SeqCst);
        state.gate.add_permits(1);
        client.flush().await.unwrap();

        let uploads: Vec<_> = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/logs3")
            .collect();
        assert_eq!(state.calls.load(Ordering::SeqCst), uploads.len());
        let mut ids: Vec<String> = uploads
            .iter()
            .flat_map(|request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                body["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        ids.sort();
        assert_eq!(ids, ["first", "second"]);
    }

    #[tokio::test]
    async fn token_provider_errors_drop_the_rows_waiting_on_them() {
        struct Unavailable;
        #[async_trait::async_trait]
        impl TokenProvider for Unavailable {
            async fn token(
                &self,
            ) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
                Err("credential unavailable".into())
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        let rows = (0..3).map(|i| {
            serde_json::json!({"id": format!("row-{i}")})
                .as_object()
                .unwrap()
                .clone()
        });
        client.log_rows_with_token_provider(Arc::new(Unavailable), "org", None, rows);
        let _ = client.flush().await;

        assert_eq!(client.dropped_rows().upload_failed, 3);
        assert!(server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path() != "/logs3"));
    }

    #[tokio::test]
    async fn dropped_rows_counts_rejected_uploads() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(400).set_body_string("rejected"))
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        let rows = (0..3).map(|i| {
            serde_json::json!({"id": format!("row-{i}")})
                .as_object()
                .unwrap()
                .clone()
        });
        client.log_rows_with_credentials("token", "org", None, rows);
        let _ = client.flush().await;

        assert_eq!(client.dropped_rows().upload_failed, 3);
        assert_eq!(client.dropped_rows().queue_full, 0);
    }

    #[tokio::test]
    async fn dropped_rows_excludes_rows_delivered_before_a_split_fails() {
        struct SplitThenReject;
        impl wiremock::Respond for SplitThenReject {
            fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let rows = body["rows"].as_array().unwrap();
                let rejected = rows
                    .iter()
                    .filter(|row| matches!(row["id"].as_str(), Some("row-2" | "row-3")))
                    .count();
                if rejected == 0 {
                    ResponseTemplate::new(200).set_body_string("{}")
                } else if rejected == rows.len() {
                    ResponseTemplate::new(400).set_body_string("rejected")
                } else {
                    ResponseTemplate::new(413).set_body_string("too large")
                }
            }
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(SplitThenReject)
            .mount(&server)
            .await;
        let client = BraintrustClient::builder()
            .app_url(server.uri())
            .api_url(server.uri())
            .skip_login(true)
            .build()
            .await
            .expect("client");

        let rows = (0..4).map(|i| {
            serde_json::json!({"id": format!("row-{i}")})
                .as_object()
                .unwrap()
                .clone()
        });
        client.log_rows_with_credentials("token", "org", None, rows);
        let _ = client.flush().await;

        // The last two rows are dropped regardless of how the worker batches them.
        assert_eq!(client.dropped_rows().upload_failed, 2);
    }

    #[tokio::test]
    async fn log_rows_requires_login() {
        let client = BraintrustClient::builder()
            .app_url("https://example.com")
            .api_url("https://example.com")
            .skip_login(true)
            .build()
            .await
            .expect("client");
        let error = client
            .log_rows([Map::new()])
            .await
            .expect_err("skip_login clients must pass credentials explicitly");
        assert!(matches!(error, BraintrustError::InvalidConfig(_)));
    }

    #[tokio::test]
    async fn separate_urls_for_data_and_control_plane() {
        // Use two separate mock servers to verify requests go to the correct URLs
        let api_server = MockServer::start().await; // Data plane: /logs3
        let app_server = MockServer::start().await; // Control plane: /api/project/register, /api/apikey/login

        // Mount login mock on app_server
        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(mock_login_response(&[("org-id", "Test Org")]))
            .mount(&app_server)
            .await;

        // Mount project registration on app_server (control plane)
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "project": { "id": "proj-id" }
            })))
            .expect(1)
            .mount(&app_server)
            .await;

        // Mount version endpoint on api_server (data plane) - for lazy version fetch
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
            .await;

        // Mount logs endpoint on api_server (data plane)
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&api_server)
            .await;

        let client = BraintrustClient::builder()
            .api_key("test-api-key")
            .app_url(app_server.uri()) // Control plane
            .api_url(api_server.uri()) // Data plane
            .blocking_login(true)
            .build()
            .await
            .expect("client");

        let span = client
            .span_builder()
            .await
            .expect("span_builder")
            .project_name("test-project")
            .build();

        span.log(SpanLog {
            input: Some(Value::String("test".into())),
            ..Default::default()
        });
        span.flush().await.expect("flush");
        client.flush().await.expect("client flush");

        // Verify api_server received the /logs3 request (and optionally /version for lazy fetch)
        let api_requests = api_server.received_requests().await.unwrap();
        let logs3_requests: Vec<_> = api_requests
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .collect();
        assert_eq!(logs3_requests.len(), 1);

        // Verify app_server received the /api/project/register request (and login)
        let app_requests = app_server.received_requests().await.unwrap();
        let register_requests: Vec<_> = app_requests
            .iter()
            .filter(|r| r.url.path() == "/api/project/register")
            .collect();
        assert_eq!(register_requests.len(), 1);

        // Verify app_server did NOT receive /logs3
        let logs_on_app: Vec<_> = app_requests
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .collect();
        assert!(logs_on_app.is_empty(), "/logs3 should not go to app_server");

        // Verify api_server did NOT receive /api/project/register
        let register_on_api: Vec<_> = api_requests
            .iter()
            .filter(|r| r.url.path() == "/api/project/register")
            .collect();
        assert!(
            register_on_api.is_empty(),
            "/api/project/register should not go to api_server"
        );
    }
}
