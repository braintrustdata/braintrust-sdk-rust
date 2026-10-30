use braintrust_sdk_rust::api::{self, types};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROJECT: &str = "00000000-0000-0000-0000-000000000001";
const DATASET: &str = "00000000-0000-0000-0000-000000000002";

#[tokio::test]
async fn reexported_client_sends_typed_requests_with_custom_auth_and_pagination() {
    let server = MockServer::start().await;
    let dataset =
        json!({"id": DATASET, "project_id": PROJECT, "name": "example", "url_slug": "example"});
    Mock::given(method("POST"))
        .and(path("/v1/dataset"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("user-agent", "generated-client-test"))
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
        "Bearer test-key".parse().unwrap(),
    );
    let http = reqwest::Client::builder()
        .default_headers(headers)
        .user_agent("generated-client-test")
        .build()
        .unwrap();
    let client = api::Client::new_with_client(&server.uri(), http);
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
async fn generated_client_preserves_api_error_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/dataset"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": "rate limited"})))
        .mount(&server)
        .await;
    let error = api::Client::new(&server.uri())
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
    assert_eq!(api::OPENAPI_SPEC_SHA256.len(), 64);
    assert!(!api::OPENAPI_SPEC_SOURCE.is_empty());
}
