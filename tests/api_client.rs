use std::{error::Error as _, time::Duration};

use braintrust_sdk_rust::api::{
    self,
    btql::{BTQLQuery, BTQLQueryInner},
    datasets::DatasetRegisterRequest,
    experiments::{BaseExperimentRequest, ExperimentRegisterRequest},
    logs3::{InsertRows, Logs3OverflowUploadRequest},
    projects::ProjectRegisterRequest,
};
use serde_json::json;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

fn config(api: &MockServer, app: &MockServer, token: &str) -> api::Config {
    let mut config = api::Config::new(token);
    config.set_api_url(api.uri());
    config.set_app_url(app.uri());
    config.set_org_name(Some("isolated-org".into()));
    config.set_timeout(Duration::from_secs(3));
    config
}

#[tokio::test]
async fn explicit_scope_works_without_login_and_login_does_not_mutate_it() {
    let api = MockServer::start().await;
    let app = MockServer::start().await;
    let mut settings = config(&api, &app, "explicit-token");
    settings.set_user_agent(Some("manual-ingest/1".into()));
    let client = api::Client::new(settings).unwrap();
    assert!(api.received_requests().await.unwrap().is_empty());
    assert!(app.received_requests().await.unwrap().is_empty());

    Mock::given(method("POST"))
        .and(path("/api/apikey/login"))
        .and(header("authorization", "Bearer explicit-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"org_info":[{
                "id":"other-org", "name":"other", "api_url":"http://must-not-be-used.invalid"
            }]})),
        )
        .expect(1)
        .mount(&app)
        .await;
    assert_eq!(
        client.login().await.unwrap().org_info()[0].id(),
        "other-org"
    );
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(header("authorization", "Bearer explicit-token"))
        .and(header("x-bt-org-name", "isolated-org"))
        .and(header("user-agent", "manual-ingest/1"))
        .and(body_json(
            json!({"rows":[{"id":"row-1","project_id":"project"}],"api_version":2}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&api)
        .await;
    client
        .clone()
        .logs3()
        .insert(InsertRows::new(vec![
            json!({"id":"row-1","project_id":"project"}),
        ]))
        .await
        .unwrap();
    assert_eq!(app.received_requests().await.unwrap().len(), 1);
    assert_eq!(api.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn failures_make_one_attempt_without_retry_or_overflow() {
    for status in [400, 401, 413, 429, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/logs3"))
            .respond_with(ResponseTemplate::new(status).set_body_string("rejected"))
            .expect(1)
            .mount(&server)
            .await;
        let client = api::Client::new(config(&server, &server, "token")).unwrap();
        let error = client
            .logs3()
            .post_raw(br#"{"rows":[],"api_version":2}"#.to_vec())
            .await
            .unwrap_err();
        match error {
            api::Error::Http {
                status: actual,
                operation,
                body,
                ..
            } => {
                assert_eq!(actual, status);
                assert_eq!(operation, "logs3");
                assert_eq!(body, "rejected");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn redirects_are_returned_without_a_followup_request() {
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/logs3", target.uri()).as_str()),
        )
        .expect(1)
        .mount(&source)
        .await;
    let client = api::Client::new(config(&source, &source, "token")).unwrap();
    assert!(matches!(
        client.logs3().post_raw(vec![]).await,
        Err(api::Error::Http { status: 307, .. })
    ));
    assert!(target.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn btql_readback_preserves_cursor_and_request_errors() {
    let data = MockServer::start().await;
    let control = MockServer::start().await;
    let client = api::Client::new(config(&data, &control, "token")).unwrap();
    Mock::given(method("POST"))
        .and(path("/btql"))
        .and(body_json(
            json!({"query":{"from":"dataset('id')","limit":5,"cursor":"next"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"record","input":1}],"cursor":"last"})),
        )
        .expect(1)
        .mount(&data)
        .await;
    let page = client
        .btql()
        .query_dataset_records({
            let mut inner = BTQLQueryInner::new("dataset('id')");
            inner.set_limit(Some(5)).set_cursor(Some("next".into()));
            let mut query = BTQLQuery::new("dataset('id')");
            query.set_query(inner);
            query
        })
        .await
        .unwrap();
    assert_eq!(page.cursor(), Some("last"));
    assert_eq!(page.data().len(), 1);
    assert!(control.received_requests().await.unwrap().is_empty());

    data.reset().await;
    Mock::given(method("POST"))
        .and(path("/btql"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&data)
        .await;
    assert!(matches!(
        client
            .btql()
            .query_dataset_records(BTQLQuery::new("dataset('id')"))
            .await,
        Err(api::Error::Http { status: 500, .. })
    ));
}

#[tokio::test]
async fn malformed_success_is_a_decode_error_with_status_and_source() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(202).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;
    let client = api::Client::new(config(&server, &server, "token")).unwrap();
    let error = client.logs3().version().await.unwrap_err();
    assert!(error.source().is_some());
    assert!(matches!(
        error,
        api::Error::Decode {
            status: 202,
            operation: "version",
            ..
        }
    ));
}

#[tokio::test]
async fn selected_resource_endpoints_keep_their_wire_contracts_and_planes() {
    let data = MockServer::start().await;
    let control = MockServer::start().await;
    let client = api::Client::new(config(&data, &control, "token")).unwrap();
    for (route, response) in [
        ("/api/project/register", json!({"project":{"id":"project"}})),
        (
            "/api/dataset/register",
            json!({"project":{"id":"project","name":"p"},"dataset":{"id":"dataset","name":"d"}}),
        ),
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"p"},"experiment":{"id":"experiment","name":"e"}}),
        ),
        (
            "/api/base_experiment/get_id",
            json!({"base_exp_id":"base","base_exp_name":"baseline"}),
        ),
    ] {
        Mock::given(method("POST"))
            .and(path(route))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .expect(1)
            .mount(&control)
            .await;
    }
    assert_eq!(
        client
            .projects()
            .register({
                let mut request = ProjectRegisterRequest::new("p");
                request.set_org_id(Some("org".into()));
                request
            })
            .await
            .unwrap()
            .project()
            .id(),
        "project"
    );
    assert_eq!(
        client
            .datasets()
            .register(DatasetRegisterRequest::new("p", "org", "d"))
            .await
            .unwrap()
            .dataset()
            .id(),
        "dataset"
    );
    assert_eq!(
        client
            .experiments()
            .register({
                let mut request = ExperimentRegisterRequest::new("p", "org");
                request.set_experiment_name(Some("e".into()));
                request
            })
            .await
            .unwrap()
            .experiment()
            .id(),
        "experiment"
    );
    assert_eq!(
        client
            .experiments()
            .base(BaseExperimentRequest::new("experiment"))
            .await
            .unwrap()
            .base_exp_id(),
        Some("base")
    );
    Mock::given(method("GET"))
        .and(path("/experiment-comparison2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"scores":{},"metrics":{}})))
        .expect(1)
        .mount(&control)
        .await;
    client
        .experiments()
        .compare("experiment & name", Some("base/id"))
        .await
        .unwrap();
    let requests = control.received_requests().await.unwrap();
    let comparison = requests
        .iter()
        .find(|r| r.url.path() == "/experiment-comparison2")
        .unwrap();
    assert_eq!(
        comparison
            .url
            .query_pairs()
            .find(|(k, _)| k == "experiment_id")
            .unwrap()
            .1,
        "experiment & name"
    );
    Mock::given(method("GET")).and(path("/dataset-summary"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"project_name":"p","project_id":"project","dataset_name":"d","dataset_id":"dataset"})))
        .expect(1).mount(&data).await;
    assert_eq!(
        client
            .datasets()
            .summarize("dataset")
            .await
            .unwrap()
            .dataset_id(),
        "dataset"
    );
    assert_eq!(data.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn overflow_grant_is_explicit_and_does_not_upload_or_commit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3/overflow"))
        .and(body_json(
            json!({"content_type":"application/json","size_bytes":123,"rows":[]}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"method":"PUT","signedUrl":"https://signed.invalid/upload","key":"key"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let client = api::Client::new(config(&server, &server, "token")).unwrap();
    let grant = client
        .logs3()
        .request_overflow_upload(Logs3OverflowUploadRequest::new(
            "application/json",
            123,
            vec![],
        ))
        .await
        .unwrap();
    assert_eq!(grant.key(), "key");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
fn config_rejects_ambiguous_routes_and_redacts_credentials() {
    let mut settings = api::Config::new("do-not-print-token");
    settings.set_api_url("https://user:secret@example.test");
    assert!(!format!("{settings:?}").contains("secret"));
    assert!(!format!("{settings:?}").contains("do-not-print-token"));
    for url in [
        "file:///tmp/foo",
        "https://u:p@example.test",
        "https://example.test/?key=secret",
        "https://example.test/#fragment",
    ] {
        settings.set_api_url(url);
        assert!(matches!(
            api::Client::new(settings.clone()),
            Err(api::Error::Config(_))
        ));
    }
    assert!(api::Client::new(api::Config::new("\ninvalid-token")).is_err());
}

#[tokio::test]
async fn base_path_is_preserved_without_a_trailing_slash() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/prefix/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let mut settings = config(&server, &server, "token");
    settings.set_api_url(format!("{}/prefix", server.uri()));
    assert!(api::Client::new(settings)
        .unwrap()
        .logs3()
        .version()
        .await
        .unwrap()
        .logs3_payload_max_bytes()
        .is_none());
}
