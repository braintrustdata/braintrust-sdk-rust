//! Main Experiment type and implementation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::OnceCell;

use crate::api::experiments::ExperimentRegisterRequest;
use crate::error::Result;
use crate::experiments::event_log::ExperimentLog;
use crate::experiments::feedback::Feedback;
use crate::experiments::metadata::{
    BaseExperimentInfo, ExportedExperiment, ProjectMetadata, RepoInfo,
};
use crate::experiments::span_builder::ExperimentSpanBuilder;
use crate::experiments::summary::{ExperimentSummary, MetricSummary, ScoreSummary};
use crate::internal::execution::ExecutionBinding;
use crate::internal::ports::{ExperimentApi, PreparedSpan, SpanSubmitter};
use crate::internal::session::CredentialScope;
use crate::span::{SpanBuilder, SpanHandle, SpanLog};
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
/// Scheduled methods submit immediately on the selected runtime. Awaiting only
/// observes their completion; dropping a token does not cancel accepted work.
pub struct Experiment {
    pub(crate) api: Arc<dyn ExperimentApi>,
    pub(crate) execution: ExecutionBinding,
    pub(crate) submitter: Arc<dyn SpanSubmitter>,
    pub(crate) scope: CredentialScope,
    pub(crate) project_name: String,
    pub(crate) experiment_name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) base_experiment: Option<String>,
    pub(crate) metadata: Option<Map<String, Value>>,
    pub(crate) public: Option<bool>,
    pub(crate) update: Option<bool>,
    pub(crate) repo_info: Option<RepoInfo>,
    pub(crate) state: Arc<ExperimentState>,
}

/// Registration and live experiment state have the same lifetime as the handle
/// identity. Cloning a handle never resets initialization or recording state.
#[derive(Default)]
pub(crate) struct ExperimentState {
    metadata: OnceCell<ExperimentMetadata>,
    last_start_time: AtomicU64,
    recording: std::sync::Mutex<RecordingMode>,
}

#[derive(Default)]
struct RecordingMode {
    pending_starts: usize,
    selected: bool,
}
impl ExperimentState {
    fn recording(&self) -> std::sync::MutexGuard<'_, RecordingMode> {
        self.recording
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
/// A rejected/failed/lost start must not permanently select span recording.
struct PendingSpanStart(Arc<ExperimentState>);
impl Drop for PendingSpanStart {
    fn drop(&mut self) {
        self.0.recording().pending_starts -= 1;
    }
}

impl Clone for Experiment {
    fn clone(&self) -> Self {
        Self {
            api: self.api.clone(),
            execution: self.execution.clone(),
            submitter: self.submitter.clone(),
            scope: self.scope.clone(),
            project_name: self.project_name.clone(),
            experiment_name: self.experiment_name.clone(),
            description: self.description.clone(),
            base_experiment: self.base_experiment.clone(),
            update: self.update,
            repo_info: self.repo_info.clone(),
            metadata: self.metadata.clone(),
            public: self.public,
            state: self.state.clone(),
        }
    }
}

impl Experiment {
    /// Log an event to the experiment, creating and ending a span internally.
    ///
    /// Returns the span ID which can be used with `log_feedback()`.
    ///
    /// Returns an error if registration/delivery fails, or if start_span() has
    /// already selected the span-recording interface for this experiment.
    /// Admission happens immediately, before registration. Dropping the token
    /// does not cancel the write; awaiting includes full batch delivery.
    pub fn log(&self, event: ExperimentLog) -> crate::Completion<String, crate::BraintrustError> {
        self.execution.response_try(|| {
            let span_recording = {
                let mode = self.state.recording();
                mode.selected || mode.pending_starts > 0
            };
            if span_recording {
                return Err(crate::BraintrustError::InvalidConfig(
                    "cannot use log() after start_span(); use the span to log events".into(),
                ));
            }
            let start_time = f64::from_bits(self.state.last_start_time.load(Ordering::Acquire));
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs_f64())
                .unwrap_or(0.0);
            let start_time = if start_time == 0.0 { now } else { start_time };
            let mut event = event.into_span_log();
            event
                .metrics
                .get_or_insert_with(HashMap::new)
                .entry("end".into())
                .or_insert(now);
            let payload = SpanBuilder::new(self.submitter.clone(), self.scope.clone())
                .span_type(SpanType::Eval)
                .start_time(start_time)
                .into_payload(event);
            let row_id = payload.row_id.clone();
            let delivery = self.admit_payload(payload)?;
            let state = self.state.clone();
            Ok(async move {
                delivery.await?;
                state
                    .last_start_time
                    .store(now.to_bits(), Ordering::Release);
                Ok(row_id)
            })
        })
    }

    /// Start a new root span for this experiment.
    ///
    /// Use this for more control over span lifecycle. Pending and successful
    /// starts prevent `log()` on this experiment.
    /// Rejected or failed starts release their pending mode reservation.
    pub fn start_span(&self) -> crate::Completion<ExperimentSpanBuilder, crate::BraintrustError> {
        self.execution.response_try(|| {
            self.state.recording().pending_starts += 1;
            let pending = PendingSpanStart(self.state.clone());
            let experiment = self.clone();
            Ok(async move {
                // Capture the guard before spawning, including runtime loss
                // before first poll. Only successful registration commits mode.
                let _pending = pending;
                let metadata = experiment.register_metadata().await?;
                experiment.state.recording().selected = true;
                Ok(ExperimentSpanBuilder::new(&experiment, metadata.clone()))
            })
        })
    }

    /// Log feedback on an existing span/record.
    ///
    /// Use this to add scores, comments, or other feedback after the initial log.
    /// Admission happens before registration. Dropping the token does not cancel
    /// the patch; awaiting reports full batch delivery.
    pub fn log_feedback(
        &self,
        id: &str,
        feedback: Feedback,
    ) -> crate::Completion<(), crate::BraintrustError> {
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
            org_id: self.scope.org_id().to_owned(),
            org_name: self.scope.org_name().map(str::to_owned),
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

        self.submit_patch(payload)
    }

    /// Get a summary of the experiment's performance.
    ///
    /// This flushes pending data and fetches aggregated scores from the API.
    /// If a base experiment is configured, comparison data will be included.
    /// The flush cutoff is captured at issuance, before the returned token is polled.
    pub fn summarize(&self) -> crate::Completion<ExperimentSummary, crate::BraintrustError> {
        self.execution.response_try(|| {
            let flush = self.flush();
            if let Err(error) = &*flush {
                return Err(error.clone().into());
            }
            let experiment = self.clone();
            Ok(async move {
                flush.await?;
                experiment.summarize_inner().await
            })
        })
    }

    async fn summarize_inner(&self) -> Result<ExperimentSummary> {
        let metadata = self.register_metadata().await?;

        // Try to get base experiment for comparison
        let base_exp_id = self
            .fetch_base_experiment_inner()
            .await?
            .map(|base| base.id);

        // Fetch comparison data from API
        let comparison = self
            .api
            .fetch_experiment_comparison(
                &self.scope,
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

    /// Capture pending writes through this call, including unresolved registration.
    pub fn flush(&self) -> crate::Completion<crate::FlushReport, crate::FlushError> {
        self.submitter.flush()
    }

    /// Get the experiment ID (after registration).
    pub fn experiment_id(&self) -> Option<String> {
        self.state.metadata.get().map(|m| m.experiment_id.clone())
    }

    /// Get the experiment name (after registration).
    pub fn experiment_name(&self) -> String {
        self.state
            .metadata
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
        self.execution
            .response(async move { experiment.project_inner().await })
    }

    async fn project_inner(&self) -> Result<ProjectMetadata> {
        let metadata = self.register_metadata().await?;
        Ok(ProjectMetadata {
            id: metadata.project_id.clone(),
            name: self.project_name.clone(),
        })
    }

    /// Execute a callback within a traced span.
    ///
    /// Finalizes the span and flushes active work when the callback completes.
    /// Setup, final-write, and final-flush errors are returned. Individual writes expose their
    /// delivery results through the completion tokens returned inside the callback.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result = experiment.traced(|span| async move {
    ///     span.log(SpanLog::builder().input(json!({"x": 1})).build().unwrap());
    ///     compute_result()
    /// }).await;
    /// ```
    pub async fn traced<F, Fut, R>(&self, callback: F) -> Result<R>
    where
        F: FnOnce(SpanHandle) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let span = self.start_span().await?.name("root").build();
        let result = callback(span.clone()).await;
        let end = span.end();
        let flush = span.flush();
        let ended = end.await;
        let flushed = flush.await;
        ended?;
        flushed?;
        Ok(result)
    }

    /// Update an existing span by ID, merging new data with existing data.
    ///
    /// This is useful for adding information to a span after it has been created,
    /// such as adding scores or metadata computed later.
    /// Admission happens before registration. Dropping the token does not cancel
    /// the patch; awaiting reports full batch delivery.
    pub fn update_span(
        &self,
        id: &str,
        event: SpanLog,
    ) -> crate::Completion<(), crate::BraintrustError> {
        let payload = SpanPayload {
            row_id: id.to_string(),
            span_id: id.to_string(),
            is_merge: true,
            span_components: None,
            org_id: self.scope.org_id().to_owned(),
            org_name: self.scope.org_name().map(str::to_owned),
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

        self.submit_patch(payload)
    }

    fn submit_patch(&self, payload: SpanPayload) -> crate::Completion<(), crate::BraintrustError> {
        self.execution.response_try(|| {
            let delivery = self.admit_payload(payload)?;
            Ok(async move {
                delivery.await?;
                Ok(())
            })
        })
    }

    /// Called only after reserving response capacity, so a rejected response
    /// never leaves a write behind. The write owns all remaining preparation.
    fn admit_payload(
        &self,
        payload: SpanPayload,
    ) -> Result<crate::Completion<crate::DeliveryReceipt, crate::WriteError>> {
        let delivery = if let Some(metadata) = self.state.metadata.get() {
            self.submitter.submit(
                self.scope.clone(),
                payload,
                Some(ParentSpanInfo::Experiment {
                    object_id: metadata.experiment_id.clone(),
                }),
                None,
            )
        } else {
            let experiment = self.clone();
            let scope = self.scope.clone();
            self.submitter.submit_deferred(
                payload.row_id.clone(),
                Box::pin(async move {
                    let metadata = experiment.register_metadata().await?;
                    let parent_info = Some(ParentSpanInfo::Experiment {
                        object_id: metadata.experiment_id.clone(),
                    });
                    Ok(PreparedSpan {
                        scope,
                        payload,
                        parent_info,
                    })
                }),
            )
        };
        if let Err(error) = &*delivery {
            return Err(error.clone().into());
        }
        Ok(delivery)
    }

    /// Fetch the base experiment used for comparison.
    ///
    /// Returns `None` if no base experiment has been configured or detected.
    pub fn fetch_base_experiment(
        &self,
    ) -> crate::Completion<Option<BaseExperimentInfo>, crate::BraintrustError> {
        let experiment = self.clone();
        self.execution
            .response(async move { experiment.fetch_base_experiment_inner().await })
    }

    async fn fetch_base_experiment_inner(&self) -> Result<Option<BaseExperimentInfo>> {
        let metadata = self.register_metadata().await?;
        self.api
            .fetch_base_experiment(&self.scope, &metadata.experiment_id)
            .await
    }

    /// Export experiment context for distributed tracing.
    ///
    /// Use this to pass experiment context to another process or service,
    /// allowing spans created there to be linked to this experiment.
    pub fn export(&self) -> crate::Completion<ExportedExperiment, crate::BraintrustError> {
        let experiment = self.clone();
        self.execution
            .response(async move { experiment.export_inner().await })
    }

    async fn export_inner(&self) -> Result<ExportedExperiment> {
        let metadata = self.register_metadata().await?;
        Ok(ExportedExperiment {
            object_type: "experiment".to_string(),
            object_id: metadata.experiment_id.clone(),
        })
    }

    /// Polled only on the selected context, by preparation or a bound operation.
    async fn register_metadata(&self) -> Result<&ExperimentMetadata> {
        self.state
            .metadata
            .get_or_try_init(|| async {
                let request = ExperimentRegisterRequest {
                    project_name: self.project_name.clone(),
                    org_id: self.scope.org_id().to_owned(),
                    experiment_name: self.experiment_name.clone(),
                    description: self.description.clone(),
                    base_experiment: self.base_experiment.clone(),
                    metadata: self.metadata.clone(),
                    public: self.public,
                    update: self.update,
                    repo_info: self.repo_info.clone(),
                };
                let response = self.api.register_experiment(&self.scope, request).await?;
                Ok(ExperimentMetadata {
                    project_id: response.project.id,
                    experiment_id: response.experiment.id,
                    experiment_name: response.experiment.name,
                })
            })
            .await
    }
}
