use braintrust_api_client::transport::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3OverflowRequest, Logs3OverflowUpload,
};
use braintrust_api_client::{Client, ClientInfo, ClientInitError, Error};
use serde_json::json;
use wiremock::matchers::{
    body_bytes, body_json, body_string_contains, header, method, path, query_param,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn discovery_selects_an_org_and_routes_generated_and_manual_operations() {
    let app = MockServer::start().await;
    let fallback = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST")).and(path("/app/api/apikey/login"))
        .and(header("authorization", "Bearer discovery-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"org_info": [
            {"id":"other", "name":"other-org", "api_url":fallback.uri()},
            {"id":"selected", "name":"selected-org", "api_url":format!("{}/data/", api.uri()), "proxy_url":"https://proxy.example", "git_metadata":{"collect":false}}
        ]}))).expect(1).mount(&app).await;
    Mock::given(method("GET"))
        .and(path("/data/v1/dataset"))
        .and(header("authorization", "Bearer discovery-key"))
        .and(header("x-bt-org-name", "selected-org"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects":[]})))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/data/version"))
        .and(header("authorization", "Bearer discovery-key"))
        .and(header("x-bt-org-name", "selected-org"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"logs3_payload_max_bytes":12345})),
        )
        .expect(1)
        .mount(&api)
        .await;
    let builder = Client::builder()
        .api_key("discovery-key")
        .app_url(format!("{}/app/", app.uri()))
        .api_url(fallback.uri())
        .org_name("selected-org");
    let client = builder.build_with_login().await.unwrap();
    assert_eq!(client.organization().unwrap().id, "selected");
    assert_eq!(
        client.organization().unwrap().extra["git_metadata"],
        json!({"collect":false})
    );
    assert_eq!(ClientInfo::baseurl(&client), format!("{}/data", api.uri()));
    assert_eq!(client.app_url(), format!("{}/app", app.uri()));
    assert!(!format!("{client:?}").contains("discovery-key"));
    client.get_dataset().send().await.unwrap();
    assert_eq!(
        client.version().await.unwrap().logs3_payload_max_bytes,
        Some(12345)
    );
    assert!(fallback.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn plain_build_and_login_do_not_retarget_the_client() {
    let app = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"org_info":[{"id":"org", "name":"org", "api_url":"https://discovered.example"}]})))
        .expect(1).mount(&app).await;
    let client = Client::builder()
        .api_key("test-key")
        .app_url(app.uri())
        .api_url(api.uri())
        .build()
        .unwrap();
    assert!(app.received_requests().await.unwrap().is_empty());
    assert!(api.received_requests().await.unwrap().is_empty());
    assert_eq!(client.login().await.unwrap().org_info.len(), 1);
    assert_eq!(ClientInfo::baseurl(&client), api.uri());
    assert!(client.organization().is_none());
}

#[tokio::test]
async fn discovery_defaults_to_first_org_and_retains_fallback_url_if_absent() {
    let app = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"org_info":[{"id":"first", "name":"first"}, {"id":"second", "name":"second"}]}),
        ))
        .mount(&app)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .app_url(app.uri())
        .api_url(api.uri())
        .build_with_login()
        .await
        .unwrap();
    assert_eq!(client.organization().unwrap().id, "first");
    assert_eq!(ClientInfo::baseurl(&client), api.uri());
}

#[tokio::test]
async fn discovery_reports_missing_orgs_and_invalid_discovered_urls() {
    for (orgs, selected) in [
        (json!([]), None),
        (json!([{"id":"org", "name":"available"}]), Some("missing")),
        (
            json!([{"id":"org", "name":"available", "api_url":"file:///tmp/not-an-api"}]),
            None,
        ),
    ] {
        let app = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/apikey/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"org_info":orgs})))
            .mount(&app)
            .await;
        let builder = Client::builder()
            .api_key("test-key")
            .app_url(app.uri())
            .api_url("https://fallback.example");
        let builder = if let Some(name) = selected {
            builder.org_name(name)
        } else {
            builder
        };
        let error = builder.build_with_login().await.unwrap_err();
        assert!(match selected {
            Some(_) => matches!(error, ClientInitError::OrganizationNotFound(_)),
            None if orgs.as_array().unwrap().is_empty() =>
                matches!(error, ClientInitError::NoOrganizations),
            None => matches!(error, ClientInitError::InvalidUrl("login API URL")),
        });
    }
}

#[tokio::test]
async fn logging_and_btql_preserve_payloads_and_transport_errors() {
    let server = MockServer::start().await;
    let payload = b"{ \"rows\": [], \"api_version\": 2 }";
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("x-bt-org-name", "my-org"))
        .and(body_bytes(payload.as_slice()))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(
            json!({"query":"select * from project_logs('id')", "fmt":"json"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data":[{"id":"row"}], "cursor":"next", "statistics":{"rows":1}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .mount(&server)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .org_name("my-org")
        .build()
        .unwrap();
    assert_eq!(
        client.logs3_raw(payload.to_vec()).await.unwrap().status(),
        204
    );
    let response = client
        .btql_json(&json!({"query":"select * from project_logs('id')"}))
        .await
        .unwrap();
    assert_eq!(response.data[0]["id"], "row");
    assert_eq!(response.cursor.as_deref(), Some("next"));
    assert_eq!(response.extra["statistics"], json!({"rows":1}));
    let Error::UnexpectedResponse(response) = client.version().await.unwrap_err() else {
        panic!("expected original HTTP response")
    };
    assert_eq!(response.status(), 429);
    assert_eq!(response.text().await.unwrap(), "slow down");
}

#[tokio::test]
async fn raw_btql_supports_non_json_results() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(body_json(
            json!({"query":{"from":"dataset('id')", "cursor":"next"}, "fmt":"jsonl"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"id\":1}\n"))
        .mount(&server)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .build()
        .unwrap();
    let response = client
        .btql(&json!({"query":{"from":"dataset('id')", "cursor":"next"}, "fmt":"jsonl"}))
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "{\"id\":1}\n");
}

#[tokio::test]
async fn control_plane_uses_app_url_and_summaries_use_api_url() {
    let app = MockServer::start().await;
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({"project_name":"example"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project"}})))
        .expect(1)
        .mount(&app)
        .await;
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .and(query_param("experiment_id", "exp"))
        .and(query_param("base_experiment_id", "base"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"scores":{}})))
        .expect(1)
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/dataset-summary"))
        .and(query_param("dataset_id", "dataset"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"dataset_id":"dataset"})))
        .expect(1)
        .mount(&api)
        .await;
    let client = Client::builder()
        .api_key("test-key")
        .api_url(api.uri())
        .app_url(app.uri())
        .build()
        .unwrap();
    let response = client
        .control_plane("project", "register", &json!({"project_name":"example"}))
        .await
        .unwrap();
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["project"]["id"],
        "project"
    );
    client
        .experiment_comparison("exp", Some("base"))
        .await
        .unwrap();
    client.dataset_summary("dataset").await.unwrap();
    assert!(matches!(
        client.control_plane("../bad", "register", &json!({})).await,
        Err(Error::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn overflow_upload_uses_separate_transport_without_api_credentials() {
    let api = MockServer::start().await;
    let storage = MockServer::start().await;
    let upload_url = format!("{}/signed?signature=storage-secret", storage.uri());
    let payload = b"{\"rows\":[{\"id\":\"row\"}],\"api_version\":2}";
    Mock::given(method("POST")).and(path("/logs3/overflow"))
        .and(header("authorization", "Bearer test-key"))
        .and(body_json(json!({"rows":[{"object_ids":{"project_id":"project"},"input_row":{"byte_size":payload.len()}}],"content_type":"application/json"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"method":"PUT", "signedUrl": upload_url, "key":"stored-key", "headers":{"x-storage-header":"required", "Content-Type":"application/octet-stream"}})))
        .expect(1).mount(&api).await;
    Mock::given(method("PUT"))
        .and(path("/signed"))
        .and(query_param("signature", "storage-secret"))
        .and(header("x-storage-header", "required"))
        .and(header("content-type", "application/octet-stream"))
        .and(header("user-agent", "storage-agent"))
        .and(body_bytes(payload.as_slice()))
        .and(|request: &Request| {
            !request.headers.contains_key("authorization")
                && !request.headers.contains_key("x-bt-org-name")
                && !request.headers.contains_key("x-api-only")
        })
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&storage)
        .await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer custom-default-key".parse().unwrap(),
    );
    headers.insert("x-api-only", "private".parse().unwrap());
    let client = Client::builder()
        .api_key("test-key")
        .org_name("org")
        .api_url(api.uri())
        .http_client_builder(reqwest::Client::builder().default_headers(headers))
        .upload_http_client_builder(reqwest::Client::builder().user_agent("storage-agent"))
        .build()
        .unwrap();
    let upload = client
        .logs3_overflow(&Logs3OverflowRequest {
            rows: vec![Logs3OverflowInputRow {
                object_ids: serde_json::from_value(json!({"project_id":"project"})).unwrap(),
                is_delete: None,
                input_row: Logs3OverflowInputRowMeta {
                    byte_size: payload.len(),
                },
            }],
            content_type: Some("application/json".into()),
        })
        .await
        .unwrap();
    client
        .upload_logs3_overflow(&upload, payload.to_vec())
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_json(
            json!({"rows":{"type":"logs3_overflow", "key":"stored-key"}, "api_version":2}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&api)
        .await;
    client
        .logs3(&json!({"rows":upload.reference(), "api_version":2}))
        .await
        .unwrap();
}

#[tokio::test]
async fn signed_multipart_upload_preserves_fields_and_boundary() {
    let storage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/upload"))
        .and(body_string_contains("name=\"key\"\r\n\r\nobject-key"))
        .and(body_string_contains("name=\"file\""))
        .and(body_string_contains("payload-data"))
        .and(header("x-storage", "required"))
        .and(|request: &Request| {
            request
                .headers
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("multipart/form-data; boundary=")
                && !request.headers.contains_key("authorization")
        })
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&storage)
        .await;
    let client = Client::builder().api_key("test-key").build().unwrap();
    let upload = Logs3OverflowUpload {
        method: "POST".into(),
        signed_url: format!("{}/upload", storage.uri()),
        key: "object-key".into(),
        fields: Some(
            [
                ("key".into(), "object-key".into()),
                ("Content-Type".into(), "application/json".into()),
            ]
            .into(),
        ),
        headers: Some(
            [
                ("Content-Type".into(), "application/json".into()),
                ("x-storage".into(), "required".into()),
            ]
            .into(),
        ),
    };
    client
        .upload_logs3_overflow(&upload, "payload-data")
        .await
        .unwrap();
    let missing_fields = Logs3OverflowUpload {
        fields: None,
        ..upload.clone()
    };
    assert!(matches!(
        client.upload_logs3_overflow(&missing_fields, "data").await,
        Err(Error::InvalidRequest(_))
    ));
    let bad_method = Logs3OverflowUpload {
        method: "DELETE".into(),
        ..upload
    };
    assert!(matches!(
        client.upload_logs3_overflow(&bad_method, "data").await,
        Err(Error::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn login_failures_preserve_status_and_body() {
    let app = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(401).set_body_string("invalid credentials"))
        .expect(1)
        .mount(&app)
        .await;
    let error = Client::builder()
        .api_key("test-key")
        .app_url(app.uri())
        .build_with_login()
        .await
        .unwrap_err();
    let ClientInitError::Login(error) = error else {
        panic!("expected login error")
    };
    let Error::UnexpectedResponse(response) = *error else {
        panic!("expected original HTTP response")
    };
    assert_eq!(response.status(), 401);
    assert_eq!(response.text().await.unwrap(), "invalid credentials");
}

#[test]
fn invalid_endpoint_and_org_configuration_is_rejected_before_requests() {
    for url in ["file:///tmp/not-an-api", "not a URL"] {
        assert!(matches!(
            Client::builder().api_key("test-key").api_url(url).build(),
            Err(ClientInitError::InvalidUrl("BRAINTRUST_API_URL"))
        ));
        assert!(matches!(
            Client::builder().api_key("test-key").app_url(url).build(),
            Err(ClientInitError::InvalidUrl("BRAINTRUST_APP_URL"))
        ));
    }
    assert!(matches!(
        Client::builder()
            .api_key("test-key")
            .org_name("bad\norg")
            .build(),
        Err(ClientInitError::InvalidOrgName)
    ));
}
