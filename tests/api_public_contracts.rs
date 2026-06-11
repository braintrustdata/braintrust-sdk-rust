#![cfg(feature = "internal-api")]

use braintrust_sdk_rust::api::btql::{BTQLQuery, BTQLRequest, DatasetRecord};
use braintrust_sdk_rust::api::comparisons::{BaseExperimentRequest, ExperimentComparisonRequest};
use braintrust_sdk_rust::api::logs3::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3OverflowUploadRequest, LOGS_API_VERSION,
};
use braintrust_sdk_rust::api::registrations::{
    DatasetRegisterRequest, ExperimentRegisterRequest, ProjectRegisterRequest, RepoInfo,
};
use braintrust_sdk_rust::api::summaries::DatasetSummaryRequest;

#[test]
fn moved_api_contracts_are_public_and_default_based() {
    let _record_type: Option<DatasetRecord> = None;
    let mut btql = BTQLQuery::default();
    btql.from = "dataset('dataset-id')".to_string();
    btql.limit = Some(1);
    let _generic_btql = BTQLRequest::from_query_string("select * from dataset");

    let mut project_registration = ProjectRegisterRequest::default();
    project_registration.project_name = "Project".to_string();
    project_registration.org_id = Some("org-id".to_string());
    project_registration.org_name = Some("Org".to_string());

    let mut dataset_registration = DatasetRegisterRequest::default();
    dataset_registration.project_name = "Project".to_string();
    dataset_registration.org_name = "Org".to_string();
    dataset_registration.dataset_name = "Dataset".to_string();
    dataset_registration.description = Some("description".to_string());

    let mut repo_info = RepoInfo::default();
    repo_info.commit = Some("abcdef".to_string());
    repo_info.branch = Some("main".to_string());
    repo_info.dirty = Some(false);

    let mut experiment_registration = ExperimentRegisterRequest::default();
    experiment_registration.project_name = "Project".to_string();
    experiment_registration.org_id = "org-id".to_string();
    experiment_registration.experiment_name = Some("Experiment".to_string());
    experiment_registration.repo_info = Some(repo_info);
    experiment_registration.public = Some(false);

    let mut summary = DatasetSummaryRequest::default();
    summary.dataset_id = "dataset-id".to_string();

    let mut base = BaseExperimentRequest::default();
    base.id = "experiment-id".to_string();
    let mut comparison = ExperimentComparisonRequest::default();
    comparison.experiment_id = "experiment-id".to_string();
    comparison.base_experiment_id = Some("base-id".to_string());

    let mut overflow_meta = Logs3OverflowInputRowMeta::default();
    overflow_meta.byte_size = 10;
    let mut overflow_row = Logs3OverflowInputRow::default();
    overflow_row.object_ids = serde_json::Map::new();
    overflow_row.input_row = overflow_meta;
    assert_eq!(overflow_row.input_row().byte_size(), 10);

    let mut overflow_upload = Logs3OverflowUploadRequest::default();
    overflow_upload.content_type = "application/json".to_string();
    overflow_upload.size_bytes = 10;
    overflow_upload.rows = vec![overflow_row];
    assert_eq!(LOGS_API_VERSION, 2);
}
