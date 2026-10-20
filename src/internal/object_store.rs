//! One-attempt signed object uploads, independent of Braintrust credentials.
use crate::api::logs3::Logs3OverflowUpload;
use crate::error::{BraintrustError, Result};
use bytes::Bytes;
use url::Url;

/// Only signed grant information reaches object storage. Braintrust credentials
/// are scoped to the API client, never added by this transport.
pub(crate) struct ObjectStoreTransport {
    pub(crate) http: reqwest::Client,
}
impl ObjectStoreTransport {
    pub(crate) async fn upload(&self, grant: &Logs3OverflowUpload, payload: Bytes) -> Result<()> {
        let url = Url::parse(&grant.signed_url)
            .map_err(|_| BraintrustError::InvalidConfig("invalid overflow signed URL".into()))?;
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(pairs) = &grant.headers {
            for (key, value) in pairs {
                let key =
                    reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|_| {
                        BraintrustError::InvalidConfig("invalid overflow header name".into())
                    })?;
                let value = reqwest::header::HeaderValue::from_str(value).map_err(|_| {
                    BraintrustError::InvalidConfig("invalid overflow header value".into())
                })?;
                headers.insert(key, value);
            }
        }
        let request = if grant.method.eq_ignore_ascii_case("POST") {
            let fields = grant.fields.as_ref().ok_or_else(|| {
                BraintrustError::InvalidConfig("overflow POST upload missing fields".into())
            })?;
            let content_type = fields
                .get("Content-Type")
                .map(String::as_str)
                .unwrap_or("application/json");
            let mut form = reqwest::multipart::Form::new();
            for (key, value) in fields {
                form = form.text(key.clone(), value.clone());
            }
            form = form.part(
                "file",
                reqwest::multipart::Part::bytes(payload.to_vec())
                    .mime_str(content_type)
                    .map_err(|_| {
                        BraintrustError::InvalidConfig("invalid overflow content type".into())
                    })?,
            );
            headers.remove(reqwest::header::CONTENT_TYPE);
            self.http.post(url).headers(headers).multipart(form)
        } else if grant.method.eq_ignore_ascii_case("PUT") {
            if url
                .host_str()
                .is_some_and(|host| host.ends_with(".blob.core.windows.net"))
                && !headers.contains_key("x-ms-blob-type")
            {
                headers.insert(
                    "x-ms-blob-type",
                    reqwest::header::HeaderValue::from_static("BlockBlob"),
                );
            }
            self.http
                .put(url)
                .header("content-type", "application/json")
                .headers(headers)
                .body(payload)
        } else {
            return Err(BraintrustError::InvalidConfig(
                "unsupported overflow upload method".into(),
            ));
        };
        let response = request
            .send()
            .await
            .map_err(|error| BraintrustError::Http(error.without_url()))?;
        if !response.status().is_success() {
            return Err(BraintrustError::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }
        Ok(())
    }
}
