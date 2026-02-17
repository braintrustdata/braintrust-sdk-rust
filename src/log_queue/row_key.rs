use crate::types::Logs3Row;

/// Composite key for row merging (mirrors TS generateMergedRowKey).
/// Rows with the same key are merged when `is_merge: true`.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
pub(crate) struct RowKey {
    pub(crate) org_id: String,
    pub(crate) project_id: Option<String>,
    pub(crate) experiment_id: Option<String>,
    pub(crate) dataset_id: Option<String>,
    pub(crate) prompt_session_id: Option<String>,
    pub(crate) log_id: Option<String>,
    pub(crate) row_id: String,
}

impl RowKey {
    pub(crate) fn from_row(row: &Logs3Row) -> Self {
        Self {
            org_id: row.org_id.clone(),
            project_id: row.destination.project_id().map(|s| s.to_string()),
            experiment_id: row.destination.experiment_id().map(|s| s.to_string()),
            dataset_id: row.destination.dataset_id().map(|s| s.to_string()),
            prompt_session_id: row.destination.prompt_session_id().map(|s| s.to_string()),
            log_id: row.destination.log_id().map(|s| s.to_string()),
            row_id: row.id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogDestination;
    use chrono::Utc;

    #[test]
    fn test_row_key_from_row() {
        let row = Logs3Row {
            id: "row-1".to_string(),
            is_merge: None,
            span_id: "span-1".to_string(),
            root_span_id: "root-1".to_string(),
            span_parents: None,
            destination: LogDestination::experiment("exp-123"),
            org_id: "org-1".to_string(),
            org_name: None,
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
            created: Utc::now(),
        };

        let key = RowKey::from_row(&row);
        assert_eq!(key.org_id, "org-1");
        assert_eq!(key.row_id, "row-1");
        assert_eq!(key.experiment_id, Some("exp-123".to_string()));
    }
}
