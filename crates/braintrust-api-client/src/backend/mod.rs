pub mod auth;
pub mod btql;
pub mod extensions;
pub mod mcp;
pub use btql::{BtqlOptions, Pagination};

use backoff::{ExponentialBackoffBuilder, backoff::Backoff};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Client, Request, Transport};
use anyhow::{Context, Result};
use reqwest::header::CONTENT_TYPE;
use reqwest::{ClientBuilder, Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const DEFAULT_USER_AGENT: &str = concat!("braintrust-api-client/", env!("CARGO_PKG_VERSION"));

fn build_http_client(timeout: std::time::Duration) -> Result<reqwest::Client> {
    build_http_client_from_builder(reqwest::Client::builder().timeout(timeout))
}

fn build_http_client_from_builder(builder: ClientBuilder) -> Result<reqwest::Client> {
    configured_http_builder(builder)?
        .build()
        .context("failed to build HTTP client")
}

fn configured_http_builder(builder: ClientBuilder) -> Result<ClientBuilder> {
    Ok(crate::client::configure_tls(builder)?.user_agent(DEFAULT_USER_AGENT))
}

pub(crate) fn build_auth_transport(timeout: Duration) -> Result<Transport> {
    build_http_client(timeout).map(Transport::new)
}

pub(crate) fn build_auth_transport_from_builder(builder: ClientBuilder) -> Result<Transport> {
    build_http_client_from_builder(builder).map(Transport::new)
}

pub fn build_api_client(
    api_url: &str,
    app_url: &str,
    key: &str,
    timeout: Duration,
) -> Result<Client> {
    Client::builder()
        .api_key(key)
        .api_url(api_url)
        .app_url(app_url)
        .http_client_builder(configured_http_builder(
            reqwest::Client::builder().timeout(timeout),
        )?)
        .build()
        .context("failed to build Braintrust API client")
}

fn retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse()
        .ok()
        .map(Duration::from_secs)
}

pub fn api_error(error: crate::Error) -> anyhow::Error {
    match error {
        crate::Error::Api(error) => {
            let mut body = error.body;
            if let Some(cause) = error.parse_error {
                body.push_str(&format!("\nfailed to read error response body: {cause}"));
            }
            HttpError {
                status: StatusCode::from_u16(error.status).expect("HTTP response status"),
                body,
            }
            .into()
        }
        crate::Error::Transport(crate::HttpError::Network(error)) => error.into(),
        error => anyhow::anyhow!(error),
    }
}

#[derive(Clone)]
pub struct BackendClient {
    http: Client,
    base_url: String,
    api_key: String,
    org_id: String,
    org_name: String,
    options: HttpOptions,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct HttpError {
    pub status: reqwest::StatusCode,
    pub body: String,
}

impl HttpError {
    /// Create an HTTP error from a status and response body.
    pub fn new(status: StatusCode, body: String) -> Self {
        Self { status, body }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request failed ({}): {}", self.status, self.body)
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug)]
pub struct ResponseParseError {
    message: String,
}

impl std::fmt::Display for ResponseParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ResponseParseError {}

async fn parse_json_response<T: DeserializeOwned>(
    response: reqwest::Response,
    method: &str,
    path: &str,
) -> Result<T> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response
        .bytes()
        .await
        .context("failed to read response body")?;

    parse_json_body::<T>(&body, method, path, status, content_type.as_deref())
}

fn parse_json_body<T: DeserializeOwned>(
    body: &[u8],
    method: &str,
    path: &str,
    status: StatusCode,
    content_type: Option<&str>,
) -> Result<T> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let parsed = match serde_path_to_error::deserialize(&mut deserializer) {
        Ok(parsed) => parsed,
        Err(err) => {
            let json_path = err.path().to_string();
            let inner = err.into_inner();
            return Err(parse_response_error::<T>(
                method,
                path,
                status,
                content_type,
                &json_path,
                &inner,
                body.len(),
            ));
        }
    };

    if let Err(err) = deserializer.end() {
        return Err(parse_response_error::<T>(
            method,
            path,
            status,
            content_type,
            "<root>",
            &err,
            body.len(),
        ));
    }

    Ok(parsed)
}

fn parse_response_error<T>(
    method: &str,
    path: &str,
    status: StatusCode,
    content_type: Option<&str>,
    json_path: &str,
    err: &serde_json::Error,
    body_len: usize,
) -> anyhow::Error {
    let json_path = if json_path.is_empty() || json_path == "." {
        "<root>"
    } else {
        json_path
    };
    let content_type = content_type.unwrap_or("<missing>");
    let message = format!(
        "failed to parse response from {method} {path}\n  target: {}\n  JSON path: {json_path}\n  reason: {err}\n  status: {status}\n  content-type: {content_type}\n  body bytes: {body_len}",
        std::any::type_name::<T>(),
    );

    ResponseParseError { message }.into()
}

#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BtqlResponse<T = serde_json::Map<String, serde_json::Value>> {
    pub data: Vec<T>,
    #[serde(default, deserialize_with = "btql::deserialize_cursor")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub schema: Option<serde_json::Value>,
}

impl<T> BtqlResponse<T> {
    /// Create a result with no continuation cursor or schema.
    pub fn new(data: Vec<T>) -> Self {
        Self {
            data,
            cursor: None,
            schema: None,
        }
    }
}

impl BackendClient {
    pub fn new(
        api_url: &str,
        app_url: &str,
        api_key: &str,
        org_id: &str,
        org_name: &str,
        user_agent: &str,
    ) -> Result<Self> {
        let http = Client::builder()
            .api_url(api_url)
            .app_url(app_url)
            .api_key(api_key)
            .http_client_builder(
                configured_http_builder(reqwest::Client::builder().timeout(DEFAULT_HTTP_TIMEOUT))?
                    .user_agent(user_agent),
            )
            .build()?;
        Ok(Self {
            http,
            base_url: api_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            org_id: org_id.to_owned(),
            org_name: org_name.to_owned(),
            options: HttpOptions::default(),
        })
    }

    pub fn generated(&self) -> &crate::GeneratedClient {
        &self.http
    }

    /// Execute a generated operation using the configured retry and timeout policy.
    pub async fn run<T, E, F, Fut>(&self, mut operation: F) -> Result<T>
    where
        E: std::fmt::Debug,
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, crate::Error<E>>>,
    {
        let mut backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(300))
            .with_max_elapsed_time(None)
            .build();
        let attempts = if self.options.retry.unwrap_or(false) {
            MAX_ATTEMPTS
        } else {
            1
        };
        for attempt in 1..=attempts {
            let mut retry_after = None;
            let result = {
                let attempt = async {
                    let result = operation().await;
                    retry_after = result
                        .as_ref()
                        .err()
                        .and_then(|error| error.api())
                        .and_then(|error| retry_after_header(&error.headers));
                    result.map_err(generated_error)
                };
                if let Some(timeout) = self.options.timeout {
                    use futures_util::future::{Either, select};
                    match select(
                        Box::pin(attempt),
                        Box::pin(futures_timer::Delay::new(timeout)),
                    )
                    .await
                    {
                        Either::Left((result, _)) => result,
                        Either::Right(_) => Err(OperationTimeout(timeout).into()),
                    }
                } else {
                    attempt.await
                }
            };
            if let Err(error) = &result
                && attempt < attempts
                && let Some(reason) = retry_reason(error)
            {
                if let Some(observer) = &self.options.on_retry {
                    observer(reason);
                }
                futures_timer::Delay::new(retry_delay(retry_after, &mut backoff)).await;
                continue;
            }
            return result;
        }
        unreachable!("at least one operation attempt")
    }

    pub fn url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{}/{}", self.base_url, path)
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn org_id(&self) -> &str {
        &self.org_id
    }

    pub fn org_name(&self) -> &str {
        &self.org_name
    }

    /// Return a configured copy, leaving the original client unchanged.
    /// Unset scalar options inherit existing settings; headers and query params
    /// are appended. Settings apply to both HTTP and BTQL calls.
    pub(crate) fn with_http_options(&self, options: HttpOptions) -> Self {
        let mut configured = self.clone();
        configured.options.timeout = options.timeout.or(configured.options.timeout);
        configured.options.retry = options.retry.or(configured.options.retry);
        configured.options.on_retry = options.on_retry.or(configured.options.on_retry);
        configured.options.trace = options.trace.or(configured.options.trace);
        configured.options.headers.extend(options.headers);
        configured.options.query.extend(options.query);
        configured
    }

    /// Append a header on a configured copy before an HTTP or BTQL call.
    pub(crate) fn header(&self, name: &str, value: &str) -> Self {
        self.with_http_options(HttpOptions {
            headers: vec![(name.to_owned(), value.to_owned())],
            ..Default::default()
        })
    }

    fn prepare(&self, method: Method, path: &str) -> Request {
        let mut inner = self.http.api_request(method, path);
        for (name, value) in &self.options.headers {
            inner = inner.header(name, value);
        }
        if let Some(timeout) = self.options.timeout {
            inner = inner.timeout(timeout);
        }
        inner.query(&self.options.query)
    }

    async fn send_empty<T: FromResponse>(&self, method: Method, path: &str) -> Result<T> {
        let request = self.prepare(method.clone(), path);
        self.execute(method, path, request).await
    }
    pub(crate) async fn get<T: FromResponse>(&self, path: &str) -> Result<T> {
        self.send_empty(Method::GET, path).await
    }
    pub(crate) async fn post<T: FromResponse>(
        &self,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.send_body(Method::POST, path, body).await
    }
    pub(crate) async fn put<T: FromResponse>(
        &self,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.send_body(Method::PUT, path, body).await
    }
    #[cfg(test)]
    pub(crate) async fn patch<T: FromResponse>(
        &self,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        self.send_body(Method::PATCH, path, body).await
    }
    async fn send_body<T: FromResponse>(
        &self,
        method: Method,
        path: &str,
        body: &(impl Serialize + ?Sized),
    ) -> Result<T> {
        let request = self.prepare(method.clone(), path).json(body);
        self.execute(method, path, request).await
    }

    /// Delete an object, ignoring a successful response body.
    #[cfg(test)]
    pub(crate) async fn delete(&self, path: &str) -> Result<()> {
        self.delete_with_response::<NoContent>(path)
            .await
            .map(|_| ())
    }

    /// Delete an object and decode the response body.
    pub(crate) async fn delete_with_response<T: FromResponse>(&self, path: &str) -> Result<T> {
        self.send_empty(Method::DELETE, path).await
    }
}

/// Options applied to one configured copy of a backend client.
///
/// ```
/// use braintrust_api_client::backend::OperationOptions;
/// let mut options = OperationOptions::default();
/// options.retry = Some(true);
/// ```
///
/// ```compile_fail
/// use braintrust_api_client::backend::OperationOptions;
/// let _ = OperationOptions { retry: Some(true), ..Default::default() };
/// ```
#[derive(Clone, Default)]
#[non_exhaustive]
pub struct OperationOptions {
    pub timeout: Option<Duration>,
    pub retry: Option<bool>,
    pub on_retry: Option<Arc<dyn Fn(RetryReason) + Send + Sync>>,
    pub trace: Option<String>,
}
impl BackendClient {
    pub fn with_options(&self, options: OperationOptions) -> Self {
        self.with_http_options(HttpOptions {
            timeout: options.timeout,
            retry: options.retry,
            on_retry: options.on_retry,
            trace: options.trace,
            ..Default::default()
        })
    }
}

fn generated_error<E: std::fmt::Debug>(error: crate::Error<E>) -> anyhow::Error {
    match error {
        crate::Error::Api(error)
            if (200..300).contains(&error.status) && error.parse_error.is_some() =>
        {
            ResponseParseError {
                message: format!(
                    "failed to parse backend response: {}",
                    error.parse_error.unwrap()
                ),
            }
            .into()
        }
        crate::Error::Api(error) => api_error(crate::Error::Api(crate::ApiError {
            status: error.status,
            headers: error.headers,
            body: error.body,
            raw_body: error.raw_body,
            typed: None,
            parse_error: error.parse_error,
        })),
        crate::Error::Transport(error) => api_error(crate::Error::Transport(error)),
    }
}

/// Transport settings shared by HTTP and BTQL. Unset fields use client defaults.
#[derive(Clone, Default)]
pub(crate) struct HttpOptions {
    pub timeout: Option<Duration>,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    /// HTTP defaults to one attempt; BTQL defaults to retrying transient failures.
    /// Opt into retries only when an operation is safe to repeat.
    pub retry: Option<bool>,
    pub on_retry: Option<Arc<dyn Fn(RetryReason) + Send + Sync>>,
    pub trace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RetryReason {
    Status(u16),
    Network,
}

const MAX_ATTEMPTS: usize = 5;
const MAX_DELAY: Duration = Duration::from_secs(8);

#[derive(Debug, thiserror::Error)]
#[error("backend operation timed out after {0:?}")]
pub struct OperationTimeout(Duration);

impl OperationTimeout {
    /// The timeout applied to the operation.
    pub fn duration(&self) -> Duration {
        self.0
    }
}

fn retry_reason(error: &anyhow::Error) -> Option<RetryReason> {
    if error.is::<OperationTimeout>() {
        return Some(RetryReason::Network);
    }
    if let Some(error) = error.downcast_ref::<HttpError>() {
        let status = error.status;
        return (status.is_server_error()
            || status == StatusCode::PAYLOAD_TOO_LARGE
            || status == StatusCode::TOO_MANY_REQUESTS)
            .then_some(RetryReason::Status(status.as_u16()));
    }
    error.downcast_ref::<reqwest::Error>().and_then(|error| {
        (!error.is_builder()
            && (error.is_connect()
                || error.is_timeout()
                || error.is_request()
                || error.is_body()
                || error.is_decode()))
        .then_some(RetryReason::Network)
    })
}

fn retry_delay(retry_after: Option<Duration>, backoff: &mut impl Backoff) -> Duration {
    // Server rate-limit windows take precedence over the local backoff cap.
    retry_after.unwrap_or_else(|| backoff.next_backoff().unwrap_or(MAX_DELAY).min(MAX_DELAY))
}

impl BackendClient {
    async fn execute<T: FromResponse>(
        &self,
        method: Method,
        path: &str,
        request: Request,
    ) -> Result<T> {
        let started = Instant::now();
        let mut backoff = ExponentialBackoffBuilder::new()
            .with_initial_interval(Duration::from_millis(300))
            .with_multiplier(2.0)
            .with_randomization_factor(0.2)
            .with_max_interval(MAX_DELAY)
            .with_max_elapsed_time(None)
            .build();
        let attempts = if self.options.retry.unwrap_or(false) {
            MAX_ATTEMPTS
        } else {
            1
        };

        let request = request.build().context("failed to build API request")?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let request = request
                .try_clone()
                .context("request body cannot be replayed for retry")?;
            let mut retry_after = None;
            let result = async {
                let response = match request.send().await {
                    Ok(response) => response,
                    Err(error) => {
                        if let Some(api) = error.api() {
                            retry_after = retry_after_header(&api.headers);
                        }
                        return Err(api_error(error));
                    }
                };
                retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(Duration::from_secs);
                T::decode(response, method.as_str(), path).await
            }
            .await;
            if let Err(error) = &result
                && attempt < attempts
                && let Some(reason) = retry_reason(error)
            {
                if let Some(observer) = &self.options.on_retry {
                    observer(reason);
                }
                futures_timer::Delay::new(retry_delay(retry_after, &mut backoff)).await;
                continue;
            }
            if let Some(label) = &self.options.trace {
                eprintln!(
                    "{label}: {} {} {}ms {}",
                    method,
                    path,
                    started.elapsed().as_millis(),
                    if result.is_ok() { "ok" } else { "failed" }
                );
            }
            return result.map_err(|error| {
                let message = format!(
                    "{} {} failed after {attempt} attempt(s): {error}",
                    method, path
                );
                error.context(message)
            });
        }
    }
}

/// The response type selects decoding. Ordinary deserializable types use JSON.
/// Decoding stays inside the shared retry/error boundary, including body reads.
pub(crate) trait FromResponse: Sized {
    fn decode(
        response: reqwest::Response,
        method: &str,
        path: &str,
    ) -> impl Future<Output = Result<Self>> + Send;
}
impl<T: DeserializeOwned> FromResponse for T {
    async fn decode(response: reqwest::Response, method: &str, path: &str) -> Result<Self> {
        parse_json_response(response, method, path).await
    }
}

/// A successful response whose body is irrelevant (including an empty 204).
#[cfg(test)]
pub(crate) struct NoContent;
#[cfg(test)]
impl FromResponse for NoContent {
    async fn decode(_: reqwest::Response, _: &str, _: &str) -> Result<Self> {
        Ok(Self)
    }
}

/// A text response, without JSON decoding.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct Text(pub String);
#[cfg(test)]
impl FromResponse for Text {
    async fn decode(response: reqwest::Response, _: &str, _: &str) -> Result<Self> {
        response
            .text()
            .await
            .map(Self)
            .context("failed to read response body")
    }
}

/// Retain response headers alongside a decoded JSON or text body.
#[non_exhaustive]
pub struct WithHeaders<T> {
    pub headers: reqwest::header::HeaderMap,
    pub body: T,
}
impl<T: FromResponse> FromResponse for WithHeaders<T> {
    async fn decode(response: reqwest::Response, method: &str, path: &str) -> Result<Self> {
        let headers = response.headers().clone();
        let body = T::decode(response, method, path).await?;
        Ok(Self { headers, body })
    }
}

const UPLOAD_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub async fn put_signed_url(
    url: &str,
    body: Vec<u8>,
    content_encoding: Option<&str>,
) -> Result<()> {
    let mut headers = HashMap::new();
    if let Some(encoding) = content_encoding {
        headers.insert("Content-Encoding".to_string(), encoding.to_string());
    }
    put_signed_url_with_headers(url, body, &headers).await
}

pub async fn put_signed_url_with_headers(
    url: &str,
    body: Vec<u8>,
    headers: &HashMap<String, String>,
) -> Result<()> {
    let client = build_auth_transport(UPLOAD_HTTP_TIMEOUT)?;
    client
        .put_signed_url(url, body, headers)
        .await
        .map_err(api_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn truncated_error_response_preserves_status_body_and_read_failure() {
        use std::io::{Read, Write};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).unwrap() > 0);
            socket.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nupstream unavailable").unwrap();
            socket.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        let error = build_auth_transport(DEFAULT_HTTP_TIMEOUT)
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap_err();
        let error = api_error(error);
        let error = error.downcast_ref::<HttpError>().unwrap();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert!(error.body.contains("upstream unavailable"), "{error}");
        assert!(
            error.body.contains("failed to read error response body"),
            "{error}"
        );
        server.join().unwrap();
    }

    use super::*;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
    use serde::Deserialize;
    use serde_json::json;

    type RecordedUserAgent = Arc<Mutex<Option<String>>>;

    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct TestResponse {
        data: Vec<TestRow>,
    }

    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct TestRow {
        id: String,
    }

    async fn record_user_agent(
        state: web::Data<RecordedUserAgent>,
        req: HttpRequest,
    ) -> HttpResponse {
        let user_agent = req
            .headers()
            .get("user-agent")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        *state.lock().expect("user agent lock") = user_agent;
        HttpResponse::Ok().finish()
    }

    async fn captured_user_agent(client: reqwest::Client) -> Option<String> {
        let state = Arc::new(Mutex::new(None));
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock server");
        let addr = listener.local_addr().expect("mock server addr");
        let data = web::Data::new(state.clone());
        let server = HttpServer::new(move || {
            App::new()
                .app_data(data.clone())
                .route("/", web::get().to(record_user_agent))
        })
        .workers(1)
        .listen(listener)
        .expect("listen mock server")
        .run();
        let handle = server.handle();
        tokio::spawn(server);

        let response = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("send request");
        assert!(response.status().is_success());
        handle.stop(true).await;

        state.lock().expect("user agent lock").clone()
    }

    #[tokio::test]
    async fn build_http_client_sets_default_user_agent() {
        let client = build_http_client(DEFAULT_HTTP_TIMEOUT).expect("build client");
        let user_agent = captured_user_agent(client).await;

        assert_eq!(user_agent.as_deref(), Some(DEFAULT_USER_AGENT));
    }

    #[test]
    fn btql_response_deserializes_optional_cursor() {
        let response: BtqlResponse<serde_json::Value> = serde_json::from_value(json!({
            "data": [],
            "cursor": "cursor-1",
        }))
        .expect("btql response");

        assert_eq!(response.cursor.as_deref(), Some("cursor-1"));
    }

    #[test]
    fn btql_response_cursor_defaults_to_none() {
        let response: BtqlResponse<serde_json::Value> = serde_json::from_value(json!({
            "data": [],
        }))
        .expect("btql response");

        assert_eq!(response.cursor, None);
    }

    #[test]
    fn parse_json_body_reports_path_and_reason() {
        let err = parse_json_body::<TestResponse>(
            br#"{"data":[{"id":1}]}"#,
            "POST",
            "/btql",
            StatusCode::OK,
            Some("application/json"),
        )
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("failed to parse response from POST /btql"));
        assert!(message.contains("JSON path: data[0].id"));
        assert!(message.contains("invalid type: integer `1`, expected a string"));
        assert!(message.contains("status: 200 OK"));
        assert!(message.contains("content-type: application/json"));
    }

    #[test]
    fn parse_json_body_rejects_trailing_characters() {
        let err = parse_json_body::<BtqlResponse<serde_json::Value>>(
            br#"{"data":[]} trailing"#,
            "GET",
            "/btql",
            StatusCode::OK,
            Some("application/json"),
        )
        .unwrap_err();
        let message = err.to_string();

        assert!(message.contains("JSON path: <root>"));
        assert!(message.contains("trailing characters"));
    }
}

/// Shared connections for clients scoped to different credentials.
#[derive(Clone)]
pub struct BackendConnection {
    client: reqwest::Client,
}
impl BackendConnection {
    pub fn new(timeout: Duration, user_agent: &str) -> Result<Self> {
        Ok(Self {
            client: configured_http_builder(reqwest::Client::builder().timeout(timeout))?
                .user_agent(user_agent)
                .build()?,
        })
    }
    pub fn client(&self, api_url: &str, app_url: &str, key: &str) -> Result<Client> {
        Ok(Client::builder()
            .api_key(key)
            .api_url(api_url)
            .app_url(app_url)
            .http_client(self.client.clone())
            .build()?)
    }
}

/// Semantic failure categories for callers choosing an action or exit status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailureKind {
    Authentication,
    InvalidInput,
    Unavailable,
}
impl FailureKind {
    pub fn from_status(status: u16) -> Option<Self> {
        match status {
            401 | 403 => Some(Self::Authentication),
            400..=499 => Some(Self::InvalidInput),
            500..=599 => Some(Self::Unavailable),
            _ => None,
        }
    }
}
fn response_failure(error: &anyhow::Error) -> Option<&HttpError> {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<HttpError>())
}
pub fn failure_kind(error: &anyhow::Error) -> Option<FailureKind> {
    if let Some(error) = response_failure(error) {
        return FailureKind::from_status(error.status.as_u16());
    }
    if error
        .chain()
        .any(|source| source.is::<reqwest::Error>() || source.is::<OperationTimeout>())
    {
        return Some(FailureKind::Unavailable);
    }
    None
}
/// Whether the backend rejected the request with a non-success response.
pub fn is_response_rejection(error: &anyhow::Error) -> bool {
    response_failure(error).is_some()
}

pub fn is_conflict(error: &anyhow::Error) -> bool {
    response_failure(error).is_some_and(|error| error.status.as_u16() == 409)
}
pub fn error_details(error: &anyhow::Error) -> Option<serde_json::Value> {
    serde_json::from_str(&response_failure(error)?.body).ok()
}
pub fn query_lint_message(error: &anyhow::Error) -> Option<String> {
    error_details(error)?
        .get("Message")?
        .as_str()
        .map(ToOwned::to_owned)
}
pub fn is_provider_authentication_failure(error: &anyhow::Error) -> bool {
    response_failure(error).is_some_and(extensions::is_provider_auth_response)
}

/// Parse a resource definition without silently discarding unrepresented fields.
pub fn resource_definition<T: Serialize, R: DeserializeOwned + Serialize>(value: T) -> Result<R> {
    fn missing_field(
        input: &serde_json::Value,
        encoded: &serde_json::Value,
        path: &str,
    ) -> Option<String> {
        match (input, encoded) {
            (serde_json::Value::Object(input), serde_json::Value::Object(encoded)) => {
                for (key, value) in input {
                    let path = format!("{path}/{key}");
                    if let Some(encoded) = encoded.get(key) {
                        if let Some(path) = missing_field(value, encoded, &path) {
                            return Some(path);
                        }
                    } else {
                        return Some(path);
                    }
                }
            }
            (serde_json::Value::Array(input), serde_json::Value::Array(encoded)) => {
                for (index, value) in input.iter().enumerate() {
                    let path = format!("{path}/{index}");
                    let Some(encoded) = encoded.get(index) else {
                        return Some(path);
                    };
                    if let Some(path) = missing_field(value, encoded, &path) {
                        return Some(path);
                    }
                }
            }
            _ => {}
        }
        None
    }
    let input = serde_json::to_value(value)?;
    let definition: R = serde_json::from_value(input.clone())
        .map_err(|error| anyhow::anyhow!("invalid resource definition: {error}"))?;
    if let Some(path) = missing_field(&input, &serde_json::to_value(&definition)?, "") {
        anyhow::bail!("resource definition field '{path}' is not represented by the API model");
    }
    Ok(definition)
}
