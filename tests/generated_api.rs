use braintrust_sdk_rust::api::{self, types};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT: &str = "00000000-0000-0000-0000-000000000001";
const DATASET: &str = "00000000-0000-0000-0000-000000000002";

#[tokio::test]
async fn explicit_api_key_sets_bearer_auth_without_exposing_the_key() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer programmatic-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": []})))
        .expect(1)
        .mount(&server)
        .await;
    let client = api::Client::builder()
        .api_key("programmatic-key")
        .api_url(format!("{}/", server.uri()))
        .build()
        .unwrap();
    // The builder returns the generated type, including its trait implementations.
    assert_eq!(api::ClientInfo::baseurl(&client), server.uri());
    assert!(!format!("{client:?}").contains("programmatic-key"));
    client.get_dataset().send().await.unwrap();
}

#[test]
fn empty_or_invalid_keys_are_rejected_without_exposing_values() {
    for key in ["", " \t", "private-key\ninvalid-header"] {
        let error = api::Client::builder()
            .api_key(key)
            .api_url(api::DEFAULT_API_URL)
            .build()
            .unwrap_err();
        assert!(matches!(error, api::ClientInitError::InvalidApiKey));
        assert!(!format!("{error:?}").contains("private-key"));
        assert!(!error.to_string().contains("private-key"));
    }
}

#[tokio::test]
async fn custom_transport_preserves_headers_and_uses_the_configured_api_key() {
    let server = MockServer::start().await;
    let dataset =
        json!({"id": DATASET, "project_id": PROJECT, "name": "example", "url_slug": "example"});
    Mock::given(method("POST"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("user-agent", "generated-client-test"))
        .and(header("x-custom-header", "preserved"))
        .and(body_json(json!({"name": "example", "project_id": PROJECT})))
        .respond_with(ResponseTemplate::new(200).set_body_json(dataset.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer test-key"))
        .and(query_param("limit", "10"))
        .and(query_param("starting_after", DATASET))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"objects": [dataset]})))
        .expect(1)
        .mount(&server)
        .await;
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        "Bearer replaced-key".parse().unwrap(),
    );
    headers.insert("x-custom-header", "preserved".parse().unwrap());
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("generated-client-test");
    let client = api::Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .http_client_builder(http)
        .build()
        .unwrap();
    assert!(!format!("{client:?}").contains("test-key"));
    let body: types::CreateDataset = types::CreateDataset::builder()
        .name("example")
        .project_id(PROJECT)
        .try_into()
        .unwrap();
    let created = client
        .post_dataset()
        .body(body)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(created.id.to_string(), DATASET);
    let listed = client
        .get_dataset()
        .limit(Some(10_u64))
        .starting_after(DATASET)
        .send()
        .await
        .unwrap()
        .into_inner();
    assert_eq!(listed.objects.len(), 1);
    assert_eq!(listed.objects[0].id, created.id);
}

#[tokio::test]
async fn custom_transport_timeout_is_preserved() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"objects": []}))
                .set_delay(std::time::Duration::from_secs(1)),
        )
        .mount(&server)
        .await;
    let client = api::Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .http_client_builder(
            reqwest::Client::builder().timeout(std::time::Duration::from_millis(50)),
        )
        .build()
        .unwrap();
    let error = client.get_dataset().send().await.unwrap_err();
    assert!(matches!(error, api::Error::CommunicationError(error) if error.is_timeout()));
}

#[tokio::test]
async fn generated_client_preserves_api_error_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": "rate limited"})))
        .mount(&server)
        .await;
    let error = api::Client::builder()
        .api_key("test-key")
        .api_url(server.uri())
        .build()
        .unwrap()
        .get_dataset()
        .send()
        .await
        .unwrap_err();
    match error {
        api::Error::ErrorResponse(response) => {
            assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(response.into_inner(), json!({"error": "rate limited"}));
        }
        other => panic!("expected API error response, got {other:?}"),
    }
}

#[test]
fn nullable_enums_round_trip_null_and_non_null_values() {
    for value in [json!(null), json!("scorer")] {
        let parsed: types::FunctionTypeEnum = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
    for value in [json!(null), json!("datasets")] {
        let parsed: types::ViewType = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
    assert!(serde_json::from_value::<types::FunctionTypeEnum>(json!("unknown")).is_err());
}
