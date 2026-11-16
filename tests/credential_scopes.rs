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
    let write = span.log(SpanLog::builder().input(json!("explicit")).build().unwrap());
    client.flush().await.unwrap();
    write.await.unwrap();
    assert!(session.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn logged_in_resources_capture_the_org_route_while_explicit_resources_keep_configured_routes()
{
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
    for server in [&configured, &org_data] {
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/btql"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"record"}]})),
            )
            .expect(1)
            .mount(server)
            .await;
        Mock::given(method("GET")).and(path("/dataset-summary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project_name":"p","project_id":"project","dataset_name":"d","dataset_id":"dataset"})))
            .expect(1).mount(server).await;
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
    for (server, token, org) in [
        (&configured, "explicit", "explicit-tenant"),
        (&org_data, "session", "tenant"),
    ] {
        for request in server.received_requests().await.unwrap() {
            assert!(header("authorization", format!("Bearer {token}").as_str()).matches(&request));
            assert!(header("x-bt-org-name", org).matches(&request));
        }
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
async fn mixed_trace_scopes_keep_separate_routes_and_project_cache_entries() {
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
        .expect(2)
        .mount(&control)
        .await;
    for server in [&selected, &configured] {
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
    // Identical credentials and selectors still have separate data routes.
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
    for server in [&selected, &configured] {
        for request in server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/logs3")
        {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["rows"].as_array().unwrap().len(), 1);
            assert_eq!(body["rows"][0]["project_id"], "shared-project");
            assert!(header("x-bt-org-name", "tenant").matches(request));
        }
    }
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
