//! Match write destinations for ordering across the span and raw row paths.
use crate::log_queue::{prepare::Destination, submission::PartitionDestination};
use crate::types::LogDestination;
use std::borrow::Cow;

#[derive(PartialEq, Eq)]
enum Kind {
    Project,
    Experiment,
    Dataset,
    Playground,
}

enum Target<'a> {
    Id(Kind, Cow<'a, str>),
    ProjectName(&'a str),
    Unknown,
}

#[derive(serde::Deserialize)]
struct RawTarget<'a> {
    #[serde(borrow)]
    project_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    experiment_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    dataset_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    prompt_session_id: Option<Cow<'a, str>>,
}

impl PartitionDestination {
    fn order_target(&self) -> Target<'_> {
        let (kind, id) = match self {
            Self::Span(Destination::Known(destination)) => match destination {
                LogDestination::ProjectLogs { project_id, .. } => (Kind::Project, project_id),
                LogDestination::Experiment { experiment_id } => (Kind::Experiment, experiment_id),
                LogDestination::Dataset { dataset_id, .. } => (Kind::Dataset, dataset_id),
                LogDestination::PlaygroundLogs {
                    prompt_session_id, ..
                } => (Kind::Playground, prompt_session_id),
            },
            Self::Span(Destination::ProjectName(name)) => return Target::ProjectName(name),
            Self::InvalidSpan => return Target::Unknown,
            Self::Raw(raw) => {
                let Ok(raw) = serde_json::from_str::<RawTarget<'_>>(raw) else {
                    return Target::Unknown;
                };
                let mut ids = [
                    (Kind::Project, raw.project_id),
                    (Kind::Experiment, raw.experiment_id),
                    (Kind::Dataset, raw.dataset_id),
                    (Kind::Playground, raw.prompt_session_id),
                ]
                .into_iter()
                .filter_map(|(kind, id)| id.map(|id| (kind, id)));
                return match (ids.next(), ids.next()) {
                    (Some((kind, id)), None) => Target::Id(kind, id),
                    _ => Target::Unknown,
                };
            }
        };
        Target::Id(kind, Cow::Borrowed(id))
    }

    /// A name may still resolve to a project's ID. Distinct known objects can
    /// proceed independently even when their callers reuse the same row ID.
    pub(super) fn aliases(&self, other: &Self) -> bool {
        if self == other {
            return true;
        }
        match (self.order_target(), other.order_target()) {
            (Target::Id(kind, id), Target::Id(other_kind, other_id)) => {
                kind == other_kind && id == other_id
            }
            (Target::ProjectName(name), Target::ProjectName(other_name)) => name == other_name,
            (Target::ProjectName(_), Target::Id(kind, _))
            | (Target::Id(kind, _), Target::ProjectName(_)) => kind == Kind::Project,
            (Target::Unknown, _) | (_, Target::Unknown) => true,
        }
    }
}
