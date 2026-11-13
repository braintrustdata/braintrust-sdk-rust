use crate::{Client, Transport};
use reqwest::Method;
use serde_json::json;
use wiremock::matchers::{body_json, body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn custom_requests_preserve_body_query_and_explicit_header_scope() {
    let api = MockServer::start().await;
    let app = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/prefix/v1/function/test-id"))
        .and(query_param("version", "test-version"))
        .and(query_param("expand", "true"))
        .and(header("authorization", "Bearer test-key"))
        .and(|r: &Request| !r.headers.contains_key("x-bt-org-name"))
        .and(body_json(json!({"extra": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok":true})))
        .expect(2)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .and(path("/app/api/dataset/get"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("x-bt-org-name", "test-org"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&app)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .api_url(format!("{}/prefix", api.uri()))
        .app_url(format!("{}/app", app.uri()))
        .org_name("test-org")
        .build()
        .unwrap();
    let request = client
        .api_request(Method::PATCH, "/v1/function/test-id?version=test-version")
        .query(&[("expand", "true")])
        .json(&json!({"extra":true}));
    request.try_clone().unwrap().send().await.unwrap();
    request.send().await.unwrap();
    client
        .app_request(Method::POST, "api/dataset/get")
        .header("x-bt-org-name", "test-org")
        .json(&json!({"id":"test-id"}))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
async fn raw_statuses_and_checked_errors_retain_retry_headers() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "17")
                .set_body_string("slow down"),
        )
        .expect(2)
        .mount(&server)
        .await;
    let transport = Transport::new(reqwest::Client::new());
    let raw = transport.get(server.uri()).send_unchecked().await.unwrap();
    assert_eq!(raw.status(), 429);
    let error = transport.get(server.uri()).send().await.unwrap_err();
    let error = error.api().unwrap();
    assert_eq!(error.status, 429);
    assert_eq!(error.headers["retry-after"], "17");
    assert_eq!(error.body, "slow down");
}

#[tokio::test]
async fn unauthenticated_transport_supports_oauth_and_signed_uploads() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(|r: &Request| {
            !r.headers.contains_key("authorization") && !r.headers.contains_key("x-bt-org-name")
        })
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"access_token":"test-token"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload"))
        .and(query_param("signature", "test-signature"))
        .and(header("content-type", "application/octet-stream"))
        .and(header("content-encoding", "gzip"))
        .and(|r: &Request| {
            !r.headers.contains_key("authorization") && !r.headers.contains_key("x-bt-org-name")
        })
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    let transport = Transport::new(reqwest::Client::new());
    transport
        .post(format!("{}/oauth/token", server.uri()))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "test-token"),
        ])
        .send()
        .await
        .unwrap();
    transport
        .put_signed_url(
            &format!("{}/upload?signature=test-signature", server.uri()),
            vec![1, 2, 3],
            &[
                ("Content-Type".into(), "application/octet-stream".into()),
                ("Content-Encoding".into(), "gzip".into()),
            ]
            .into(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn absolute_looking_paths_cannot_send_credentials_to_another_host() {
    let server = MockServer::start().await;
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .build()
        .unwrap();
    client
        .api_request(Method::GET, &format!("{}/steal", other.uri()))
        .send()
        .await
        .unwrap();
    assert!(other.received_requests().await.unwrap().is_empty());
}
