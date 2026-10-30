//! HTTP operations used by SDKs but absent from the public OpenAPI specification.
//!
//! These methods perform one request. Callers own batching, retry policy, and
//! background delivery. Errors preserve unsuccessful responses for inspection.

use std::collections::HashMap;

use bytes::Bytes;
use reqwest::{Method, RequestBuilder, Response};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::{Client, Error, ResponseValue};

/// Result from an API operation not described by OpenAPI.
pub type TransportResult<T> = Result<T, Error<Value>>;

/// Organization information returned by API-key login.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoginOrganization {
    pub id: String,
    pub name: String,
    pub api_url: Option<String>,
    pub proxy_url: Option<String>,
    pub realtime_url: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Available organizations and their data-plane endpoints.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LoginResponse {
    pub org_info: Vec<LoginOrganization>,
}

/// Server capabilities used to choose logging transport limits.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VersionInfo {
    pub logs3_payload_max_bytes: Option<usize>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// JSON BTQL result. Query-specific record fields remain JSON values.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BtqlResponse {
    #[serde(default)]
    pub data: Vec<Value>,
    pub cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Destination and size metadata for a row in an overflow payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Logs3OverflowInputRow {
    pub object_ids: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delete: Option<bool>,
    pub input_row: Logs3OverflowInputRowMeta,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Logs3OverflowInputRowMeta {
    pub byte_size: usize,
}

/// Request a signed storage destination for a serialized logs3 payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Logs3OverflowRequest {
    pub rows: Vec<Logs3OverflowInputRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

/// A signed upload instruction returned by `/logs3/overflow`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Logs3OverflowUpload {
    pub method: String,
    pub signed_url: String,
    pub headers: Option<HashMap<String, String>>,
    pub fields: Option<HashMap<String, String>>,
    pub key: String,
}

/// The logs3 reference submitted after a successful signed upload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Logs3OverflowReference {
    #[serde(rename = "type")]
    pub reference_type: String,
    pub key: String,
}

impl Logs3OverflowUpload {
    /// Reference to submit to logs3 after this upload succeeds.
    pub fn reference(&self) -> Logs3OverflowReference {
        Logs3OverflowReference {
            reference_type: "logs3_overflow".into(),
            key: self.key.clone(),
        }
    }
}

// Match the generated methods' upstream error type without adding a boxed error API.
#[allow(clippy::result_large_err)]
impl Client {
    /// List organizations for the configured API key using the app endpoint.
    ///
    /// Does not select an organization or change this client's API endpoint.
    pub async fn login(&self) -> TransportResult<ResponseValue<LoginResponse>> {
        let request = self
            .request(Method::POST, &self.inner.app_url, "api/apikey/login")
            .header("content-type", "application/json");
        self.send_json(request).await
    }

    /// Fetch data-plane version and logging capabilities.
    pub async fn version(&self) -> TransportResult<ResponseValue<VersionInfo>> {
        self.send_json(self.logging_request(Method::GET, "version"))
            .await
    }

    /// Execute a SQL string or structured BTQL query, wrapped in a `query` field.
    ///
    /// The request may include `fmt`, pagination, and other backend query options.
    /// Returns the response directly to support JSON, JSONL, and Parquet formats.
    pub async fn btql<T: Serialize + ?Sized>(&self, request: &T) -> TransportResult<Response> {
        self.send(
            self.request(Method::POST, &self.baseurl, "btql")
                .json(request),
        )
        .await
    }

    /// Execute BTQL in JSON format, preserving rows, cursor, and extra result fields.
    pub async fn btql_json<T: Serialize + ?Sized>(
        &self,
        request: &T,
    ) -> TransportResult<ResponseValue<BtqlResponse>> {
        let mut request = serde_json::to_value(request)
            .map_err(|error| Error::invalid_request(error.to_string()))?;
        let object = request
            .as_object_mut()
            .ok_or_else(|| Error::invalid_request("BTQL request must be an object".into()))?;
        object.insert("fmt".into(), Value::String("json".into()));
        ResponseValue::from_response(self.btql(&request).await?).await
    }

    /// Submit a JSON logs3 payload, including its `rows` and `api_version` fields.
    pub async fn logs3<T: Serialize + ?Sized>(
        &self,
        payload: &T,
    ) -> TransportResult<ResponseValue<()>> {
        let response = self
            .send(self.logging_request(Method::POST, "logs3").json(payload))
            .await?;
        Ok(ResponseValue::empty(response))
    }

    /// Submit already-serialized logs3 JSON without parsing or reserializing it.
    pub async fn logs3_raw(&self, payload: impl Into<Bytes>) -> TransportResult<ResponseValue<()>> {
        let response = self
            .send(
                self.logging_request(Method::POST, "logs3")
                    .header("content-type", "application/json")
                    .body(payload.into()),
            )
            .await?;
        Ok(ResponseValue::empty(response))
    }

    /// Request a signed upload destination for an oversized logs3 batch.
    pub async fn logs3_overflow(
        &self,
        request: &Logs3OverflowRequest,
    ) -> TransportResult<ResponseValue<Logs3OverflowUpload>> {
        self.logs3_overflow_with_org_name(request, None).await
    }

    /// Request an overflow destination with an explicit organization header.
    pub async fn logs3_overflow_with_org_name(
        &self,
        request: &Logs3OverflowRequest,
        org_name: Option<&str>,
    ) -> TransportResult<ResponseValue<Logs3OverflowUpload>> {
        let mut request = self
            .request(Method::POST, &self.baseurl, "logs3/overflow")
            .json(request);
        if let Some(name) = org_name {
            request = request.header("x-bt-org-name", name);
        }
        self.send_json(request).await
    }

    /// Upload a logs3 payload using the supplied signed POST or PUT instruction.
    ///
    /// Uses the separate upload transport, without Braintrust authentication or
    /// organization headers. Submit the returned key to [`Self::logs3`] afterwards.
    pub async fn upload_logs3_overflow(
        &self,
        upload: &Logs3OverflowUpload,
        payload: impl Into<Bytes>,
    ) -> TransportResult<ResponseValue<()>> {
        let payload = payload.into();
        let url = reqwest::Url::parse(&upload.signed_url)
            .map_err(|error| Error::invalid_request(error.to_string()))?;
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(values) = &upload.headers {
            for (key, value) in values {
                let key = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    .map_err(|error| Error::invalid_request(error.to_string()))?;
                let value = reqwest::header::HeaderValue::from_str(value)
                    .map_err(|error| Error::invalid_request(error.to_string()))?;
                headers.insert(key, value);
            }
        }
        let request = if upload.method.eq_ignore_ascii_case("POST") {
            headers.remove(reqwest::header::CONTENT_TYPE);
            let fields = upload.fields.as_ref().ok_or_else(|| {
                Error::invalid_request("overflow POST upload requires form fields".into())
            })?;
            let mut form = reqwest::multipart::Form::new();
            for (key, value) in fields {
                form = form.text(key.clone(), value.clone());
            }
            let content_type = fields
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
                .map(|(_, value)| value.as_str())
                .unwrap_or("application/json");
            let file = reqwest::multipart::Part::bytes(payload.to_vec()).mime_str(content_type)?;
            self.inner
                .upload_client
                .post(url)
                .multipart(form.part("file", file))
        } else if upload.method.eq_ignore_ascii_case("PUT") {
            let is_azure = url
                .host_str()
                .is_some_and(|host| host.ends_with(".blob.core.windows.net"));
            headers.entry(reqwest::header::CONTENT_TYPE).or_insert(
                reqwest::header::HeaderValue::from_static("application/json"),
            );
            if is_azure {
                headers
                    .entry("x-ms-blob-type")
                    .or_insert(reqwest::header::HeaderValue::from_static("BlockBlob"));
            }
            self.inner.upload_client.put(url).body(payload)
        } else {
            return Err(Error::invalid_request(
                "overflow upload method must be POST or PUT".into(),
            ));
        };
        let response = request.headers(headers).send().await?;
        if !response.status().is_success() {
            return Err(crate::response::response_error(response).await?);
        }
        Ok(ResponseValue::empty(response))
    }

    /// Call an app control-plane action not available through the public REST API.
    ///
    /// For example, `control_plane("project", "register", &request)`. The response
    /// retains the app endpoint's original shape; no REST translation is applied.
    pub async fn control_plane<T: Serialize + ?Sized>(
        &self,
        object_type: &str,
        action: &str,
        request: &T,
    ) -> TransportResult<Response> {
        if [object_type, action].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        }) {
            return Err(Error::invalid_request(
                "control-plane object and action must be path segments".into(),
            ));
        }
        let path = format!("api/{object_type}/{action}");
        self.send(
            self.request(Method::POST, &self.inner.app_url, &path)
                .json(request),
        )
        .await
    }

    /// Call an app action and decode its response into the requested type.
    pub async fn control_plane_json<T: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        object_type: &str,
        action: &str,
        request: &T,
    ) -> TransportResult<ResponseValue<R>> {
        ResponseValue::from_response(self.control_plane(object_type, action, request).await?).await
    }

    /// Fetch experiment comparison data from the data plane.
    pub async fn experiment_comparison(
        &self,
        experiment_id: &str,
        base_experiment_id: Option<&str>,
    ) -> TransportResult<ResponseValue<Value>> {
        let mut request = self
            .request(Method::GET, &self.baseurl, "experiment-comparison2")
            .query(&[("experiment_id", experiment_id)]);
        if let Some(base) = base_experiment_id {
            request = request.query(&[("base_experiment_id", base)]);
        }
        self.send_json(request).await
    }

    /// Fetch a legacy dataset summary from the data plane.
    pub async fn dataset_summary(&self, dataset_id: &str) -> TransportResult<ResponseValue<Value>> {
        self.send_json(
            self.request(Method::GET, &self.baseurl, "dataset-summary")
                .query(&[("dataset_id", dataset_id)]),
        )
        .await
    }

    fn request(&self, method: Method, base: &str, path: &str) -> RequestBuilder {
        let url = format!("{}/{path}", base.trim_end_matches('/'));
        self.client.request(method, url)
    }

    fn logging_request(&self, method: Method, path: &str) -> RequestBuilder {
        let mut request = self.request(method, &self.baseurl, path);
        if let Some(name) = &self.inner.org_name {
            request = request.header("x-bt-org-name", name.clone());
        }
        request
    }

    async fn send(&self, request: RequestBuilder) -> TransportResult<Response> {
        let mut request = request.build()?;
        crate::client::authorize_request(&self.inner, &mut request);
        let response = self.client.execute(request).await?;
        if !response.status().is_success() {
            return Err(crate::response::response_error(response).await?);
        }
        Ok(response)
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
    ) -> TransportResult<ResponseValue<T>> {
        ResponseValue::from_response(self.send(request).await?).await
    }
}
