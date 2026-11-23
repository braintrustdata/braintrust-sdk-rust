//! Main Experiment type and implementation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

use crate::error::Result;
use crate::experiments::api::{
    BaseExperimentFetcher, ExperimentComparisonFetcher, ExperimentRegisterRequest,
    ExperimentRegistrar,
};
use crate::experiments::event_log::ExperimentLog;
use crate::experiments::feedback::Feedback;
use crate::experiments::metadata::{
    BaseExperimentInfo, ExportedExperiment, ProjectMetadata, RepoInfo,
};
use crate::experiments::span_builder::ExperimentSpanBuilder;
use crate::experiments::summary::{ExperimentSummary, MetricSummary, ScoreSummary};
use crate::log_queue::Credential;
use crate::span::{SpanBuilder, SpanHandle, SpanLog, SpanSubmitter};
use crate::types::{ParentSpanInfo, SpanAttributes, SpanPayload, SpanType};

/// Metadata from experiment registration, cached after lazy initialization.
#[derive(Debug, Clone)]
pub(crate) struct ExperimentMetadata {
    pub(crate) project_id: String,
    pub(crate) experiment_id: String,
    pub(crate) experiment_name: String,
}

/// Handle to a Braintrust experiment.
///
/// Use this to log evaluation events, create spans, log feedback, and get summaries.
#[allow(private_bounds)]
pub struct Experiment<S: SpanSubmitter> {
    pub(crate) submitter: Arc<S>,
    pub(crate) token: String,
    pub(crate) org_id: String,
    pub(crate) org_name: Option<String>,
    pub(crate) project_name: String,
    pub(crate) experiment_name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) base_experiment: Option<String>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) public: Option<bool>,
    pub(crate) update: Option<bool>,
    pub(crate) repo_info: Option<RepoInfo>,
    pub(crate) lazy_metadata: Arc<OnceCell<ExperimentMetadata>>,
    /// Last start time for sequential log() calls (stored as bits).
    pub(crate) last_start_time: Arc<AtomicU64>,
    /// Whether start_span() has been called (prevents mixing with log()).
    pub(crate) called_start_span: Arc<AtomicBool>,
    /// Pending span handles that need to be flushed.
    pub(crate) pending_flushes: Arc<Mutex<Vec<SpanHandle<S>>>>,
}

impl<S: SpanSubmitter> Clone for Experiment<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
            token: self.token.clone(),
            org_id: self.org_id.clone(),
            org_name: self.org_name.clone(),
            project_name: self.project_name.clone(),
            experiment_name: self.experiment_name.clone(),
            description: self.description.clone(),
            base_experiment: self.base_experiment.clone(),
            update: self.update,
            repo_info: self.repo_info.clone(),
            metadata: self.metadata.clone(),
            public: self.public,
            lazy_metadata: self.lazy_metadata.clone(),
            last_start_time: self.last_start_time.clone(),
            called_start_span: self.called_start_span.clone(),
            pending_flushes: Arc::clone(&self.pending_flushes),
        }
    }
}

#[allow(private_bounds)]
impl<
        S: SpanSubmitter
            + ExperimentRegistrar
            + BaseExperimentFetcher
            + ExperimentComparisonFetcher
            + 'static,
    > Experiment<S>
{
    /// Log an event to the experiment, creating and ending a span internally.
    ///
    /// Returns the span ID which can be used with `log_feedback()`.
    ///
    /// # Panics
    ///
    /// Panics if `start_span()` has already been called on this experiment,
    /// as mixing `log()` and `start_span()` is not supported.
    pub fn log(&self, event: ExperimentLog) -> crate::Completion<String, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { Ok(experiment.log_inner(event).await) })
    }

    async fn log_inner(&self, event: ExperimentLog) -> String {
        if self.called_start_span.load(Ordering::Acquire) {
            panic!(
                "Cannot use log() after start_span() has been called. \
                 Use start_span() consistently or log() consistently, but not both."
            );
        }

        // Get or create experiment registration
        let metadata = self.ensure_registered().await;

        // Create a span for this log event
        let start_time = f64::from_bits(self.last_start_time.load(Ordering::Acquire));
        let start_time = if start_time == 0.0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        } else {
            start_time
        };

        let span = self.create_internal_span(&metadata, start_time).await;

        // Get the row_id before logging - it's stable from span creation
        let row_id = span.row_id().to_string();

        // Convert ExperimentLog to SpanLog and log
        let span_log = event.into_span_log();
        span.log(span_log);

        // Record end time for next log() call
        let end_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        self.last_start_time
            .store(end_time.to_bits(), Ordering::Release);

        // Track for flush
        self.pending_flushes.lock().await.push(span);

        row_id
    }

    /// Start a new root span for this experiment.
    ///
    /// Use this for more control over span lifecycle. Once called,
    /// you cannot use `log()` on this experiment.
    pub fn start_span(
        &self,
    ) -> crate::Completion<ExperimentSpanBuilder<'static, S>, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { Ok(experiment.start_span_inner().await) })
    }

    async fn start_span_inner(&self) -> ExperimentSpanBuilder<'static, S> {
        self.called_start_span.store(true, Ordering::Release);
        let metadata = self.ensure_registered().await;
        ExperimentSpanBuilder::new(self.clone(), metadata)
    }

    /// Log feedback on an existing span/record.
    ///
    /// Use this to add scores, comments, or other feedback after the initial log.
    pub fn log_feedback(
        &self,
        id: &str,
        feedback: Feedback,
    ) -> crate::Completion<(), crate::BraintrustError> {
        let experiment = self.clone();
        let id = id.to_owned();
        crate::service::submit(async move {
            experiment.log_feedback_inner(&id, feedback).await;
            Ok(())
        })
    }

    async fn log_feedback_inner(&self, id: &str, feedback: Feedback) {
        let metadata = self.ensure_registered().await;

        // Create a merge payload for the existing span
        let mut scores = HashMap::new();
        if let Some(fb_scores) = feedback.scores {
            scores = fb_scores;
        }

        let mut metadata_map = Map::new();
        if let Some(fb_metadata) = feedback.metadata {
            metadata_map = fb_metadata;
        }

        // Add feedback-specific fields to metadata
        if let Some(comment) = feedback.comment {
            metadata_map.insert("comment".to_string(), Value::String(comment));
        }
        if let Some(source) = feedback.source {
            metadata_map.insert("source".to_string(), Value::String(source));
        }

        // Note: We don't set project_name here because the destination is
        // determined by the experiment_id via parent_info.
        let payload = SpanPayload {
            row_id: id.to_string(),
            span_id: id.to_string(), // Use same ID for merge
            is_merge: true,
            span_components: None,
            org_id: self.org_id.clone(),
            org_name: self.org_name.clone(),
            project_name: None,
            input: None,
            output: None,
            expected: feedback.expected,
            error: None,
            scores: (!scores.is_empty()).then_some(scores),
            metadata: (!metadata_map.is_empty()).then_some(metadata_map),
            metrics: None,
            tags: feedback.tags,
            context: None,
            span_attributes: None,
            extra: HashMap::new(),
        };

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        self.submitter.submit(
            Credential::token_value(self.token.clone()),
            payload,
            Some(parent_info),
        );
    }

    /// Get a summary of the experiment's performance.
    ///
    /// This flushes pending data and fetches aggregated scores from the API.
    /// If a base experiment is configured, comparison data will be included.
    pub fn summarize(&self) -> crate::Completion<ExperimentSummary, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { experiment.summarize_inner().await })
    }

    async fn summarize_inner(&self) -> Result<ExperimentSummary> {
        // Flush all pending data first
        self.flush().await?;

        let metadata = self.ensure_registered().await;

        // Try to get base experiment for comparison
        let base_exp_id = self
            .fetch_base_experiment()
            .await
            .ok()
            .flatten()
            .map(|b| b.id);

        // Fetch comparison data from API
        let comparison = self
            .submitter
            .fetch_experiment_comparison(
                &self.token,
                &metadata.experiment_id,
                base_exp_id.as_deref(),
            )
            .await?;

        // Build summary from comparison response
        let mut summary = ExperimentSummary::new(
            self.project_name.clone(),
            metadata.experiment_name.clone(),
            Some(metadata.project_id.clone()),
            Some(metadata.experiment_id.clone()),
        );

        // Set comparison experiment name if present
        if let Some(base_name) = comparison.base_experiment_name {
            summary.set_comparison_experiment_name(base_name);
        }

        // Convert scores from comparison response
        let scores: HashMap<String, ScoreSummary> = comparison
            .scores
            .into_iter()
            .map(|(name, data)| {
                let mut score_summary = ScoreSummary::new(name.clone(), data.score);
                if let Some(diff) = data.diff {
                    score_summary.set_diff(diff);
                }
                if let Some(improvements) = data.improvements {
                    score_summary.set_improvements(improvements);
                }
                if let Some(regressions) = data.regressions {
                    score_summary.set_regressions(regressions);
                }
                (name, score_summary)
            })
            .collect();
        summary.set_scores(scores);

        // Convert metrics from comparison response
        if !comparison.metrics.is_empty() {
            let metrics: HashMap<String, MetricSummary> = comparison
                .metrics
                .into_iter()
                .map(|(name, data)| {
                    let mut metric_summary = MetricSummary::new(name.clone(), data.metric);
                    if let Some(diff) = data.diff {
                        metric_summary.set_diff(diff);
                    }
                    if let Some(improvements) = data.improvements {
                        metric_summary.set_improvements(improvements);
                    }
                    if let Some(regressions) = data.regressions {
                        metric_summary.set_regressions(regressions);
                    }
                    (name, metric_summary)
                })
                .collect();
            summary.set_metrics(metrics);
        }

        Ok(summary)
    }

    /// Flush all pending experiment data to Braintrust.
    pub fn flush(&self) -> crate::Completion<(), crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { experiment.flush_inner().await })
    }

    async fn flush_inner(&self) -> Result<()> {
        let mut pending = self.pending_flushes.lock().await;
        for span in pending.drain(..) {
            span.end();
        }
        drop(pending);
        self.submitter.flush().await.map(|_| ()).map_err(Into::into)
    }

    /// Get the experiment ID (after registration).
    pub fn experiment_id(&self) -> Option<String> {
        self.lazy_metadata.get().map(|m| m.experiment_id.clone())
    }

    /// Get the experiment name (after registration).
    pub fn experiment_name(&self) -> String {
        self.lazy_metadata
            .get()
            .map(|m| m.experiment_name.clone())
            .unwrap_or_else(|| {
                self.experiment_name
                    .clone()
                    .unwrap_or_else(|| "unnamed".to_string())
            })
    }

    /// Get the project containing this experiment.
    pub fn project(&self) -> crate::Completion<ProjectMetadata, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { Ok(experiment.project_inner().await) })
    }

    async fn project_inner(&self) -> ProjectMetadata {
        let metadata = self.ensure_registered().await;
        ProjectMetadata {
            id: metadata.project_id.clone(),
            name: self.project_name.clone(),
        }
    }

    /// Execute a callback within a traced span.
    ///
    /// The span is automatically flushed when the callback completes (success or error).
    /// This ensures all span data is sent to Braintrust before the method returns.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result = experiment.traced(|span| async move {
    ///     span.log(SpanLog::builder().input(json!({"x": 1})).build().unwrap());
    ///     compute_result()
    /// }).await;
    /// ```
    pub fn traced<F, Fut, R>(&self, callback: F) -> crate::Completion<R, crate::BraintrustError>
    where
        F: FnOnce(SpanHandle<S>) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        let experiment = self.clone();
        crate::service::submit(async move { Ok(experiment.traced_inner(callback).await) })
    }

    async fn traced_inner<F, Fut, R>(&self, callback: F) -> R
    where
        F: FnOnce(SpanHandle<S>) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let span = self.start_span_inner().await.name("root").build();
        let result = callback(span.clone()).await;
        // Flush span data - ignore errors since this is best-effort
        let _ = span.flush().await;
        result
    }

    /// Update an existing span by ID, merging new data with existing data.
    ///
    /// This is useful for adding information to a span after it has been created,
    /// such as adding scores or metadata computed later.
    pub fn update_span(
        &self,
        id: &str,
        event: SpanLog,
    ) -> crate::Completion<(), crate::BraintrustError> {
        let experiment = self.clone();
        let id = id.to_owned();
        crate::service::submit(async move {
            experiment.update_span_inner(&id, event).await;
            Ok(())
        })
    }

    async fn update_span_inner(&self, id: &str, event: SpanLog) {
        let metadata = self.ensure_registered().await;

        let payload = SpanPayload {
            row_id: id.to_string(),
            span_id: id.to_string(),
            is_merge: true,
            span_components: None,
            org_id: self.org_id.clone(),
            org_name: self.org_name.clone(),
            project_name: None,
            input: event.input,
            output: event.output,
            expected: event.expected,
            error: event.error,
            scores: event.scores,
            metadata: event.metadata,
            metrics: event.metrics,
            tags: event.tags,
            context: event.context,
            span_attributes: event.name.map(|name| SpanAttributes {
                name: Some(name),
                span_type: None,
                purpose: None,
                extra: HashMap::new(),
            }),
            extra: HashMap::new(),
        };

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        self.submitter.submit(
            Credential::token_value(self.token.clone()),
            payload,
            Some(parent_info),
        );
    }

    /// Fetch the base experiment used for comparison.
    ///
    /// Returns `None` if no base experiment has been configured or detected.
    pub fn fetch_base_experiment(
        &self,
    ) -> crate::Completion<Option<BaseExperimentInfo>, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { experiment.fetch_base_experiment_inner().await })
    }

    async fn fetch_base_experiment_inner(&self) -> Result<Option<BaseExperimentInfo>> {
        let metadata = self.ensure_registered().await;
        self.submitter
            .fetch_base_experiment(&self.token, &metadata.experiment_id)
            .await
    }

    /// Export experiment context for distributed tracing.
    ///
    /// Use this to pass experiment context to another process or service,
    /// allowing spans created there to be linked to this experiment.
    pub fn export(&self) -> crate::Completion<ExportedExperiment, crate::BraintrustError> {
        let experiment = self.clone();
        crate::service::submit(async move { Ok(experiment.export_inner().await) })
    }

    async fn export_inner(&self) -> ExportedExperiment {
        let metadata = self.ensure_registered().await;
        ExportedExperiment {
            object_type: "experiment".to_string(),
            object_id: metadata.experiment_id.clone(),
        }
    }

    /// Ensure the experiment is registered, returning cached metadata.
    pub(crate) async fn ensure_registered(&self) -> ExperimentMetadata {
        self.lazy_metadata
            .get_or_try_init(|| async {
                let mut request =
                    ExperimentRegisterRequest::new(self.project_name.clone(), self.org_id.clone());
                request.experiment_name = self.experiment_name.clone();
                request.description = self.description.clone();
                request.base_experiment = self.base_experiment.clone();
                request.metadata = self.metadata.clone();
                request.public = self.public;
                request.update = self.update;
                request.repo_info = self.repo_info.clone().map(|repo| {
                    let mut value = crate::api::ExperimentRepoInfo::default();
                    value.commit = repo.commit;
                    value.branch = repo.branch;
                    value.tag = repo.tag;
                    value.dirty = repo.dirty;
                    value.author_name = repo.author_name;
                    value.author_email = repo.author_email;
                    value.commit_message = repo.commit_message;
                    value.commit_time = repo.commit_time;
                    value
                });

                let response = self
                    .submitter
                    .register_experiment(&self.token, request)
                    .await?;
                Ok::<_, crate::BraintrustError>(ExperimentMetadata {
                    project_id: response.project.id,
                    experiment_id: response.experiment.id,
                    experiment_name: response.experiment.name,
                })
            })
            .await
            .cloned()
            .unwrap_or_else(|error| {
                // Preserve the existing best-effort result without caching a failed registration.
                tracing::warn!(%error, "experiment registration failed");
                ExperimentMetadata {
                    project_id: String::new(),
                    experiment_id: Uuid::new_v4().to_string(),
                    experiment_name: self
                        .experiment_name
                        .clone()
                        .unwrap_or_else(|| "unnamed".into()),
                }
            })
    }

    /// Create an internal span for log() calls.
    async fn create_internal_span(
        &self,
        metadata: &ExperimentMetadata,
        start_time: f64,
    ) -> SpanHandle<S> {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        let mut builder = SpanBuilder::new(
            Arc::clone(&self.submitter),
            self.token.clone(),
            self.org_id.clone(),
        )
        .parent_info(parent_info)
        .span_type(SpanType::Eval)
        .start_time(start_time);

        if let Some(org_name) = &self.org_name {
            builder = builder.org_name(org_name.clone());
        }

        builder.build()
    }
}
