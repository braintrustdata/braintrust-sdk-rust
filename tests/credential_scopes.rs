use braintrust_sdk_rust::{BraintrustClient, DatasetInsert, SpanLog};
use serde_json::json;
use wiremock::{
    matchers::{header, method, path},
    Match, Mock, MockServer, ResponseTemplate,
};

fn registration() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        json!({"project":{"id":"project","name":"p"},"dataset":{"id":"dataset","name":"d"}}),
    )
}
fn record() -> DatasetInsert {
    DatasetInsert::builder().input(json!(1)).build()
}

#[tokio::test]
async fn explicit_handles_keep_their_routes_after_shared_login_state_is_populated() {
    let configured = MockServer::start().await;
    let session = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"explicit-project"}})),
        )
        .expect(1)
        .mount(&configured)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer explicit"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&configured)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(configured.uri())
        .app_url(configured.uri())
        .build()
        .await
        .unwrap();
    let builder = client
        .span_builder_with_credentials("explicit", "org")
        .project_name("p");
    assert!(client.login_state().await.set(
        "session-secret".into(),
        "other-org".into(),
        "other".into(),
        session.uri(),
        session.uri()
    ));
    let span = builder.build();
    let write = span.log(SpanLog::builder().input(json!("explicit")).build());
    client.flush().await.unwrap();
    write.await.unwrap();
    assert!(session.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn explicit_api_url_is_preserved_for_logged_in_and_explicit_resources() {
    let control = MockServer::start().await;
    let configured = MockServer::start().await;
    let org_data = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"org_info":[{"id":"org", "name":"tenant", "api_url":org_data.uri()}]}),
        ))
        .expect(1)
        .mount(&control)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/dataset/register"))
        .respond_with(registration())
        .expect(2)
        .mount(&control)
        .await;
    for server in [&configured] {
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(204))
            .expect(2)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/btql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"record"}]})),
            )
            .expect(2)
            .mount(server)
            .await;
        Mock::given(method("GET")).and(path("/dataset-summary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project_name":"p","project_id":"project","dataset_name":"d","dataset_id":"dataset"})))
            .expect(2).mount(server).await;
    }
    let client = BraintrustClient::builder()
        .api_key("session")
        .blocking_login(true)
        .app_url(control.uri())
        .api_url(configured.uri())
        .build()
        .await
        .unwrap();
    let session = client
        .dataset_builder()
        .await
        .unwrap()
        .project_name("p")
        .dataset_name("d")
        .build();
    let explicit = client
        .dataset_builder_with_credentials("explicit", "explicit-tenant")
        .project_name("p")
        .dataset_name("d")
        .build();
    for dataset in [session.clone(), explicit] {
        dataset.insert(record()).await.unwrap();
        assert_eq!(
            dataset
                .fetch(None)
                .await
                .unwrap()
                .collect()
                .await
                .unwrap()
                .len(),
            1
        );
        dataset.summarize().await.unwrap();
    }
    assert!(org_data.received_requests().await.unwrap().is_empty());
    for request in configured.received_requests().await.unwrap() {
        let org = if header("authorization", "Bearer explicit").matches(&request) {
            "explicit-tenant"
        } else {
            assert!(header("authorization", "Bearer session").matches(&request));
            "tenant"
        };
        assert!(header("x-bt-org-name", org).matches(&request));
    }
}

#[tokio::test]
async fn organization_headers_partition_registration_capabilities_and_uploads() {
    let server = MockServer::start().await;
    for org in ["tenant-a", "tenant-b"] {
        Mock::given(method("POST"))
            .and(path("/api/project/register"))
            .and(header("x-bt-org-name", org))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":org}})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("x-bt-org-name", org))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("x-bt-org-name", org))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let builder = client
        .span_builder_with_credentials("shared-token", "shared-org")
        .project_name("same-name");
    let a = builder.clone().org_name("tenant-a").build();
    let b = builder.org_name("tenant-b").build();
    let first = a.log(SpanLog::default());
    let second = b.log(SpanLog::default());
    client.flush().await.unwrap();
    first.await.unwrap();
    second.await.unwrap();
    for request in server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/logs3")
    {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let org = rows[0]["project_id"].as_str().unwrap();
        assert!(header("x-bt-org-name", org).matches(request));
    }
}

#[tokio::test]
async fn matching_logged_in_and_explicit_scopes_share_project_and_capability_caches() {
    let control = MockServer::start().await;
    let configured = MockServer::start().await;
    let selected = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"org_info":[{"id":"org","name":"tenant","api_url":selected.uri()}]}),
        ))
        .mount(&control)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .and(header("authorization", "Bearer shared"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"shared-project"}})),
        )
        .expect(1)
        .mount(&control)
        .await;
    for server in [&configured] {
        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("authorization", "Bearer shared"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer shared"))
            .respond_with(ResponseTemplate::new(204))
            .expect(2)
            .mount(server)
            .await;
    }
    let client = BraintrustClient::builder()
        .api_key("shared")
        .blocking_login(true)
        .app_url(control.uri())
        .api_url(configured.uri())
        .build()
        .await
        .unwrap();
    // Explicit API URLs remain authoritative after login, so these scopes match.
    let logged = client
        .span_builder()
        .await
        .unwrap()
        .project_name("p")
        .build();
    let explicit = client
        .span_builder_with_credentials("shared", "org")
        .org_name("tenant")
        .project_name("p")
        .build();
    for _ in 0..2 {
        let a = logged.log(SpanLog::default());
        let b = explicit.clone().log(SpanLog::default());
        client.flush().await.unwrap();
        a.await.unwrap();
        b.await.unwrap();
    }
    for server in [&configured] {
        for request in server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/logs3")
        {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["rows"].as_array().unwrap().len(), 2);
            assert_eq!(body["rows"][0]["project_id"], "shared-project");
            assert!(header("x-bt-org-name", "tenant").matches(request));
        }
    }
    assert!(selected.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn experiment_clones_and_child_spans_preserve_the_explicit_scope() {
    let server = MockServer::start().await;
    for (route, response) in [
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"p"},"experiment":{"id":"experiment","name":"e"}}),
        ),
        (
            "/api/base_experiment/get_id",
            json!({"base_exp_id":"base","base_exp_name":"base"}),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .and(header("authorization", "Bearer explicit"))
            .and(header("x-bt-org-name", "tenant"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .and(header("x-bt-org-name", "tenant"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"scores":{},"metrics":{}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer explicit"))
        .and(header("x-bt-org-name", "tenant"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    let experiment = client
        .experiment_builder_with_credentials("explicit", "org")
        .org_name("tenant")
        .project_name("p")
        .build();
    let span = experiment.start_span().await.unwrap().build();
    span.log(SpanLog::default()).await.unwrap();
    experiment.clone().summarize().await.unwrap();
    assert!(!client.is_logged_in().await);
}

#[tokio::test]
async fn span_builder_debug_redacts_resource_credentials() {
    let client = BraintrustClient::builder()
        .skip_login(true)
        .build()
        .await
        .unwrap();
    let builder = client.span_builder_with_credentials("private-resource-token", "org");
    assert!(!format!("{builder:?}").contains("private-resource-token"));
}

#[tokio::test]
async fn project_registration_cannot_reuse_another_tokens_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .and(header("authorization", "Bearer allowed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project":{"id":"project"}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .and(header("authorization", "Bearer rejected"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer allowed"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .unwrap();
    let allowed = client
        .span_builder_with_credentials("allowed", "org")
        .project_name("p")
        .build();
    allowed.log(SpanLog::default()).await.unwrap();
    let rejected = client
        .span_builder_with_credentials("rejected", "org")
        .project_name("p")
        .build();
    let error = rejected.log(SpanLog::default()).await.unwrap_err();
    assert!(error.to_string().contains("403"));
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/logs3")
            .count(),
        1
    );
}

#[tokio::test]
async fn resource_builders_defer_credential_validation_until_use() {
    let server = MockServer::start().await;
    let client = BraintrustClient::builder()
        .skip_login(true)
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .unwrap();
    for token in ["", "invalid\nheader"] {
        let dataset = client
            .dataset_builder_with_credentials(token, "tenant")
            .project_name("project")
            .dataset_name("dataset")
            .build();
        let experiment = client
            .experiment_builder_with_credentials(token, "org")
            .project_name("project")
            .build();
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(matches!(
            dataset.dataset_id().await,
            Err(braintrust_sdk_rust::BraintrustError::InvalidConfig(_))
        ));
        assert!(matches!(
            experiment.project().await,
            Err(braintrust_sdk_rust::BraintrustError::InvalidConfig(_))
        ));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn logged_in_raw_rows_use_the_discovered_route() {
    let control = MockServer::start().await;
    let data = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"org_info":[{"id":"org","name":"tenant","api_url":data.uri()}]}),
            ),
        )
        .expect(1)
        .mount(&control)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer session"))
        .and(header("x-bt-org-name", "tenant"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&data)
        .await;
    let client = BraintrustClient::builder()
        .api_key("session")
        .app_url(control.uri())
        .blocking_login(true)
        .build()
        .await
        .unwrap();
    client
        .log_rows([json!({"id":"raw"}).as_object().unwrap().clone()])
        .await
        .unwrap();
    client.flush().await.unwrap();
}

#[tokio::test]
async fn one_token_provider_keeps_raw_row_organization_scopes_separate() {
    struct Provider;
    #[async_trait::async_trait]
    impl braintrust_sdk_rust::TokenProvider for Provider {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok("token".into())
        }
    }
    let server = MockServer::start().await;
    for org in ["tenant-a", "tenant-b"] {
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .and(header("authorization", "Bearer token"))
            .and(header("x-bt-org-name", org))
            .and(wiremock::matchers::body_json(
                json!({"rows":[{"id":org}],"api_version":2}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = BraintrustClient::builder()
        .api_url(server.uri())
        .app_url(server.uri())
        .build_without_login()
        .unwrap();
    let provider = std::sync::Arc::new(Provider);
    for org in ["tenant-a", "tenant-b"] {
        client.log_rows_with_token_provider(
            provider.clone(),
            "same-org-id",
            Some(org),
            [json!({"id":org}).as_object().unwrap().clone()],
        );
    }
    client.flush().await.unwrap();
}
