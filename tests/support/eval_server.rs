use serde_json::json;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Standard recording endpoints; tests override individual responses at priority 1.
pub async fn mount(server: &MockServer, register_project: bool) {
    for (endpoint, response) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org","name":"org"}]}),
        ),
        ("/api/project/register", json!({"project":{"id":"project"}})),
        (
            "/api/experiment/register",
            json!({"project":{"id":"project","name":"project"},"experiment":{"id":"experiment","name":"experiment"}}),
        ),
        ("/logs3", json!({})),
    ].into_iter().filter(|(endpoint, _)| register_project || *endpoint != "/api/project/register") {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(server)
        .await;
}
