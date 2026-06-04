use serde::de::DeserializeOwned;
use serde::Serialize;
use url::Url;

use crate::api::logs3::Logs3OverflowUpload;
use crate::api::session::LoginState;
use crate::api::transport::{decode_json_response, ensure_success_response, upload_signed_payload};
use crate::error::{BraintrustError, Result};

/// Low-level typed HTTP client for selected Braintrust API endpoints.
///
/// `ApiClient` is built from an authenticated [`LoginState`]. It keeps that
/// state as the source of truth for API URL, app URL, bearer token, and
/// organization context. It does no automatic retries or higher-level workflow
/// handling; methods return transport and API errors directly to the caller.
#[derive(Clone, Debug)]
pub struct ApiClient {
    login_state: LoginState,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ApiBase {
    Api,
    App,
}

impl ApiClient {
    /// Creates an API client from authenticated login state.
    ///
    /// Login remains outside the API layer. This constructor validates that the
    /// provided state is authenticated, then keeps it as the session source of
    /// truth.
    pub fn from_login_state(login_state: LoginState) -> Result<Self> {
        login_state
            .api_key()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
        login_state
            .org_id()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
        let api_url = login_state
            .api_url()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
        let app_url = login_state
            .app_url()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;

        parse_base_url(&api_url, "api_url")?;
        parse_base_url(&app_url, "app_url")?;

        Ok(Self { login_state })
    }

    /// Returns the projects resource client.
    pub fn projects(&self) -> crate::api::projects::ProjectsClient {
        crate::api::projects::ProjectsClient::new(self.clone())
    }

    /// Returns the datasets resource client.
    pub fn datasets(&self) -> crate::api::datasets::DatasetsClient {
        crate::api::datasets::DatasetsClient::new(self.clone())
    }

    /// Returns the experiments resource client.
    pub fn experiments(&self) -> crate::api::experiments::ExperimentsClient {
        crate::api::experiments::ExperimentsClient::new(self.clone())
    }

    /// Returns the project logs resource client.
    pub fn project_logs(&self) -> crate::api::project_logs::ProjectLogsClient {
        crate::api::project_logs::ProjectLogsClient::new(self.clone())
    }

    /// Returns the registration endpoint client.
    pub fn registrations(&self) -> crate::api::registrations::RegistrationsClient {
        crate::api::registrations::RegistrationsClient::new(self.clone())
    }

    /// Returns the BTQL endpoint client.
    pub fn btql(&self) -> crate::api::btql::BtqlClient {
        crate::api::btql::BtqlClient::new(self.clone())
    }

    /// Returns the summary endpoint client.
    pub fn summaries(&self) -> crate::api::summaries::SummariesClient {
        crate::api::summaries::SummariesClient::new(self.clone())
    }

    /// Returns the experiment comparison endpoint client.
    pub fn comparisons(&self) -> crate::api::comparisons::ComparisonsClient {
        crate::api::comparisons::ComparisonsClient::new(self.clone())
    }

    /// Returns the logs3 endpoint client.
    pub fn logs3(&self) -> crate::api::logs3::Logs3Client {
        crate::api::logs3::Logs3Client::new(self.clone())
    }

    /// Returns the functions resource client.
    pub fn functions(&self) -> crate::api::functions::FunctionsClient {
        crate::api::functions::FunctionsClient::new(self.clone())
    }

    /// Returns the prompts resource client.
    pub fn prompts(&self) -> crate::api::prompts::PromptsClient {
        crate::api::prompts::PromptsClient::new(self.clone())
    }

    /// Returns the project automations resource client.
    pub fn project_automations(&self) -> crate::api::project_automations::ProjectAutomationsClient {
        crate::api::project_automations::ProjectAutomationsClient::new(self.clone())
    }

    /// Returns the Brainstore automation endpoint client.
    pub fn brainstore_automation(
        &self,
    ) -> crate::api::brainstore_automation::BrainstoreAutomationClient {
        crate::api::brainstore_automation::BrainstoreAutomationClient::new(self.clone())
    }

    /// Returns the attachment endpoint client.
    pub fn attachments(&self) -> crate::api::attachments::AttachmentsClient {
        crate::api::attachments::AttachmentsClient::new(self.clone())
    }

    /// Returns the API keys resource client.
    pub fn api_keys(&self) -> crate::api::api_keys::ApiKeysClient {
        crate::api::api_keys::ApiKeysClient::new(self.clone())
    }

    /// Returns the AI secrets endpoint client.
    pub fn ai_secrets(&self) -> crate::api::ai_secrets::AiSecretsClient {
        crate::api::ai_secrets::AiSecretsClient::new(self.clone())
    }

    /// Returns the dataset lookup endpoint client.
    pub fn dataset_lookup(&self) -> crate::api::dataset_lookup::DatasetLookupClient {
        crate::api::dataset_lookup::DatasetLookupClient::new(self.clone())
    }

    pub(crate) fn org_id(&self) -> Result<String> {
        self.login_state
            .org_id()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))
    }

    pub(crate) fn org_name(&self) -> Option<String> {
        self.login_state
            .org_name()
            .filter(|org_name| !org_name.trim().is_empty())
    }

    pub(crate) async fn upload_signed_payload(
        &self,
        upload: &Logs3OverflowUpload,
        payload: &[u8],
    ) -> Result<()> {
        upload_signed_payload(self.login_state.http_client(), upload, payload).await
    }

    pub(crate) async fn get_json<T>(&self, path: &str, parse_context: &'static str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.get_json_from(ApiBase::Api, path, parse_context).await
    }

    pub(crate) async fn get_json_from<T>(
        &self,
        base: ApiBase,
        path: &str,
        parse_context: &'static str,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let response = self
            .authenticated(
                self.login_state
                    .http_client()
                    .get(self.endpoint(base, path)?),
            )?
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        decode_json_response(response, parse_context).await
    }

    pub(crate) async fn get_json_with_query<Q, T>(
        &self,
        path: &str,
        query: &Q,
        parse_context: &'static str,
    ) -> Result<T>
    where
        Q: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        self.get_json_with_query_from(ApiBase::Api, path, query, parse_context)
            .await
    }

    pub(crate) async fn get_json_with_query_from<Q, T>(
        &self,
        base: ApiBase,
        path: &str,
        query: &Q,
        parse_context: &'static str,
    ) -> Result<T>
    where
        Q: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let mut url = self.endpoint(base, path)?;
        let query = serde_html_form::to_string(query).map_err(|err| {
            BraintrustError::InvalidConfig(format!("invalid API query parameters: {err}"))
        })?;

        if !query.is_empty() {
            url.set_query(Some(&query));
        }

        let response = self
            .authenticated(self.login_state.http_client().get(url))?
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        decode_json_response(response, parse_context).await
    }

    pub(crate) async fn post_json<B, T>(
        &self,
        path: &str,
        body: &B,
        parse_context: &'static str,
    ) -> Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        self.post_json_to(ApiBase::Api, path, body, parse_context)
            .await
    }

    pub(crate) async fn post_json_to<B, T>(
        &self,
        base: ApiBase,
        path: &str,
        body: &B,
        parse_context: &'static str,
    ) -> Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let url = self.endpoint(base, path)?;

        let response = self
            .authenticated(self.login_state.http_client().post(url))?
            .json(body)
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        decode_json_response(response, parse_context).await
    }

    pub(crate) async fn post_bytes(&self, path: &str, body: Vec<u8>) -> Result<()> {
        let url = self.endpoint(ApiBase::Api, path)?;

        let response = self
            .authenticated(self.login_state.http_client().post(url))?
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        ensure_success_response(response).await
    }

    pub(crate) async fn patch_json<B, T>(
        &self,
        path: &str,
        body: &B,
        parse_context: &'static str,
    ) -> Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        self.patch_json_to(ApiBase::Api, path, body, parse_context)
            .await
    }

    pub(crate) async fn patch_json_to<B, T>(
        &self,
        base: ApiBase,
        path: &str,
        body: &B,
        parse_context: &'static str,
    ) -> Result<T>
    where
        B: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let url = self.endpoint(base, path)?;

        let response = self
            .authenticated(self.login_state.http_client().patch(url))?
            .json(body)
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        decode_json_response(response, parse_context).await
    }

    pub(crate) async fn delete_json<T>(&self, path: &str, parse_context: &'static str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.delete_json_from(ApiBase::Api, path, parse_context)
            .await
    }

    pub(crate) async fn delete_json_from<T>(
        &self,
        base: ApiBase,
        path: &str,
        parse_context: &'static str,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let url = self.endpoint(base, path)?;

        let response = self
            .authenticated(self.login_state.http_client().delete(url))?
            .send()
            .await
            .map_err(|err| BraintrustError::Network(err.to_string()))?;

        decode_json_response(response, parse_context).await
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        let api_key = self
            .login_state
            .api_key()
            .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?;
        let request = request.bearer_auth(api_key);
        if let Some(org_name) = self.org_name() {
            Ok(request.header("x-bt-org-name", org_name))
        } else {
            Ok(request)
        }
    }

    fn endpoint(&self, base: ApiBase, path: &str) -> Result<Url> {
        let (base_url, label) = match base {
            ApiBase::Api => (
                self.login_state
                    .api_url()
                    .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?,
                "api_url",
            ),
            ApiBase::App => (
                self.login_state
                    .app_url()
                    .ok_or_else(|| BraintrustError::InvalidConfig("Not logged in".into()))?,
                "app_url",
            ),
        };
        let base_url = parse_base_url(&base_url, label)?;

        base_url
            .join(path.trim_start_matches('/'))
            .map_err(|err| BraintrustError::InvalidConfig(format!("invalid API path: {err}")))
    }
}

pub(crate) fn path_segment(segment: &str) -> String {
    url::form_urlencoded::byte_serialize(segment.as_bytes()).collect()
}

fn parse_base_url(value: &str, label: &str) -> Result<Url> {
    let mut url = Url::parse(value)
        .map_err(|err| BraintrustError::InvalidConfig(format!("invalid {label}: {err}")))?;

    if !url.path().ends_with('/') {
        let mut path = url.path().to_string();
        path.push('/');
        url.set_path(&path);
    }

    Ok(url)
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn login_state(api_url: String, app_url: String, org_name: &str) -> LoginState {
        let state = LoginState::new();
        assert!(state.set(
            "token".to_string(),
            "org-id".to_string(),
            org_name.to_string(),
            api_url,
            app_url,
        ));
        state
    }

    #[test]
    fn from_login_state_rejects_missing_state() {
        let error = ApiClient::from_login_state(LoginState::new()).unwrap_err();

        assert!(matches!(error, BraintrustError::InvalidConfig(_)));
        assert!(error.to_string().contains("Not logged in"));
    }

    #[test]
    fn from_login_state_stores_authenticated_state() {
        let api = ApiClient::from_login_state(login_state(
            "https://api.example.test/root".to_string(),
            "https://app.example.test/app".to_string(),
            "Org",
        ))
        .expect("api client");

        assert_eq!(api.login_state.api_key().as_deref(), Some("token"));
        assert_eq!(api.org_id().expect("org id"), "org-id");
        assert_eq!(api.org_name().as_deref(), Some("Org"));
        assert_eq!(
            api.endpoint(ApiBase::Api, "")
                .expect("api endpoint")
                .as_str(),
            "https://api.example.test/root/"
        );
        assert_eq!(
            api.endpoint(ApiBase::App, "")
                .expect("app endpoint")
                .as_str(),
            "https://app.example.test/app/"
        );
    }

    #[tokio::test]
    async fn non_success_status_maps_to_api_error() {
        let server = MockServer::start().await;
        let api = ApiClient::from_login_state(login_state(server.uri(), server.uri(), "Org"))
            .expect("api client");

        Mock::given(method("GET"))
            .and(path("/v1/project/missing"))
            .and(header("x-bt-org-name", "Org"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server failed"))
            .mount(&server)
            .await;

        let error = api.projects().get("missing").await.unwrap_err();

        assert!(matches!(
            error,
            BraintrustError::Api {
                status: 500,
                ref message
            } if message == "server failed"
        ));
    }
}
