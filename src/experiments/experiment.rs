//! Main Experiment type and implementation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::sync::OnceCell;

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
use crate::internal::session::CredentialScope;
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
    called_start_span: AtomicBool,
}

impl<S: SpanSubmitter> Clone for Experiment<S> {
    fn clone(&self) -> Self {
        Self {
            submitter: Arc::clone(&self.submitter),
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
    /// Returns an error if registration/delivery fails, or if start_span() has
    /// already selected the span-recording interface for this experiment.
    pub async fn log(&self, event: ExperimentLog) -> Result<String> {
        if self.state.called_start_span.load(Ordering::Acquire) {
            return Err(crate::BraintrustError::InvalidConfig(
                "cannot use log() after start_span(); use the span to log events".into(),
            ));
        }

        // Get or create experiment registration
        let metadata = self.ensure_registered().await?;

        // Create a span for this log event
        let start_time = f64::from_bits(self.state.last_start_time.load(Ordering::Acquire));
        let start_time = if start_time == 0.0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
        } else {
            start_time
        };

        let span = self.create_internal_span(metadata, start_time);

        // Get the row_id before logging - it's stable from span creation
        let row_id = span.row_id().to_string();

        let end_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0);
        let mut span_log = event.into_span_log();
        span_log
            .metrics
            .get_or_insert_with(HashMap::new)
            .entry("end".into())
            .or_insert(end_time);
        // A single complete event snapshot owns the delivery obligation.
        span.log(span_log).await?;

        self.state
            .last_start_time
            .store(end_time.to_bits(), Ordering::Release);

        Ok(row_id)
    }

    /// Start a new root span for this experiment.
    ///
    /// Use this for more control over span lifecycle. Once called,
    /// you cannot use `log()` on this experiment.
    pub async fn start_span(&self) -> Result<ExperimentSpanBuilder<'_, S>> {
        let metadata = self.ensure_registered().await?;
        self.state.called_start_span.store(true, Ordering::Release);
        Ok(ExperimentSpanBuilder::new(self, metadata.clone()))
    }

    /// Log feedback on an existing span/record.
    ///
    /// Use this to add scores, comments, or other feedback after the initial log.
    pub async fn log_feedback(&self, id: &str, feedback: Feedback) -> Result<()> {
        let metadata = self.ensure_registered().await?;

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

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        self.submitter
            .submit(self.scope.clone(), payload, Some(parent_info))
            .await?;
        Ok(())
    }

    /// Get a summary of the experiment's performance.
    ///
    /// This flushes pending data and fetches aggregated scores from the API.
    /// If a base experiment is configured, comparison data will be included.
    pub async fn summarize(&self) -> Result<ExperimentSummary> {
        // Flush all pending data first
        self.flush().await?;

        let metadata = self.ensure_registered().await?;

        // Try to get base experiment for comparison
        let base_exp_id = self.fetch_base_experiment().await?.map(|base| base.id);

        // Fetch comparison data from API
        let comparison = self
            .submitter
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

    /// Flush all pending experiment data to Braintrust.
    pub async fn flush(&self) -> Result<()> {
        self.submitter.flush().await?;
        Ok(())
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
    pub async fn project(&self) -> Result<ProjectMetadata> {
        let metadata = self.ensure_registered().await?;
        Ok(ProjectMetadata {
            id: metadata.project_id.clone(),
            name: self.project_name.clone(),
        })
    }

    /// Execute a callback within a traced span.
    ///
    /// Finalizes the span and flushes active work when the callback completes.
    /// Setup and final-flush errors are returned. Individual writes expose their
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
        F: FnOnce(SpanHandle<S>) -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let span = self.start_span().await?.name("root").build();
        let result = callback(span.clone()).await;
        span.end();
        span.flush().await?;
        Ok(result)
    }

    /// Update an existing span by ID, merging new data with existing data.
    ///
    /// This is useful for adding information to a span after it has been created,
    /// such as adding scores or metadata computed later.
    pub async fn update_span(&self, id: &str, event: SpanLog) -> Result<()> {
        let metadata = self.ensure_registered().await?;

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

        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        self.submitter
            .submit(self.scope.clone(), payload, Some(parent_info))
            .await?;
        Ok(())
    }

    /// Fetch the base experiment used for comparison.
    ///
    /// Returns `None` if no base experiment has been configured or detected.
    pub async fn fetch_base_experiment(&self) -> Result<Option<BaseExperimentInfo>> {
        let metadata = self.ensure_registered().await?;
        self.submitter
            .fetch_base_experiment(&self.scope, &metadata.experiment_id)
            .await
    }

    /// Export experiment context for distributed tracing.
    ///
    /// Use this to pass experiment context to another process or service,
    /// allowing spans created there to be linked to this experiment.
    pub async fn export(&self) -> Result<ExportedExperiment> {
        let metadata = self.ensure_registered().await?;
        Ok(ExportedExperiment {
            object_type: "experiment".to_string(),
            object_id: metadata.experiment_id.clone(),
        })
    }

    /// Ensure the experiment is registered, returning cached metadata.
    pub(crate) async fn ensure_registered(&self) -> Result<&ExperimentMetadata> {
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
                let response = self
                    .submitter
                    .register_experiment(&self.scope, request)
                    .await?;
                Ok(ExperimentMetadata {
                    project_id: response.project.id,
                    experiment_id: response.experiment.id,
                    experiment_name: response.experiment.name,
                })
            })
            .await
    }

    /// Create an internal span for log() calls.
    fn create_internal_span(
        &self,
        metadata: &ExperimentMetadata,
        start_time: f64,
    ) -> SpanHandle<S> {
        let parent_info = ParentSpanInfo::Experiment {
            object_id: metadata.experiment_id.clone(),
        };

        // Note: When using ParentSpanInfo::Experiment, we don't need project_name
        // because the destination is determined by the experiment_id.
        let builder = SpanBuilder::new(Arc::clone(&self.submitter), self.scope.clone())
            .parent_info(parent_info)
            .span_type(SpanType::Eval)
            .start_time(start_time);

        builder.build()
    }
}
