use crate::{ApiError, Error, HttpError};
use reqwest::{Response, StatusCode, header::HeaderMap};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A parsed response together with its HTTP status and headers.
#[derive(Debug, Clone)]
pub struct ResponseValue<T> {
    inner: T,
    status: StatusCode,
    headers: HeaderMap,
}

impl<T> ResponseValue<T> {
    /// Consume the response and return its parsed body.
    pub fn into_inner(self) -> T {
        self.inner
    }
    /// HTTP response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }
    /// HTTP response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}

impl<T> std::ops::Deref for ResponseValue<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

// Use the same inspectable error envelope as generated operations.
#[allow(clippy::result_large_err)]
impl<T: DeserializeOwned> ResponseValue<T> {
    pub(crate) async fn from_response(response: Response) -> Result<Self, Error<Value>> {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|error| {
            Error::Api(ApiError {
                status: status.as_u16(),
                headers: headers.clone(),
                body: String::new(),
                raw_body: Vec::new(),
                typed: None,
                parse_error: Some(error.to_string()),
            })
        })?;
        let inner = serde_json::from_slice(&bytes).map_err(|error| {
            Error::Api(ApiError {
                status: status.as_u16(),
                headers: headers.clone(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
                raw_body: bytes.to_vec(),
                typed: None,
                parse_error: Some(error.to_string()),
            })
        })?;
        Ok(Self {
            inner,
            status,
            headers,
        })
    }
}

impl ResponseValue<()> {
    pub(crate) fn empty(response: Response) -> Self {
        Self {
            inner: (),
            status: response.status(),
            headers: response.headers().clone(),
        }
    }
}

impl<E: std::fmt::Debug> crate::ApiOpError<E> {
    pub(crate) fn invalid_request(message: String) -> Self {
        HttpError::Config(message).into()
    }
}

pub(crate) async fn response_error(response: Response) -> Error<Value> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let (raw_body, parse_error) = match response.bytes().await {
        Ok(bytes) => (bytes.to_vec(), None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    Error::Api(ApiError {
        status,
        headers,
        body: String::from_utf8_lossy(&raw_body).into_owned(),
        typed: serde_json::from_slice(&raw_body).ok(),
        raw_body,
        parse_error,
    })
}
