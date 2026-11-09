use std::sync::Arc;

use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    RequestBuilder, Response,
};
use serde::{de::DeserializeOwned, Serialize};
use url::Url;

use super::{Config, Error};
use crate::http::{build_http_client_builder, CaBundleConfig};

#[derive(Clone, Copy)]
pub(crate) enum Plane {
    Control,
    Data,
}

/// Immutable HTTP client shared by lightweight endpoint views.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}
struct Inner {
    http: reqwest::Client,
    api_url: Url,
    app_url: Url,
    headers: HeaderMap,
}
impl Client {
    pub fn new(config: Config) -> Result<Self, Error> {
        let api_url = base_url(&config.api_url)?;
        let app_url = base_url(&config.app_url)?;
        if config.api_key.trim().is_empty() {
            return Err(Error::Config("api_key must not be empty".into()));
        }
        let mut headers = HeaderMap::new();
        let mut token = HeaderValue::from_str(&format!("Bearer {}", config.api_key))
            .map_err(|_| Error::Config("invalid api_key header".into()))?;
        token.set_sensitive(true);
        headers.insert(AUTHORIZATION, token);
        if let Some(org) = config.org_name {
            headers.insert(
                "x-bt-org-name",
                HeaderValue::from_str(&org)
                    .map_err(|_| Error::Config("invalid organization header".into()))?,
            );
        }
        let ca = match (config.custom_ca_bundle_path, config.custom_ca_bundle_pem) {
            (Some(_), Some(_)) => {
                return Err(Error::Config(
                    "choose a CA file or PEM data, not both".into(),
                ))
            }
            (Some(path), None) => Some(CaBundleConfig::File(path)),
            (None, Some(pem)) => Some(CaBundleConfig::from_pem(pem, "API configuration")),
            (None, None) => CaBundleConfig::from_environment(),
        };
        let http =
            build_http_client_builder(config.timeout, ca.as_ref(), config.user_agent.as_deref())
                .map_err(|err| Error::Config(err.to_string()))?
                // Redirects would be additional HTTP requests and can change routing.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| Error::Config("failed to build HTTP client".into()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                api_url,
                app_url,
                headers,
            }),
        })
    }
    pub fn logs3(&self) -> super::logs3::Logs3 {
        super::logs3::Logs3 {
            client: self.clone(),
        }
    }
    pub fn btql(&self) -> super::btql::Btql {
        super::btql::Btql {
            client: self.clone(),
        }
    }
    pub fn datasets(&self) -> super::datasets::Datasets {
        super::datasets::Datasets {
            client: self.clone(),
        }
    }
    pub fn experiments(&self) -> super::experiments::Experiments {
        super::experiments::Experiments {
            client: self.clone(),
        }
    }
    pub fn projects(&self) -> super::projects::Projects {
        super::projects::Projects {
            client: self.clone(),
        }
    }
    /// One login request. Organization selection and client reconfiguration are explicit.
    pub async fn login(&self) -> Result<super::login::LoginResponse, Error> {
        let response = self
            .send(
                self.request(Plane::Control, "api/apikey/login", true)?
                    .header("content-type", "application/json"),
                "api/apikey/login",
            )
            .await?;
        decode(response, "api/apikey/login").await
    }
    fn request(
        &self,
        plane: Plane,
        path: &'static str,
        post: bool,
    ) -> Result<RequestBuilder, Error> {
        let base = if matches!(plane, Plane::Control) {
            &self.inner.app_url
        } else {
            &self.inner.api_url
        };
        let url = base
            .join(path)
            .map_err(|_| Error::Config("invalid endpoint path".into()))?;
        let request = if post {
            self.inner.http.post(url)
        } else {
            self.inner.http.get(url)
        };
        Ok(request.headers(self.inner.headers.clone()))
    }
    async fn send(
        &self,
        request: RequestBuilder,
        operation: &'static str,
    ) -> Result<Response, Error> {
        let response = request
            .send()
            .await
            .map_err(|source| transport(operation, source))?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|source| transport(operation, source))?;
        Err(Error::Http {
            operation,
            status,
            body,
        })
    }
    pub(crate) async fn get_json<T: DeserializeOwned>(
        &self,
        plane: Plane,
        path: &'static str,
        query: &[(&str, &str)],
    ) -> Result<T, Error> {
        let response = self
            .send(self.request(plane, path, false)?.query(query), path)
            .await?;
        decode(response, path).await
    }
    pub(crate) async fn post_json<Q: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        plane: Plane,
        path: &'static str,
        body: &Q,
    ) -> Result<T, Error> {
        let response = self
            .send(self.request(plane, path, true)?.json(body), path)
            .await?;
        decode(response, path).await
    }
    pub(crate) async fn post_empty<Q: Serialize + ?Sized>(
        &self,
        plane: Plane,
        path: &'static str,
        body: &Q,
    ) -> Result<(), Error> {
        self.send(self.request(plane, path, true)?.json(body), path)
            .await?;
        Ok(())
    }
    pub(crate) async fn post_bytes(
        &self,
        path: &'static str,
        payload: Vec<u8>,
    ) -> Result<(), Error> {
        self.send(
            self.request(Plane::Data, path, true)?
                .header("content-type", "application/json")
                .body(payload),
            path,
        )
        .await?;
        Ok(())
    }
}
fn base_url(value: &str) -> Result<Url, Error> {
    let mut url = Url::parse(value).map_err(|_| Error::Config("invalid endpoint URL".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config(
            "endpoint must be an HTTP(S) base URL without credentials, query or fragment".into(),
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}
fn transport(operation: &'static str, source: reqwest::Error) -> Error {
    Error::Transport {
        operation,
        source: source.without_url(),
    }
}
async fn decode<T: DeserializeOwned>(
    response: Response,
    operation: &'static str,
) -> Result<T, Error> {
    let status = response.status().as_u16();
    let body = response
        .bytes()
        .await
        .map_err(|source| transport(operation, source))?;
    serde_json::from_slice(&body).map_err(|source| Error::Decode {
        operation,
        status,
        source,
    })
}
