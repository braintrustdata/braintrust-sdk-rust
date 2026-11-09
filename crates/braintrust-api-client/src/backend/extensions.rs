//! Typed operations for backend interfaces not represented completely in OpenAPI.
use super::{BackendClient, HttpError, WithHeaders};
use crate::types;
use anyhow::Result;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionListQuery {
    pub project_id: Option<String>,
    pub project_name: Option<String>,
    pub slug: Option<String>,
    pub id: Option<String>,
    pub version: Option<String>,
    pub environment: Option<String>,
    pub cursor: Option<String>,
    pub snapshot: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CodeUploadSlot {
    pub url: String,
    #[serde(rename = "bundleId")]
    pub bundle_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AutomationProjectRequest {
    pub automation_id: String,
    pub project_id: String,
}

impl AutomationProjectRequest {
    /// Create a AutomationProjectRequest with the required fields.
    pub fn new(automation_id: String, project_id: String) -> Self {
        Self {
            automation_id,
            project_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UpsertObjectCursorRequest {
    pub automation_id: String,
    pub object_id: String,
}

impl UpsertObjectCursorRequest {
    /// Create a UpsertObjectCursorRequest with the required fields.
    pub fn new(automation_id: String, object_id: String) -> Self {
        Self {
            automation_id,
            object_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResetObjectCursorRequest {
    pub automation_id: String,
    pub object_id: String,
    pub start_xact_id: String,
}

impl ResetObjectCursorRequest {
    /// Create a ResetObjectCursorRequest with the required fields.
    pub fn new(automation_id: String, object_id: String, start_xact_id: String) -> Self {
        Self {
            automation_id,
            object_id,
            start_xact_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RegisterTopicAutomationRequest {
    pub project_automation_name: String,
    pub description: String,
    pub project_id: String,
    pub config: RegisterTopicAutomationConfig,
    pub update: bool,
}

impl RegisterTopicAutomationRequest {
    /// Create a RegisterTopicAutomationRequest with the required fields.
    pub fn new(
        project_automation_name: String,
        description: String,
        project_id: String,
        config: RegisterTopicAutomationConfig,
        update: bool,
    ) -> Self {
        Self {
            project_automation_name,
            description,
            project_id,
            config,
            update,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RegisterTopicAutomationConfig {
    pub event_type: String,
    pub sampling_rate: f64,
    pub facet_functions: Vec<GlobalFacetFunctionRef>,
    pub topic_map_functions: Vec<TopicMapFunctionRef>,
    pub scope: TraceScopeConfig,
    pub rerun_seconds: i64,
    pub backfill_time_range: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub btql_filter: Option<String>,
}

impl RegisterTopicAutomationConfig {
    /// Create a RegisterTopicAutomationConfig with the required fields.
    pub fn new(
        event_type: String,
        sampling_rate: f64,
        facet_functions: Vec<GlobalFacetFunctionRef>,
        topic_map_functions: Vec<TopicMapFunctionRef>,
        scope: TraceScopeConfig,
        rerun_seconds: i64,
        backfill_time_range: String,
    ) -> Self {
        Self {
            event_type,
            sampling_rate,
            facet_functions,
            topic_map_functions,
            scope,
            rerun_seconds,
            backfill_time_range,
            btql_filter: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GlobalFacetFunctionRef {
    #[serde(rename = "type")]
    pub ref_type: String,
    pub name: String,
    pub function_type: String,
}

impl GlobalFacetFunctionRef {
    /// Create a GlobalFacetFunctionRef with the required fields.
    pub fn new(ref_type: String, name: String, function_type: String) -> Self {
        Self {
            ref_type,
            name,
            function_type,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopicMapFunctionRef {
    pub function: FunctionIdRef,
}

impl TopicMapFunctionRef {
    /// Create a TopicMapFunctionRef with the required fields.
    pub fn new(function: FunctionIdRef) -> Self {
        Self { function }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionIdRef {
    #[serde(rename = "type")]
    pub ref_type: String,
    pub id: String,
}

impl FunctionIdRef {
    /// Create a FunctionIdRef with the required fields.
    pub fn new(ref_type: String, id: String) -> Self {
        Self { ref_type, id }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TraceScopeConfig {
    #[serde(rename = "type")]
    pub scope_type: String,
    pub idle_seconds: i64,
}

impl TraceScopeConfig {
    /// Create a TraceScopeConfig with the required fields.
    pub fn new(scope_type: String, idle_seconds: i64) -> Self {
        Self {
            scope_type,
            idle_seconds,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopicMapReportUrlRequest<'a> {
    pub function_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<&'a str>,
}

impl<'a> TopicMapReportUrlRequest<'a> {
    /// Create a TopicMapReportUrlRequest with the required fields.
    pub fn new(function_id: &'a str) -> Self {
        Self {
            function_id,
            version: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[non_exhaustive]
pub struct AiProviderSecret {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub preview_secret: Option<String>,
    #[serde(default)]
    pub secret_updated_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
}

impl AiProviderSecret {
    /// Create a secret descriptor with no optional metadata.
    pub fn new(name: String) -> Self {
        Self {
            id: None,
            name,
            r#type: None,
            preview_secret: None,
            secret_updated_at: None,
            updated_at: None,
            created: None,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AttachmentUploadMetadata {
    pub signed_url: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionPage {
    pub objects: Vec<types::Function>,
    pub next_cursor: Option<String>,
    pub snapshot: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SignedUrl {
    pub url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TopicMapAssetRequest {
    pub function_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl TopicMapAssetRequest {
    /// Create a TopicMapAssetRequest with the required fields.
    pub fn new(function_id: String) -> Self {
        Self {
            function_id,
            version: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EnvironmentObject {
    pub id: String,
    pub object_type: String,
    pub object_id: String,
    pub object_version: String,
    pub environment_slug: String,
    pub environment_id: Option<String>,
    pub created: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EnvironmentAssignment {
    pub object_version: String,
    pub org_name: String,
}

impl EnvironmentAssignment {
    /// Create a EnvironmentAssignment with the required fields.
    pub fn new(object_version: String, org_name: String) -> Self {
        Self {
            object_version,
            org_name,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FunctionInsert {
    #[serde(flatten)]
    pub definition: types::CreateFunction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub if_exists: Option<String>,
}

impl FunctionInsert {
    /// Create a FunctionInsert with the required fields.
    pub fn new(definition: types::CreateFunction) -> Self {
        Self {
            definition,
            if_exists: None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InsertFunctionsRequest {
    pub functions: Vec<FunctionInsert>,
}

impl InsertFunctionsRequest {
    /// Create a InsertFunctionsRequest with the required fields.
    pub fn new(functions: Vec<FunctionInsert>) -> Self {
        Self { functions }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InsertedFunction {
    pub id: String,
    pub project_id: String,
    pub slug: String,
    pub found_existing: bool,
}

impl InsertedFunction {
    /// Describe a function returned by an insert operation.
    pub fn new(id: String, project_id: String, slug: String, found_existing: bool) -> Self {
        Self {
            id,
            project_id,
            slug,
            found_existing,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InsertFunctionsResponse {
    #[serde(default)]
    pub functions: Vec<InsertedFunction>,
    pub xact_id: Option<String>,
    pub ignored_count: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RuntimeContext {
    pub runtime: String,
    pub version: String,
}

impl RuntimeContext {
    /// Create a RuntimeContext with the required fields.
    pub fn new(runtime: String, version: String) -> Self {
        Self { runtime, version }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CodeUploadRequest {
    pub org_id: String,
    pub runtime_context: RuntimeContext,
}

impl CodeUploadRequest {
    /// Create a CodeUploadRequest with the required fields.
    pub fn new(org_id: String, runtime_context: RuntimeContext) -> Self {
        Self {
            org_id,
            runtime_context,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AttachmentRequest {
    pub key: String,
    pub filename: String,
    pub content_type: String,
    pub org_id: String,
}

impl AttachmentRequest {
    /// Create a AttachmentRequest with the required fields.
    pub fn new(key: String, filename: String, content_type: String, org_id: String) -> Self {
        Self {
            key,
            filename,
            content_type,
            org_id,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AttachmentStatusRequest {
    pub key: String,
    pub org_id: String,
    pub status: AttachmentStatus,
}

impl AttachmentStatusRequest {
    /// Create a AttachmentStatusRequest with the required fields.
    pub fn new(key: String, org_id: String, status: AttachmentStatus) -> Self {
        Self {
            key,
            org_id,
            status,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "upload_status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttachmentStatus {
    Done,
    Error { error_message: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AsyncQuerySubmission {
    pub query: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AsyncQuery {
    pub id: String,
    pub project_id: Option<String>,
    pub object_id: Option<String>,
    pub status: AsyncQueryState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AsyncQueryState {
    Queued,
    Running,
    Canceled,
    Succeeded,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AsyncQueryStatus {
    pub status: AsyncQueryState,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AsyncQueryPage {
    pub status: AsyncQueryState,
    #[serde(default)]
    pub rows: Vec<Map<String, Value>>,
    #[serde(default, deserialize_with = "super::btql::deserialize_cursor")]
    pub cursor: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AutomationPatch {
    pub id: String,
    #[serde(flatten)]
    pub patch: types::PatchProjectAutomation,
}

impl AutomationPatch {
    /// Create a AutomationPatch with the required fields.
    pub fn new(id: String, patch: types::PatchProjectAutomation) -> Self {
        Self { id, patch }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum AutomationRegistration {
    Wrapped {
        project_automation: types::ProjectAutomation,
    },
    Direct(types::ProjectAutomation),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AutomationCursors {
    pub total_segments: Option<u64>,
    pub pending_segments: Option<u64>,
    pub error_segments: Option<u64>,
    pub pending_min_compacted_xact_id: Option<Value>,
    pub pending_max_compacted_xact_id: Option<Value>,
    pub pending_min_executed_xact_id: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AutomationObjectCursors {
    pub total_objects: Option<u64>,
    pub due_objects: Option<u64>,
    pub error_objects: Option<u64>,
    pub last_compacted_xact_id: Option<Value>,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub retry_after: Option<String>,
    pub last_error: Option<String>,
    pub last_error_at: Option<String>,
    pub topic_runtime: Option<Value>,
}
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Invocation {
    pub function: types::FunctionId,
    pub arguments: types::InvokeApi,
}

impl Invocation {
    /// Create a Invocation with the required fields.
    pub fn new(function: types::FunctionId, arguments: types::InvokeApi) -> Self {
        Self {
            function,
            arguments,
        }
    }
}
impl<'de> Deserialize<'de> for Invocation {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self {
            function: serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?,
            arguments: serde_json::from_value(value).map_err(serde::de::Error::custom)?,
        })
    }
}
impl Serialize for Invocation {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(&self.arguments).map_err(serde::ser::Error::custom)?;
        let function = serde_json::to_value(&self.function).map_err(serde::ser::Error::custom)?;
        value
            .as_object_mut()
            .unwrap()
            .extend(function.as_object().unwrap().clone());
        value.serialize(serializer)
    }
}

impl BackendClient {
    fn query_client(&self) -> Self {
        if self.org_name().is_empty() {
            self.clone()
        } else {
            self.header("x-bt-org-name", self.org_name())
        }
    }
    pub async fn function_page(&self, query: &FunctionListQuery) -> Result<FunctionPage> {
        let mut params = Vec::new();
        for (name, value) in [
            ("project_id", &query.project_id),
            ("project_name", &query.project_name),
            ("slug", &query.slug),
            ("ids", &query.id),
            ("version", &query.version),
            ("environment", &query.environment),
            ("cursor", &query.cursor),
            ("snapshot", &query.snapshot),
        ] {
            if let Some(value) = value {
                params.push((name, value));
            }
        }
        let mut result: FunctionPage = self
            .get(&format!(
                "/v1/function?{}",
                serde_urlencoded::to_string(params)?
            ))
            .await?;
        result.next_cursor = result
            .next_cursor
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        result.snapshot = result
            .snapshot
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        Ok(result)
    }
    pub async fn dataset_snapshots(
        &self,
        dataset_id: &str,
    ) -> Result<types::GetDatasetSnapshotResponse> {
        self.get(&format!(
            "/v1/dataset_snapshot?{}",
            serde_urlencoded::to_string([("dataset_id", dataset_id)])?
        ))
        .await
    }
    pub async fn create_dataset_snapshot(
        &self,
        request: &types::CreateDatasetSnapshot,
    ) -> Result<SnapshotCreation> {
        let response: WithHeaders<types::DatasetSnapshot> =
            self.post("/v1/dataset_snapshot", request).await?;
        let found_existing = response
            .headers
            .get("x-bt-found-existing")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
        Ok(SnapshotCreation {
            dataset_snapshot: response.body,
            found_existing,
        })
    }
    pub async fn project_automations(
        &self,
        project_id: &str,
    ) -> Result<types::GetProjectAutomationResponse> {
        self.get(&format!(
            "/v1/project_automation?{}",
            serde_urlencoded::to_string([("project_id", project_id)])?
        ))
        .await
    }
    pub async fn assign_prompt(
        &self,
        prompt_id: &str,
        environment: &str,
        request: &EnvironmentAssignment,
    ) -> Result<EnvironmentObject> {
        self.put(
            &format!(
                "/environment-object/prompt/{}/{}",
                encode(prompt_id),
                encode(environment)
            ),
            request,
        )
        .await
    }
    pub async fn unassign_prompt(
        &self,
        prompt_id: &str,
        environment: &str,
    ) -> Result<EnvironmentObject> {
        self.delete_with_response(&format!(
            "/environment-object/prompt/{}/{}?{}",
            encode(prompt_id),
            encode(environment),
            serde_urlencoded::to_string([("org_name", self.org_name())])?
        ))
        .await
    }
    pub async fn invoke(&self, request: &Invocation) -> Result<Value> {
        let client = if self.org_name().is_empty() {
            self.clone()
        } else {
            self.header("x-bt-org-name", self.org_name())
        };
        client.post("/function/invoke", request).await
    }
    pub async fn code_upload(&self, request: &CodeUploadRequest) -> Result<CodeUploadSlot> {
        self.post("/function/code", request).await
    }
    pub async fn insert_functions(
        &self,
        request: &InsertFunctionsRequest,
    ) -> Result<InsertFunctionsResponse> {
        match self.post("/insert-functions", request).await {
            Err(error)
                if error
                    .downcast_ref::<HttpError>()
                    .is_some_and(|e| e.status.as_u16() == 422) =>
            {
                let response = error.downcast_ref::<HttpError>().unwrap();
                let report = serde_json::from_str(&response.body)?;
                Err(FunctionValidationError { report }.into())
            }
            result => result,
        }
    }
    pub async fn topic_map_report(&self, request: &TopicMapAssetRequest) -> Result<SignedUrl> {
        self.post("/topic-map-report-url", request).await
    }
    pub async fn topic_map_bundle(&self, request: &TopicMapAssetRequest) -> Result<SignedUrl> {
        self.post("/topic-map-btmap-url", request).await
    }
    pub async fn register_topic_automation(
        &self,
        request: &RegisterTopicAutomationRequest,
    ) -> Result<AutomationRegistration> {
        self.post("/api/project_automation/register", request).await
    }
    pub async fn patch_topic_automation(
        &self,
        request: &AutomationPatch,
    ) -> Result<AutomationRegistration> {
        self.post("/api/project_automation/patch_id", request).await
    }
    pub async fn automation_cursors(
        &self,
        request: &AutomationProjectRequest,
    ) -> Result<AutomationCursors> {
        self.post("/brainstore/automation/get-cursors", request)
            .await
    }
    pub async fn automation_object_cursors(
        &self,
        request: &AutomationProjectRequest,
    ) -> Result<AutomationObjectCursors> {
        self.post("/brainstore/automation/get-object-cursors", request)
            .await
    }
    pub async fn upsert_automation_object_cursor(
        &self,
        request: &UpsertObjectCursorRequest,
    ) -> Result<()> {
        let _: Value = self
            .post("/brainstore/automation/upsert-object-cursor", request)
            .await?;
        Ok(())
    }
    pub async fn reset_automation_cursors(&self, request: &ResetObjectCursorRequest) -> Result<()> {
        let _: Value = self
            .post("/brainstore/automation/reset-cursors", request)
            .await?;
        Ok(())
    }
    pub async fn request_attachment(
        &self,
        request: &AttachmentRequest,
    ) -> Result<AttachmentUploadMetadata> {
        self.post("/attachment", request).await
    }
    pub async fn attachment_status(&self, request: &AttachmentStatusRequest) -> Result<()> {
        let _: Value = self.post("/attachment/status", request).await?;
        Ok(())
    }
    pub async fn submit_query(&self, request: &AsyncQuerySubmission) -> Result<AsyncQuery> {
        self.query_client().post("/btql/async", request).await
    }
    pub async fn query_status(&self, id: &str) -> Result<AsyncQueryStatus> {
        self.query_client()
            .get(&format!("/btql/async/{}", encode(id)))
            .await
    }
    pub async fn query_page(
        &self,
        id: &str,
        limit: u64,
        cursor: Option<&str>,
    ) -> Result<AsyncQueryPage> {
        let mut params = vec![("limit", limit.to_string())];
        if let Some(cursor) = cursor {
            params.push(("cursor", cursor.to_owned()));
        }
        self.query_client()
            .get(&format!(
                "/btql/async/{}/result?{}",
                encode(id),
                serde_urlencoded::to_string(params)?
            ))
            .await
    }
    pub async fn ai_provider_secrets(&self) -> Result<Vec<AiProviderSecret>> {
        self.app_post(
            "/api/ai_secret/get",
            &serde_json::json!({"org_id":self.org_id()}),
        )
        .await
    }
    async fn app_post<T: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        path: &str,
        request: &T,
    ) -> Result<R> {
        let response = self
            .http
            .app_request(reqwest::Method::POST, path)
            .json(request)
            .send()
            .await
            .map_err(super::api_error)?;
        Ok(response.json().await?)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SnapshotCreation {
    pub dataset_snapshot: types::DatasetSnapshot,
    pub found_existing: bool,
}
fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateApiKey {
    pub name: String,
    pub org_id: String,
}

impl CreateApiKey {
    /// Create a CreateApiKey with the required fields.
    pub fn new(name: String, org_id: String) -> Self {
        Self { name, org_id }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreatedApiKey {
    pub key: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DatasetLookup {
    pub project_id: String,
    pub name: String,
}
impl BackendClient {
    pub async fn create_api_key(&self, request: &CreateApiKey) -> Result<CreatedApiKey> {
        self.post("/v1/api_key", request).await
    }
    pub async fn dataset_lookup(
        &self,
        dataset_id: &str,
        org_name: &str,
    ) -> Result<Vec<DatasetLookup>> {
        let response = self
            .http
            .app_request(reqwest::Method::POST, "api/dataset/get")
            .header("x-bt-org-name", org_name)
            .json(&serde_json::json!({"id":dataset_id}))
            .send()
            .await
            .map_err(super::api_error)?;
        Ok(response.json().await?)
    }
}

impl crate::Client {
    pub async fn lookup_dataset(
        &self,
        dataset_id: &str,
        org_name: &str,
    ) -> Result<Vec<DatasetLookup>> {
        let response = self
            .app_request(reqwest::Method::POST, "api/dataset/get")
            .header("x-bt-org-name", org_name)
            .json(&serde_json::json!({"id":dataset_id}))
            .send()
            .await
            .map_err(super::api_error)?;
        Ok(response.json().await?)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FunctionValidationSuggestion {
    pub action: String,
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FunctionValidationIssue {
    pub code: String,
    pub path: Vec<Value>,
    pub message: String,
    pub blocking: bool,
    #[serde(default)]
    pub suggestion: Option<FunctionValidationSuggestion>,
}

impl std::fmt::Display for FunctionValidationIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.code)?;
        } else {
            for (index, part) in self.path.iter().enumerate() {
                if index > 0 {
                    f.write_str(".")?;
                }
                match part.as_str() {
                    Some(part) => f.write_str(part)?,
                    None => write!(f, "{part}")?,
                }
            }
        }
        write!(f, ": {}", self.message)?;
        if let Some(suggestion) = &self.suggestion {
            match (suggestion.action.as_str(), &suggestion.value) {
                ("remove", _) => f.write_str("; suggestion: remove this parameter")?,
                ("set", Some(value)) => write!(f, "; suggestion: set it to {value}")?,
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FunctionValidationResult {
    pub issues: Vec<FunctionValidationIssue>,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct FunctionValidationReport {
    pub valid: bool,
    pub results: Vec<FunctionValidationResult>,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct FunctionValidationError {
    pub report: FunctionValidationReport,
}

impl FunctionValidationError {
    /// Create an error from a backend validation report.
    pub fn new(report: FunctionValidationReport) -> Self {
        Self { report }
    }
}

impl std::fmt::Display for FunctionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the backend rejected the function definition")?;
        for result in &self.report.results {
            for issue in &result.issues {
                write!(f, "; {issue}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for FunctionValidationError {}

pub fn is_provider_auth_response(error: &HttpError) -> bool {
    if error.status != reqwest::StatusCode::UNAUTHORIZED
        && error.status != reqwest::StatusCode::FORBIDDEN
    {
        return false;
    }

    let Ok(body) = serde_json::from_str::<Value>(&error.body) else {
        return false;
    };
    let provider_error = body.get("error").unwrap_or(&body);
    provider_error.get("code").and_then(Value::as_str) == Some("invalid_api_key")
        || provider_error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| {
                let message = message.to_ascii_lowercase();
                message.contains("incorrect api key provided")
                    || message.contains("llm provider") && message.contains("credential")
            })
}

impl BackendClient {
    pub async fn restore_dataset(
        &self,
        dataset_id: &str,
        version: &str,
    ) -> Result<types::DatasetRestoreResponse> {
        let request = types::DatasetRestoreRequest {
            version: version.into(),
        };
        let mut result = self
            .run(|| {
                self.generated()
                    .post_dataset_id_restore(dataset_id, request.clone())
            })
            .await?;
        result.xact_id = result
            .xact_id
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        if result.xact_id.is_none() && (result.rows_restored > 0 || result.rows_deleted > 0) {
            anyhow::bail!(
                "restore to xact '{}' changed rows ({} restored, {} deleted) but the response did not include a result xact",
                version,
                result.rows_restored,
                result.rows_deleted
            );
        }
        Ok(result)
    }
}

impl AsyncQuerySubmission {
    pub fn new(query: impl Into<String>, batch_size: Option<u64>) -> Self {
        Self {
            query: query.into(),
            format: "jsonl".into(),
            batch_size,
        }
    }
}
impl AsyncQueryState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Canceled | Self::Succeeded | Self::Failed)
    }
}
impl std::fmt::Display for AsyncQueryState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Canceled => "canceled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        })
    }
}
