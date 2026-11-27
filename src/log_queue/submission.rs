use super::{credential::Credential, prepare::SpanRoute};
use crate::types::{ParentSpanInfo, SpanPayload};
use serde_json::{Map, Value};

pub(crate) struct SubmitCommand {
    pub(crate) credential: Credential,
    pub(super) item: QueuedItem,
}

// Spans are the common case; boxing them would add an allocation per span.
#[allow(clippy::large_enum_variant)]
pub(super) enum QueuedItem {
    /// A span payload, turned into a row at flush time.
    Span {
        payload: SpanPayload,
        parent_info: Option<ParentSpanInfo>,
    },
    /// A caller-built row, uploaded as given.
    Row {
        org_id: String,
        org_name: Option<String>,
        row: Map<String, Value>,
    },
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) enum PartitionDestination {
    Span(super::prepare::Destination),
    InvalidSpan,
    Raw(String),
}

impl SubmitCommand {
    #[cfg(test)]
    pub(crate) fn test_row(id: &str) -> Self {
        Self::test_scoped_row(id, "project", "org")
    }

    #[cfg(test)]
    pub(crate) fn test_scoped_row(id: &str, project: &str, org: &str) -> Self {
        Self {
            credential: Credential::token_value("token".into()),
            item: QueuedItem::Row {
                org_id: org.into(),
                org_name: None,
                row: serde_json::json!({"id":id,"project_id":project})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
        }
    }
    #[cfg(test)]
    pub(crate) fn test_span(is_merge: bool) -> Self {
        Self {
            credential: Credential::token_value("token".into()),
            item: QueuedItem::Span {
                payload: SpanPayload {
                    row_id: "same-row".into(),
                    span_id: "span".into(),
                    is_merge,
                    span_components: None,
                    org_id: "org".into(),
                    org_name: None,
                    project_name: None,
                    input: None,
                    output: Some(serde_json::json!({"merge":is_merge})),
                    expected: None,
                    error: None,
                    scores: None,
                    metadata: None,
                    metrics: None,
                    tags: None,
                    context: None,
                    span_attributes: None,
                    extra: Default::default(),
                },
                parent_info: Some(ParentSpanInfo::ProjectLogs {
                    object_id: "project".into(),
                }),
            },
        }
    }
    #[cfg(test)]
    pub(crate) fn test_named_span(row_id: &str, project_name: &str) -> Self {
        let mut command = Self::test_span(false);
        if let QueuedItem::Span {
            payload,
            parent_info,
        } = &mut command.item
        {
            payload.row_id = row_id.into();
            payload.span_id = row_id.into();
            payload.project_name = Some(project_name.into());
            *parent_info = None;
        }
        command
    }
    /// The organization supplied when the command was accepted.
    pub(super) fn organization(&self) -> (&str, Option<&str>) {
        match &self.item {
            QueuedItem::Span { payload, .. } => (&payload.org_id, payload.org_name.as_deref()),
            QueuedItem::Row {
                org_id, org_name, ..
            } => (org_id, org_name.as_deref()),
        }
    }

    pub(super) fn row_id(&self) -> Option<&str> {
        match &self.item {
            QueuedItem::Span { payload, .. } => Some(&payload.row_id),
            QueuedItem::Row { row, .. } => row.get("id").and_then(serde_json::Value::as_str),
        }
    }

    pub(super) fn destination(&self) -> PartitionDestination {
        match &self.item {
            QueuedItem::Span {
                payload,
                parent_info,
            } => SpanRoute::new(payload, parent_info.as_ref())
                .map_or(PartitionDestination::InvalidSpan, |route| {
                    PartitionDestination::Span(route.destination)
                }),
            QueuedItem::Row { row, .. } => {
                PartitionDestination::Raw(crate::internal::ingest::raw_destination_key(row))
            }
        }
    }
}
