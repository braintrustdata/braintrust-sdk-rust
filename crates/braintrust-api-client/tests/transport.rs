use braintrust_api_client::transport::{
    Logs3OverflowInputRow, Logs3OverflowInputRowMeta, Logs3OverflowRequest, Logs3OverflowUpload,
};
use braintrust_api_client::{Client, ClientInitError, Error, HttpError};
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
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"org_info": [
            {"id":"other", "name":"other-org", "api_url":fallback.uri()},
            {"id":"selected", "name":"selected-org", "api_url":format!("{}/data/", api.uri()), "proxy_url":"https://proxy.example", "git_metadata":{"collect":false}}
        ]}))).expect(1).mount(&app).await;
    Mock::given(method("GET"))
        .and(path("/data/v1/dataset"))
        .and(header("authorization", "Bearer discovery-key"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
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
    assert_eq!(client.baseurl(), format!("{}/data", api.uri()));
    assert_eq!(client.app_url(), format!("{}/app", app.uri()));
    assert!(!format!("{client:?}").contains("discovery-key"));
    client.get_dataset_builder().send().await.unwrap();
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
    assert_eq!(client.baseurl(), api.uri());
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
    assert_eq!(client.baseurl(), api.uri());
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
    let Error::Api(response) = client.version().await.unwrap_err() else {
        panic!("expected original HTTP response")
    };
    assert_eq!(response.status, 429);
    assert_eq!(response.body, "slow down");
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
        Err(Error::Transport(HttpError::Config(_)))
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
        .and(body_json(json!({"rows":[{"object_ids":{"project_id":"project"},"input_row":{"byte_size":payload.len()}}],"content_type":"application/json", "size_bytes":payload.len()})))
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
            size_bytes: Some(payload.len()),
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
        Err(Error::Transport(HttpError::Config(_)))
    ));
    let bad_method = Logs3OverflowUpload {
        method: "DELETE".into(),
        ..upload
    };
    assert!(matches!(
        client.upload_logs3_overflow(&bad_method, "data").await,
        Err(Error::Transport(HttpError::Config(_)))
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
    let Error::Api(response) = *error else {
        panic!("expected original HTTP response")
    };
    assert_eq!(response.status, 401);
    assert_eq!(response.body, "invalid credentials");
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

#[tokio::test]
async fn org_header_is_scoped_to_logging_operations() {
    let server = MockServer::start().await;
    let client = Client::builder()
        .api_key("key")
        .api_url(server.uri())
        .app_url(server.uri())
        .org_name("org")
        .build()
        .unwrap();
    for (verb, route, response) in [
        ("POST", "/api/apikey/login", json!({"org_info":[]})),
        ("POST", "/btql", json!({"data":[]})),
        (
            "POST",
            "/api/project/register",
            json!({"project":{"id":"id"}}),
        ),
        ("GET", "/dataset-summary", json!({})),
        ("GET", "/experiment-comparison2", json!({})),
        (
            "POST",
            "/logs3/overflow",
            json!({"method":"PUT", "signedUrl":"https://storage.example/upload", "key":"key"}),
        ),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    for (verb, route) in [("GET", "/version"), ("POST", "/logs3")] {
        Mock::given(method(verb))
            .and(path(route))
            .and(header("x-bt-org-name", "org"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(if verb == "POST" { 2 } else { 1 })
            .mount(&server)
            .await;
    }
    client.login().await.unwrap();
    client.btql(&json!({"query":"select 1"})).await.unwrap();
    client
        .control_plane("project", "register", &json!({}))
        .await
        .unwrap();
    client.dataset_summary("id").await.unwrap();
    client.experiment_comparison("id", None).await.unwrap();
    let request = Logs3OverflowRequest {
        rows: vec![],
        content_type: None,
        size_bytes: None,
    };
    client.logs3_overflow(&request).await.unwrap();
    client.version().await.unwrap();
    client
        .logs3(&json!({"rows":[],"api_version":2}))
        .await
        .unwrap();
    client
        .logs3_raw("{\"rows\":[],\"api_version\":2}")
        .await
        .unwrap();
    Mock::given(method("POST"))
        .and(path("/logs3/overflow"))
        .and(header("x-bt-org-name", "explicit-org"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"method":"PUT", "signedUrl":"https://storage.example/upload", "key":"key"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    client
        .logs3_overflow_with_org_name(&request, Some("explicit-org"))
        .await
        .unwrap();
}

#[tokio::test]
async fn generated_methods_preserve_errors_and_custom_transport_without_retries() {
    let server = MockServer::start().await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("authorization", "Bearer wrong-default".parse().unwrap());
    headers.insert("x-custom", "retained".parse().unwrap());
    let client = Client::builder()
        .api_key("key")
        .api_url(server.uri())
        .org_name("org")
        .http_client_builder(reqwest::Client::builder().default_headers(headers))
        .build()
        .unwrap();
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer key"))
        .and(header("x-custom", "retained"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "10")
                .set_body_string("slow down"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let error = client.get_dataset_builder().send().await.unwrap_err();
    let error = error.api().unwrap();
    assert_eq!(error.status, 429);
    assert_eq!(error.headers["retry-after"], "10");
    assert_eq!(error.raw_body, b"slow down");
    assert!(!format!("{client:?}").contains("Bearer key"));
}

#[test]
fn generated_request_models_distinguish_omitted_null_and_value() {
    let project = "00000000-0000-0000-0000-000000000001".parse().unwrap();
    let absent = braintrust_api_client::types::CreateDataset::new("example".into(), project);
    assert!(
        serde_json::to_value(absent)
            .unwrap()
            .get("description")
            .is_none()
    );
    let null = braintrust_api_client::types::CreateDataset::builder("example".into(), project)
        .description_null()
        .build();
    assert_eq!(
        serde_json::to_value(null).unwrap()["description"],
        serde_json::Value::Null
    );
    let value = braintrust_api_client::types::CreateDataset::builder("example".into(), project)
        .description("text".into())
        .build();
    assert_eq!(serde_json::to_value(value).unwrap()["description"], "text");
}

#[test]
fn parity_overflow_requests_preserve_total_payload_size() {
    let body = json!({"rows":[], "content_type":"application/json", "size_bytes":1234});
    let request: Logs3OverflowRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), body);
}

#[tokio::test]
async fn parity_incomplete_response_bodies_preserve_status_and_headers() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for status in [400, 200] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut buffer = [0; 1024];
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            socket.write_all(format!(
                "HTTP/1.1 {status} Response\r\nContent-Length: 100\r\nContent-Type: application/json\r\nX-Request-Id: trace\r\nConnection: close\r\n\r\n{{"
            ).as_bytes()).await.unwrap();
        });
        let client = Client::builder()
            .api_key("key")
            .api_url(format!("http://{address}"))
            .build()
            .unwrap();
        let error = client.version().await.unwrap_err();
        let response = error
            .api()
            .expect("received HTTP status must survive a body read failure");
        assert_eq!(response.status, status);
        assert_eq!(response.headers["x-request-id"], "trace");
        assert!(response.parse_error.is_some());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn parity_base_url_query_and_fragment_do_not_replace_operation_paths() {
    let server = MockServer::start().await;
    for (verb, route, response) in [
        ("GET", "/version", json!({})),
        ("GET", "/v1/dataset", json!({"objects":[]})),
        ("POST", "/api/apikey/login", json!({"org_info":[]})),
    ] {
        Mock::given(method(verb))
            .and(path(route))
            .and(|request: &Request| request.url.query().is_none())
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    let base = format!("{}?ignored=1#fragment", server.uri());
    // The SDK previously used Url::join, which drops a base query and fragment.
    assert_eq!(
        reqwest::Url::parse(&base)
            .unwrap()
            .join("version")
            .unwrap()
            .path(),
        "/version"
    );
    let client = Client::builder()
        .api_key("key")
        .api_url(&base)
        .app_url(base)
        .build()
        .unwrap();
    client.version().await.unwrap();
    client.get_dataset_builder().send().await.unwrap();
    client.login().await.unwrap();
}
