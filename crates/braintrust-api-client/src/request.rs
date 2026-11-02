use std::time::Duration;

use reqwest::{IntoUrl, Method, Response};
use serde::Serialize;

use crate::{Client, transport::TransportResult};

/// HTTP transport without implicit credentials or environment configuration.
///
/// Use this for OAuth, browser setup and signed URLs. Authenticated Braintrust
/// requests should use [`Client::api_request`] or [`Client::app_request`].
#[derive(Clone)]
pub struct Transport {
    client: reqwest::Client,
}

// Keep the same error envelope as generated API operations.
#[allow(clippy::result_large_err)]
impl Transport {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Upload bytes to a signed storage URL without adding authentication.
    pub async fn put_signed_url(
        &self,
        url: &str,
        body: impl Into<reqwest::Body>,
        headers: &std::collections::HashMap<String, String>,
    ) -> TransportResult<Response> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|error| crate::Error::invalid_request(error.to_string()))?;
        let azure = parsed
            .host_str()
            .is_some_and(|host| host.ends_with(".blob.core.windows.net"));
        let mut request = self.put(parsed).body(body);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if azure
            && !headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("x-ms-blob-type"))
        {
            request = request.header("x-ms-blob-type", "BlockBlob");
        }
        request.send().await
    }

    pub fn request(&self, method: Method, url: impl IntoUrl) -> Request {
        Request {
            inner: self.client.request(method, url),
        }
    }

    pub fn get(&self, url: impl IntoUrl) -> Request {
        self.request(Method::GET, url)
    }

    pub fn post(&self, url: impl IntoUrl) -> Request {
        self.request(Method::POST, url)
    }

    pub fn put(&self, url: impl IntoUrl) -> Request {
        self.request(Method::PUT, url)
    }
}

/// A configurable request for API operations that need a raw response.
///
/// No retries are performed. The body can be cloned for caller-owned retries.
pub struct Request {
    inner: reqwest::RequestBuilder,
}

#[allow(clippy::result_large_err)]
impl Request {
    pub fn header<K, V>(mut self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        <reqwest::header::HeaderName as TryFrom<K>>::Error: Into<http::Error>,
        reqwest::header::HeaderValue: TryFrom<V>,
        <reqwest::header::HeaderValue as TryFrom<V>>::Error: Into<http::Error>,
    {
        self.inner = self.inner.header(key, value);
        self
    }

    pub fn bearer_auth(mut self, token: impl std::fmt::Display) -> Self {
        self.inner = self.inner.bearer_auth(token);
        self
    }

    pub fn query<T: Serialize + ?Sized>(mut self, query: &T) -> Self {
        self.inner = self.inner.query(query);
        self
    }

    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        self.inner = self.inner.json(body);
        self
    }

    pub fn form<T: Serialize + ?Sized>(mut self, body: &T) -> Self {
        self.inner = self.inner.form(body);
        self
    }

    pub fn body(mut self, body: impl Into<reqwest::Body>) -> Self {
        self.inner = self.inner.body(body);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.inner = self.inner.timeout(timeout);
        self
    }

    pub fn try_clone(&self) -> Option<Self> {
        self.inner.try_clone().map(|inner| Self { inner })
    }

    /// Validate and serialize a request once before a caller-owned retry loop.
    pub fn build(self) -> reqwest::Result<PreparedRequest> {
        let (client, request) = self.inner.build_split();
        Ok(PreparedRequest {
            client,
            request: request?,
        })
    }

    /// Send once and preserve unsuccessful responses in the API error envelope.
    pub async fn send(self) -> TransportResult<Response> {
        self.build()?.send().await
    }

    /// Send once, leaving status interpretation to a protocol-specific caller.
    ///
    /// Useful for OAuth error responses and polling.
    pub async fn send_unchecked(self) -> reqwest::Result<Response> {
        self.build()?.send_unchecked().await
    }
}

/// A validated request that can be replayed without serializing its body again.
pub struct PreparedRequest {
    client: reqwest::Client,
    request: reqwest::Request,
}

#[allow(clippy::result_large_err)]
impl PreparedRequest {
    pub fn try_clone(&self) -> Option<Self> {
        self.request.try_clone().map(|request| Self {
            client: self.client.clone(),
            request,
        })
    }

    pub async fn send(self) -> TransportResult<Response> {
        let response = self.send_unchecked().await?;
        if !response.status().is_success() {
            return Err(crate::response::response_error(response).await);
        }
        Ok(response)
    }

    pub async fn send_unchecked(self) -> reqwest::Result<Response> {
        self.client.execute(self.request).await
    }
}

impl Client {
    /// Make a request under the configured API URL, with this client's key.
    ///
    /// The path is appended to the base URL; absolute URLs cannot redirect the
    /// request to another host. Organization headers must be set explicitly.
    pub fn api_request(&self, method: Method, path: &str) -> Request {
        self.scoped_request(method, &self.baseurl, path)
    }

    /// Make an authenticated request under the configured app URL.
    pub fn app_request(&self, method: Method, path: &str) -> Request {
        self.scoped_request(method, &self.inner.app_url, path)
    }

    fn scoped_request(&self, method: Method, base: &str, path: &str) -> Request {
        let url = if path.is_empty() {
            base.to_owned()
        } else {
            format!(
                "{}/{}",
                base.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        };
        let mut request = self.client.request(method, url);
        if let Some(value) = self.inner.authorization() {
            request = request.header(reqwest::header::AUTHORIZATION, value.clone());
        }
        Request { inner: request }
    }
}

impl From<reqwest::Client> for Transport {
    fn from(client: reqwest::Client) -> Self {
        Self::new(client)
    }
}
