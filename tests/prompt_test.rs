use braintrust_sdk_rust::{BraintrustClient, PromptBuilderError};
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
        .prompt_builder_with_credentials("token", "org-id")
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
        .prompt_builder_with_credentials("token", "org-id")
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
        .prompt_builder_with_credentials("token", "org-id")
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
        .prompt_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .slug("greeting-prompt")
        .build()
        .await
        .expect("build prompt");

    assert_eq!(prompt.id(), "prompt-id");
    assert_eq!(prompt.name(), "Greeting Prompt");
    assert_eq!(prompt.slug(), "greeting-prompt");
    assert_eq!(prompt.version(), "1000000001");

    // Build with variables
    let mut vars = std::collections::HashMap::new();
    vars.insert("company".to_string(), json!("Acme Inc"));
    vars.insert("name".to_string(), json!("Alice"));

    let result = prompt.build(Some(vars));

    assert!(result.is_chat());
    assert_eq!(result.model(), Some("gpt-4"));
    assert_eq!(result.params().temperature, Some(0.7));
    assert_eq!(result.params().max_tokens, Some(1000));

    let chat = result.chat().expect("should be chat");
    assert_eq!(chat.messages.len(), 2);

    // Check that variables were substituted
    match &chat.messages[0].content {
        braintrust_sdk_rust::MessageContent::Text(text) => {
            assert_eq!(text, "You are a helpful assistant for Acme Inc.");
        }
        _ => panic!("expected text content"),
    }

    match &chat.messages[1].content {
        braintrust_sdk_rust::MessageContent::Text(text) => {
            assert_eq!(text, "Hello, my name is Alice.");
        }
        _ => panic!("expected text content"),
    }
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
        .prompt_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .slug("complete-prompt")
        .build()
        .await
        .expect("build prompt");

    assert_eq!(prompt.slug(), "complete-prompt");

    // Build with variables
    let mut vars = std::collections::HashMap::new();
    vars.insert("prefix".to_string(), json!("The quick brown fox"));

    let result = prompt.build(Some(vars));

    assert!(result.is_completion());
    let completion = result.completion().expect("should be completion");
    assert_eq!(
        completion.prompt,
        "Complete this sentence: The quick brown fox"
    );
}

#[tokio::test]
async fn prompt_uses_defaults_with_overrides() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/v1/prompt"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "prompt-id",
            "name": "Test",
            "slug": "test",
            "_xact_id": "1",
            "project_id": "proj-id",
            "prompt_data": {
                "prompt": {
                    "type": "completion",
                    "prompt": "{{greeting}}, {{name}}!"
                },
                "template_format": "mustache"
            }
        })))
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .api_key("token")
        .app_url(server.uri())
        .api_url(server.uri())
        .build()
        .await
        .expect("client");

    // Set defaults in builder
    let prompt = client
        .prompt_builder_with_credentials("token", "org-id")
        .project_name("test-project")
        .slug("test")
        .default_var("greeting", json!("Hello"))
        .default_var("name", json!("World"))
        .build()
        .await
        .expect("build prompt");

    // Build with no vars - should use defaults
    let result = prompt.build(None);
    let completion = result.completion().expect("completion");
    assert_eq!(completion.prompt, "Hello, World!");

    // Build with override for name only
    let mut vars = std::collections::HashMap::new();
    vars.insert("name".to_string(), json!("Alice"));

    let result = prompt.build(Some(vars));
    let completion = result.completion().expect("completion");
    assert_eq!(completion.prompt, "Hello, Alice!");
}
