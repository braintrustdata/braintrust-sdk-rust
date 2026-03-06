use braintrust_sdk_rust::{BraintrustClient, Message, PromptBuilderError, TokenBudget};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn prompt_builder_requires_slug() {
    let client = BraintrustClient::builder()
        .api_key("token")
        .api_url("https://api.braintrust.dev")
        .build()
        .await
        .expect("client");

    let result = client
        .prompt_builder_with_credentials("token")
        .project_name("test-project")
        // Missing slug
        .build()
        .await;

    assert!(matches!(result, Err(PromptBuilderError::MissingSlug)));
}

#[tokio::test]
async fn prompt_builder_requires_project() {
    let client = BraintrustClient::builder()
        .api_key("token")
        .api_url("https://api.braintrust.dev")
        .build()
        .await
        .expect("client");

    let result = client
        .prompt_builder_with_credentials("token")
        .slug("test-prompt")
        // Missing project
        .build()
        .await;

    assert!(matches!(result, Err(PromptBuilderError::MissingProject)));
}

#[tokio::test]
async fn prompt_builder_rejects_version_and_environment() {
    let client = BraintrustClient::builder()
        .api_key("token")
        .api_url("https://api.braintrust.dev")
        .build()
        .await
        .expect("client");

    let result = client
        .prompt_builder_with_credentials("token")
        .project_name("test-project")
        .slug("test-prompt")
        .version("v1")
        .environment("production") // Both version and environment
        .build()
        .await;

    assert!(matches!(
        result,
        Err(PromptBuilderError::VersionAndEnvironmentConflict)
    ));
}

#[tokio::test]
async fn prompt_fetches_and_builds_chat_prompt() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/prompt"))
        .and(query_param("slug", "greeting-prompt"))
        .and(query_param("project_name", "test-project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "prompt-id",
            "name": "Greeting Prompt",
            "slug": "greeting-prompt",
            "_xact_id": "1000000001",
            "project_id": "proj-id",
            "prompt_data": {
                "prompt": {
                    "type": "chat",
                    "messages": [
                        {
                            "role": "system",
                            "content": "You are a helpful assistant for {{company}}."
                        },
                        {
                            "role": "user",
                            "content": "Hello, my name is {{name}}."
                        }
                    ]
                },
                "options": {
                    "model": "gpt-4",
                    "params": {
                        "temperature": 0.7,
                        "max_tokens": 1000
                    }
                },
                "template_format": "mustache"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    let prompt = client
        .prompt_builder_with_credentials("token")
        .project_name("test-project")
        .slug("greeting-prompt")
        .build()
        .await
        .expect("build prompt");

    assert_eq!(prompt.id(), "prompt-id");
    assert_eq!(prompt.name(), "Greeting Prompt");
    assert_eq!(prompt.slug(), "greeting-prompt");
    assert_eq!(prompt.version(), "1000000001");

    let request = prompt.to_request().expect("to_request should succeed");

    assert_eq!(request.model, Some("gpt-4".to_string()));
    assert_eq!(request.params.temperature, Some(0.7));
    assert_eq!(
        request.params.token_budget,
        Some(TokenBudget::OutputTokens(1000))
    );
    assert_eq!(request.messages.len(), 2);

    // Check message roles (content has unsubstituted template variables)
    assert!(matches!(request.messages[0], Message::System { .. }));
    assert!(matches!(request.messages[1], Message::User { .. }));
}

#[tokio::test]
async fn prompt_fetches_completion_prompt() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/prompt"))
        .and(query_param("slug", "complete-prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "prompt-id-2",
            "name": "Completion Prompt",
            "slug": "complete-prompt",
            "_xact_id": "1000000002",
            "project_id": "proj-id",
            "prompt_data": {
                "prompt": {
                    "type": "completion",
                    "prompt": "Complete this sentence: {{prefix}}"
                },
                "options": {
                    "model": "gpt-3.5-turbo-instruct"
                },
                "template_format": "mustache"
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    let prompt = client
        .prompt_builder_with_credentials("token")
        .project_name("test-project")
        .slug("complete-prompt")
        .build()
        .await
        .expect("build prompt");

    assert_eq!(prompt.slug(), "complete-prompt");

    // Raw prompt data is available
    assert!(!prompt.data().is_null());

    // Completion prompts are not yet supported via to_request()
    assert!(prompt.to_request().is_none());
}
