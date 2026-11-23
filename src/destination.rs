//! Structural destination identities, including unresolved names.
use crate::types::{ParentSpanInfo, SpanPayload};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Destination {
    experiment: Option<String>,
    dataset: Option<String>,
    session: Option<String>,
    project: Option<String>,
    log: Option<String>,
    project_name: Option<String>,
}
impl Destination {
    pub fn row(row: &Map<String, Value>) -> Self {
        let field = |name| {
            row.get(name).map(|value: &Value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string())
            })
        };
        Self {
            experiment: field("experiment_id"),
            dataset: field("dataset_id"),
            session: field("prompt_session_id"),
            project: field("project_id"),
            log: field("log_id"),
            project_name: None,
        }
    }
    pub fn span(payload: &SpanPayload, parent: Option<&ParentSpanInfo>) -> Self {
        let mut value = Self {
            experiment: None,
            dataset: None,
            session: None,
            project: None,
            log: None,
            project_name: payload.project_name.clone(),
        };
        if let Some(components) = &payload.span_components {
            match components.object_type {
                crate::SpanObjectType::Experiment => {
                    value.experiment = components.object_id.clone()
                }
                crate::SpanObjectType::PlaygroundLogs => {
                    value.session = components.object_id.clone()
                }
                crate::SpanObjectType::ProjectLogs => match components.project_logs_identifier() {
                    Some(crate::ProjectLogsIdentifier::ObjectId(id)) => value.project = Some(id),
                    Some(crate::ProjectLogsIdentifier::ProjectName(name)) => {
                        value.project_name = Some(name)
                    }
                    None => {}
                },
            }
        } else if let Some(parent) = parent {
            match parent {
                ParentSpanInfo::Experiment { object_id } => {
                    value.experiment = Some(object_id.clone())
                }
                ParentSpanInfo::Dataset { object_id } => value.dataset = Some(object_id.clone()),
                ParentSpanInfo::ProjectLogs { object_id } => {
                    value.project = Some(object_id.clone())
                }
                ParentSpanInfo::PlaygroundLogs { object_id } => {
                    value.session = Some(object_id.clone())
                }
                ParentSpanInfo::ProjectName { project_name } => {
                    value.project_name = Some(project_name.clone())
                }
                ParentSpanInfo::FullSpan {
                    object_type,
                    object_id,
                    compute_object_metadata_args,
                    ..
                } => match object_type {
                    crate::SpanObjectType::Experiment => value.experiment = object_id.clone(),
                    crate::SpanObjectType::PlaygroundLogs => value.session = object_id.clone(),
                    crate::SpanObjectType::ProjectLogs => match crate::project_logs_identifier(
                        object_id.as_deref(),
                        compute_object_metadata_args.as_ref(),
                    ) {
                        Some(crate::ProjectLogsIdentifier::ObjectId(id)) => {
                            value.project = Some(id)
                        }
                        Some(crate::ProjectLogsIdentifier::ProjectName(name)) => {
                            value.project_name = Some(name)
                        }
                        None => {}
                    },
                },
            }
        }
        value
    }
}
