//! Span routing and construction of logs3 rows.
use super::core::QueueSettings;
use crate::span_components::{project_logs_identifier, ProjectLogsIdentifier};
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use anyhow::{anyhow, Result};
use tokio::sync::OnceCell;

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum Destination {
    Known(LogDestination),
    ProjectName(String),
}

impl Destination {
    fn from_span(
        kind: SpanObjectType,
        id: Option<&str>,
        metadata: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Result<Self> {
        let known = match kind {
            SpanObjectType::Experiment => LogDestination::experiment(
                id.ok_or_else(|| anyhow!("experiment span is missing object_id"))?,
            ),
            SpanObjectType::PlaygroundLogs => LogDestination::playground_logs(
                id.ok_or_else(|| anyhow!("playground span is missing object_id"))?,
            ),
            SpanObjectType::ProjectLogs => match project_logs_identifier(id, metadata) {
                Some(ProjectLogsIdentifier::ObjectId(id)) => LogDestination::project_logs(id),
                Some(ProjectLogsIdentifier::ProjectName(name)) => {
                    return Ok(Self::ProjectName(name))
                }
                None => anyhow::bail!(
                    "project-log span is missing object_id, project_id, or project_name"
                ),
            },
        };
        Ok(Self::Known(known))
    }
}

pub(super) struct SpanRoute {
    pub destination: Destination,
    root_span_id: String,
    parents: Option<Vec<String>>,
}

impl SpanRoute {
    pub fn new(payload: &SpanPayload, parent: Option<&ParentSpanInfo>) -> Result<Self> {
        if let Some(span) = &payload.span_components {
            return Ok(Self {
                destination: Destination::from_span(
                    span.object_type,
                    span.object_id.as_deref(),
                    span.compute_object_metadata_args.as_ref(),
                )?,
                root_span_id: span
                    .root_span_id
                    .clone()
                    .unwrap_or_else(|| payload.span_id.clone()),
                parents: span.span_parents.clone(),
            });
        }
        let destination = match parent {
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                compute_object_metadata_args,
                span_id,
                root_span_id,
                ..
            }) => {
                return Ok(Self {
                    destination: Destination::from_span(
                        *object_type,
                        object_id.as_deref(),
                        compute_object_metadata_args.as_ref(),
                    )?,
                    root_span_id: root_span_id.clone(),
                    parents: Some(vec![span_id.clone()]),
                })
            }
            Some(ParentSpanInfo::Experiment { object_id }) => {
                Destination::Known(LogDestination::experiment(object_id))
            }
            Some(ParentSpanInfo::Dataset { object_id }) => {
                Destination::Known(LogDestination::dataset(object_id))
            }
            Some(ParentSpanInfo::ProjectLogs { object_id }) => {
                Destination::Known(LogDestination::project_logs(object_id))
            }
            Some(ParentSpanInfo::PlaygroundLogs { object_id }) => {
                Destination::Known(LogDestination::playground_logs(object_id))
            }
            Some(ParentSpanInfo::ProjectName { project_name }) => {
                Destination::ProjectName(project_name.clone())
            }
            None => Destination::ProjectName(payload.project_name.clone().ok_or_else(|| {
                anyhow!("no destination: either parent_info or project_name required")
            })?),
        };
        Ok(Self {
            destination,
            root_span_id: payload.span_id.clone(),
            parents: None,
        })
    }

    pub async fn into_row(
        self,
        payload: SpanPayload,
        resolve: impl AsyncFnOnce(&str, Option<&str>, &str) -> Result<String>,
    ) -> Result<Logs3Row> {
        let destination = match self.destination {
            Destination::Known(destination) => destination,
            Destination::ProjectName(name) => LogDestination::project_logs(
                resolve(&payload.org_id, payload.org_name.as_deref(), &name).await?,
            ),
        };
        Ok(Self::build_row(
            payload,
            self.root_span_id,
            self.parents,
            destination,
        ))
    }

    /// A rejected row can be written for debugging without resolving a project name.
    pub fn into_known_row(self, payload: SpanPayload) -> Option<Logs3Row> {
        let Destination::Known(destination) = self.destination else {
            return None;
        };
        Some(Self::build_row(
            payload,
            self.root_span_id,
            self.parents,
            destination,
        ))
    }

    fn build_row(
        payload: SpanPayload,
        root_span_id: String,
        span_parents: Option<Vec<String>>,
        destination: LogDestination,
    ) -> Logs3Row {
        Logs3Row {
            id: payload.row_id,
            is_merge: payload.is_merge.then_some(true),
            merge_paths: None,
            span_id: payload.span_id,
            root_span_id,
            span_parents,
            destination,
            org_id: payload.org_id,
            org_name: payload.org_name,
            input: payload.input,
            output: payload.output,
            expected: payload.expected,
            error: payload.error,
            scores: payload.scores,
            metadata: payload.metadata,
            metrics: payload.metrics,
            tags: payload.tags,
            context: payload.context,
            span_attributes: payload.span_attributes,
            extra: payload.extra,
            created: chrono::Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: Some("api".into()),
        }
    }
}

pub(super) type ProjectResolution = OnceCell<Result<String>>;

impl QueueSettings {
    pub(super) async fn prepare_span(
        &self,
        token: &str,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        project: &ProjectResolution,
    ) -> Result<Logs3Row> {
        SpanRoute::new(&payload, parent_info.as_ref())?
            .into_row(payload, async |org_id, org_name, name| {
                // A job has at most one unresolved project name. Reuse its
                // result for each row, including a failed registration.
                project
                    .get_or_init(|| self.ensure_project_id(token, org_id, org_name, name))
                    .await
                    .as_ref()
                    .cloned()
                    .map_err(super::observation::duplicate_error)
            })
            .await
    }

    /// Ensure a project ID is available for the given project name, registering
    /// it via the API if it is not yet cached.
    ///
    async fn ensure_project_id(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
    ) -> Result<String> {
        let app_url_key = self.app_url.to_string();
        let org = crate::resolution::OrgIdentity::new(org_id, org_name, token);
        let scope = crate::scheduler::Scope::new(app_url_key.clone(), org.scope_key());
        let entry = crate::resolution::project(&app_url_key, org, project_name);
        // Bounded by the retry budget and request timeouts, not a deadline, so
        // registration can wait out a 429 cooldown.
        let resolve = entry.get_or_try_init(|| async {
            let mut request = crate::api::ProjectRegisterRequest::new(project_name.to_owned());
            request.org_id = (!org_id.is_empty()).then(|| org_id.to_owned());
            request.org_name = org_name.map(str::to_owned);

            let client = crate::http::api_client(
                &self.client.client(),
                self.api_url.as_str(),
                self.app_url.as_str(),
                token,
                org_name,
            )?;
            // Registration shares the upload scheduler, so it waits out 429
            // cooldowns, counts against request concurrency, and retries
            // transient failures instead of dropping the span on the first one.
            let retries = self.config.num_retries();
            let register_response = crate::internal::request::scheduled(scope, retries, || {
                client.register_project(&request)
            })
            .await?
            .into_inner();

            Ok::<_, anyhow::Error>(register_response.project.id)
        });
        resolve.await.cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> SpanPayload {
        SpanPayload {
            row_id: "row".into(),
            span_id: "child".into(),
            is_merge: false,
            span_components: None,
            org_id: "org".into(),
            org_name: None,
            project_name: None,
            input: None,
            output: None,
            expected: None,
            error: None,
            scores: None,
            metadata: None,
            metrics: None,
            tags: None,
            context: None,
            span_attributes: None,
            extra: Default::default(),
        }
    }

    fn parent(metadata: serde_json::Value) -> ParentSpanInfo {
        ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::ProjectLogs,
            object_id: None,
            compute_object_metadata_args: Some(metadata.as_object().unwrap().clone()),
            span_id: "parent".into(),
            root_span_id: "root".into(),
            span_parents: None,
            propagated_event: None,
        }
    }

    #[test]
    fn rejected_span_uses_metadata_project_id_and_preserves_its_ancestry() {
        let payload = payload();
        let parent = parent(serde_json::json!({"project_id": "project", "project_name": "name"}));
        let row = SpanRoute::new(&payload, Some(&parent))
            .unwrap()
            .into_known_row(payload)
            .expect("metadata supplies a usable project ID");
        assert_eq!(row.destination.project_id(), Some("project"));
        assert_eq!(row.root_span_id, "root");
        assert_eq!(row.span_parents, Some(vec!["parent".into()]));
    }

    #[test]
    fn rejected_span_with_only_a_project_name_has_no_debug_row() {
        let payload = payload();
        let parent = parent(serde_json::json!({"project_name": "name"}));
        assert!(SpanRoute::new(&payload, Some(&parent))
            .unwrap()
            .into_known_row(payload)
            .is_none());
    }
}
