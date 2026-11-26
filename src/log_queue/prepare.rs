//! Resolve resource destinations without borrowing global login state from another facade.
use super::queue::LogQueueCore;
use crate::span_components::{project_logs_identifier, ProjectLogsIdentifier};
use crate::types::{LogDestination, Logs3Row, ParentSpanInfo, SpanObjectType, SpanPayload};
use chrono::Utc;
use std::collections::HashMap;
use url::Url;

/// Project names that failed to resolve during one delivery job. Later spans for
/// the same name fail with the same error instead of repeating the request.
pub(super) type ProjectFailures = HashMap<String, anyhow::Error>;

impl LogQueueCore {
    /// Prepare a `Logs3Row` from the job's token, payload, and parent info.
    ///
    /// Resolves the destination (including calling the project registration API
    /// if needed) and builds the complete row for the flush pipeline.
    pub(super) async fn prepare_span(
        &self,
        token: &str,
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
        failures: &mut ProjectFailures,
    ) -> std::result::Result<Logs3Row, anyhow::Error> {
        let SpanPayload {
            row_id,
            span_id,
            is_merge,
            span_components,
            org_id,
            org_name,
            project_name,
            input,
            output,
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
            extra,
        } = payload;

        if let Some(span_components) = span_components {
            let destination = self
                .destination_from_span_components(
                    token,
                    &org_id,
                    org_name.as_deref(),
                    &span_components,
                    failures,
                )
                .await?;
            let root_span_id = span_components
                .root_span_id
                .clone()
                .unwrap_or_else(|| span_id.clone());

            return Ok(Logs3Row {
                id: row_id,
                is_merge: if is_merge { Some(true) } else { None },
                merge_paths: None,
                span_id,
                root_span_id,
                span_parents: span_components.span_parents.clone(),
                destination,
                org_id,
                org_name,
                input,
                output,
                expected,
                error,
                scores,
                metadata,
                metrics,
                tags,
                context,
                span_attributes,
                extra,
                created: Utc::now(),
                xact_id: None,
                object_delete: None,
                audit_source: Some("api".to_string()),
            });
        }

        let project_id = if parent_info.is_none() {
            if let Some(ref pn) = project_name {
                Some(
                    self.ensure_project_id(token, &org_id, org_name.as_deref(), pn, failures)
                        .await?,
                )
            } else {
                None
            }
        } else {
            None
        };

        let (root_span_id, span_parents, destination) = match parent_info {
            None => {
                let dest = match project_id {
                    Some(pid) => LogDestination::project_logs(pid),
                    None => {
                        anyhow::bail!("no destination: either parent_info or project_name required")
                    }
                };
                (span_id.clone(), None, dest)
            }
            Some(ParentSpanInfo::Experiment { object_id }) => {
                (span_id.clone(), None, LogDestination::experiment(object_id))
            }
            Some(ParentSpanInfo::ProjectLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::project_logs(object_id),
            ),
            Some(ParentSpanInfo::ProjectName { project_name }) => {
                let proj_id = self
                    .ensure_project_id(token, &org_id, org_name.as_deref(), &project_name, failures)
                    .await?;
                (span_id.clone(), None, LogDestination::project_logs(proj_id))
            }
            Some(ParentSpanInfo::PlaygroundLogs { object_id }) => (
                span_id.clone(),
                None,
                LogDestination::playground_logs(object_id),
            ),
            Some(ParentSpanInfo::Dataset { object_id }) => {
                (span_id.clone(), None, LogDestination::dataset(object_id))
            }
            Some(ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                compute_object_metadata_args,
                span_id: parent_span_id,
                root_span_id: parent_root_span_id,
                span_parents: _,
                propagated_event: _,
            }) => {
                let span_parents = Some(vec![parent_span_id]);
                let dest = match object_type {
                    SpanObjectType::Experiment => {
                        LogDestination::experiment(object_id.ok_or_else(|| {
                            anyhow::anyhow!("experiment parent span is missing object_id")
                        })?)
                    }
                    SpanObjectType::ProjectLogs => {
                        match project_logs_identifier(
                            object_id.as_deref(),
                            compute_object_metadata_args.as_ref(),
                        ) {
                            Some(ProjectLogsIdentifier::ObjectId(object_id)) => {
                                LogDestination::project_logs(object_id)
                            }
                            Some(ProjectLogsIdentifier::ProjectName(project_name)) => {
                                let project_id = self
                                    .ensure_project_id(
                                        token,
                                        &org_id,
                                        org_name.as_deref(),
                                        &project_name,
                                        failures,
                                    )
                                    .await?;
                                LogDestination::project_logs(project_id)
                            }
                            None => {
                                anyhow::bail!(
                                    "project-log parent span is missing object_id, project_id, or project_name"
                                )
                            }
                        }
                    }
                    SpanObjectType::PlaygroundLogs => {
                        LogDestination::playground_logs(object_id.ok_or_else(|| {
                            anyhow::anyhow!("playground parent span is missing object_id")
                        })?)
                    }
                };
                (parent_root_span_id, span_parents, dest)
            }
        };

        Ok(Logs3Row {
            id: row_id,
            is_merge: if is_merge { Some(true) } else { None },
            merge_paths: None,
            span_id,
            root_span_id,
            span_parents,
            destination,
            org_id,
            org_name,
            input,
            output,
            expected,
            error,
            scores,
            metadata,
            metrics,
            tags,
            context,
            span_attributes,
            extra,
            created: Utc::now(),
            xact_id: None,
            object_delete: None,
            audit_source: Some("api".to_string()),
        })
    }

    async fn destination_from_span_components(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        span_components: &crate::span_components::SpanComponents,
        failures: &mut ProjectFailures,
    ) -> std::result::Result<LogDestination, anyhow::Error> {
        match span_components.object_type {
            SpanObjectType::Experiment => span_components
                .object_id
                .as_ref()
                .map(|object_id| LogDestination::experiment(object_id.clone()))
                .ok_or_else(|| anyhow::anyhow!("experiment span is missing object_id")),
            SpanObjectType::ProjectLogs => match span_components.project_logs_identifier() {
                Some(ProjectLogsIdentifier::ObjectId(object_id)) => {
                    Ok(LogDestination::project_logs(object_id))
                }
                Some(ProjectLogsIdentifier::ProjectName(project_name)) => {
                    let project_id = self
                        .ensure_project_id(token, org_id, org_name, &project_name, failures)
                        .await?;
                    Ok(LogDestination::project_logs(project_id))
                }
                None => anyhow::bail!(
                    "project-log span is missing object_id, project_id, or project_name"
                ),
            },
            SpanObjectType::PlaygroundLogs => span_components
                .object_id
                .as_ref()
                .map(|object_id| LogDestination::playground_logs(object_id.clone()))
                .ok_or_else(|| anyhow::anyhow!("playground span is missing object_id")),
        }
    }

    /// Ensure a project ID is available for the given project name, registering
    /// it via the API if it is not yet cached.
    ///
    /// Uses `login_state.app_url()` for the registration endpoint so the URL
    /// doesn't need to be passed separately.
    async fn ensure_project_id(
        &self,
        token: &str,
        org_id: &str,
        org_name: Option<&str>,
        project_name: &str,
        failures: &mut ProjectFailures,
    ) -> std::result::Result<String, anyhow::Error> {
        if let Some(error) = failures.get(project_name) {
            return Err(super::observation::duplicate_error(error));
        }
        let app_url_key = self
            .login_state
            .app_url()
            .unwrap_or_else(|| self.app_url.to_string());
        let org = crate::resolution::OrgIdentity::new(org_id, org_name, token);
        let scope = crate::scheduler::Scope::new(app_url_key.clone(), org.scope_key());
        let key = crate::resolution::ProjectKey {
            app_url: app_url_key,
            org,
            project: project_name.into(),
        };
        let entry = crate::service::global()?.projects.entry(key);
        let timeout = crate::service::global()?.config.preparation_timeout;
        let resolve = entry.get_or_try_init(|| async {
            // Prefer the login-state control-plane URL (logged-in path), falling back
            // to the client-configured URL for per-request-credential callers.
            let app_url = match self.login_state.app_url() {
                Some(app_url_str) => {
                    Url::parse(&app_url_str).map_err(|e| anyhow::anyhow!("invalid app URL: {e}"))?
                }
                None => self.app_url.clone(),
            };
            let mut request = crate::api::ProjectRegisterRequest::new(project_name.to_owned());
            request.org_id = (!org_id.is_empty()).then(|| org_id.to_owned());
            request.org_name = org_name.map(str::to_owned);

            let client = crate::http::api_client(
                &self.client,
                self.api_url.as_str(),
                app_url.as_str(),
                token,
                org_name,
            )?;
            // Registration shares the upload scheduler, so it waits out 429
            // cooldowns, counts against request concurrency, and retries
            // transient failures instead of dropping the span on the first one.
            let retries = self.config.num_retries();
            let register_response = crate::internal::upload::scheduled(scope, retries, || {
                client.register_project(&request)
            })
            .await?
            .into_inner();

            Ok::<_, anyhow::Error>(register_response.project.id)
        });
        let result = match tokio::time::timeout(timeout, resolve).await {
            Ok(result) => result.cloned(),
            Err(_) => Err(anyhow::anyhow!("project registration timed out")),
        };
        if let Err(error) = &result {
            failures.insert(
                project_name.to_owned(),
                super::observation::duplicate_error(error),
            );
        }
        result
    }
}
