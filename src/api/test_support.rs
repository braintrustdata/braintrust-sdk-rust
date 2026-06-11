use serde_json::Value;

use crate::api::{ApiClient, LoginState};

pub(crate) fn api_client(api_url: impl Into<String>) -> ApiClient {
    let url = api_url.into();
    api_client_with_urls(url.clone(), url, "")
}

pub(crate) fn api_client_with_urls(
    api_url: impl Into<String>,
    app_url: impl Into<String>,
    org_name: impl Into<String>,
) -> ApiClient {
    let state = LoginState::new();
    assert!(state.set(
        "token".to_string(),
        "org-id".to_string(),
        org_name.into(),
        api_url.into(),
        app_url.into(),
    ));
    ApiClient::from_login_state(state).expect("api client")
}

pub(crate) fn request<'a>(
    requests: &'a [wiremock::Request],
    method: &str,
    path: &str,
) -> &'a wiremock::Request {
    requests
        .iter()
        .find(|request| request.method.as_ref() == method && request.url.path() == path)
        .unwrap_or_else(|| panic!("missing request {method} {path}"))
}

pub(crate) fn request_with_body<'a>(
    requests: &'a [wiremock::Request],
    method: &str,
    path: &str,
) -> &'a wiremock::Request {
    requests
        .iter()
        .find(|request| {
            request.method.as_ref() == method
                && request.url.path() == path
                && !request.body.is_empty()
        })
        .unwrap_or_else(|| panic!("missing request with body {method} {path}"))
}

pub(crate) fn auth_header(request: &wiremock::Request) -> &str {
    request
        .headers
        .iter()
        .find_map(|(name, values)| {
            (name.as_str() == "authorization").then_some(values.last().as_str())
        })
        .expect("authorization header")
}

pub(crate) fn assert_query_value(request: &wiremock::Request, key: &str, expected: &str) {
    assert_eq!(query_value(request, key).as_deref(), Some(expected));
}

pub(crate) fn query_value(request: &wiremock::Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find_map(|(name, value)| (name == key).then_some(value))
        .map(|value| value.into_owned())
}

pub(crate) fn query_values(request: &wiremock::Request, key: &str) -> Vec<String> {
    request
        .url
        .query_pairs()
        .filter_map(|(name, value)| (name == key).then_some(value.into_owned()))
        .collect()
}

pub(crate) fn json_body(request: &wiremock::Request) -> Value {
    serde_json::from_slice(&request.body).expect("json request body")
}
