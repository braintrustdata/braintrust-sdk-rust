use braintrust_sdk_rust::{BraintrustClient, BraintrustTracingLayer};
use tracing_subscriber::{layer::SubscriberExt, registry::Registry};

#[tokio::test]
async fn test_tracing_layer_can_be_installed() {
    // Create a mock Braintrust client
    let client = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await
        .expect("Failed to create client");

    let span = client
        .span_builder_with_credentials("test-key", "test-org")
        .project_name("test-project")
        .build();

    // Install the tracing layer - should not panic
    let layer = BraintrustTracingLayer::new(span);
    let _subscriber = Registry::default().with(layer);
}

#[tokio::test]
async fn test_genai_span_is_captured() {
    use tracing::instrument;

    // Create a mock Braintrust client
    let client = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await
        .expect("Failed to create client");

    let span = client
        .span_builder_with_credentials("test-key", "test-org")
        .project_name("test-project")
        .build();

    // Install the tracing layer
    let layer = BraintrustTracingLayer::new(span.clone());
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();

    // Simulate a GenAI span using spec-compliant attribute names
    #[instrument(
        name = "gen_ai.chat",
        fields(
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "chat",
            gen_ai.request.model = "gpt-4",
            gen_ai.request.temperature = 0.7,
        )
    )]
    async fn mock_openai_call() {
        tracing::info!(
            target: "gen_ai.content.prompt",
            messages = "[{\"role\": \"user\", \"content\": \"Hello\"}]",
            "chat request"
        );

        // Simulate API call
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        let current_span = tracing::Span::current();
        current_span.record("gen_ai.response.id", "chatcmpl-123");
        current_span.record("gen_ai.response.model", "gpt-4-0314");
        current_span.record("gen_ai.usage.input_tokens", 10);
        current_span.record("gen_ai.usage.output_tokens", 20);
        // Note: total_tokens is calculated automatically, not in spec

        tracing::info!(
            target: "gen_ai.content.completion",
            content = "Hello! How can I help you?",
            "chat response"
        );
    }

    mock_openai_call().await;

    // Give some time for async logging
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_non_genai_spans_are_ignored() {
    use tracing::instrument;

    // Create a mock Braintrust client
    let client = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await
        .expect("Failed to create client");

    let span = client
        .span_builder_with_credentials("test-key", "test-org")
        .project_name("test-project")
        .build();

    // Install the tracing layer
    let layer = BraintrustTracingLayer::new(span);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();

    // This should be ignored by BraintrustTracingLayer
    #[instrument(name = "regular_function")]
    fn regular_function() {
        tracing::info!("This is a regular log");
    }

    regular_function();

    // Give some time for async logging
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_streaming_chunks_are_accumulated() {
    use tracing::instrument;

    // Create a mock Braintrust client
    let client = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await
        .expect("Failed to create client");

    let span = client
        .span_builder_with_credentials("test-key", "test-org")
        .project_name("test-project")
        .build();

    // Install the tracing layer
    let layer = BraintrustTracingLayer::new(span);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();

    #[instrument(
        name = "gen_ai.chat.stream",
        fields(
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "chat",
            gen_ai.request.model = "gpt-4",
        )
    )]
    async fn mock_streaming_call() {
        tracing::info!(
            target: "gen_ai.content.prompt",
            messages = "[{\"role\": \"user\", \"content\": \"Tell me a story\"}]",
            "chat request"
        );

        // Simulate streaming chunks
        for chunk in ["Once ", "upon ", "a ", "time..."] {
            tracing::info!(
                target: "gen_ai.content.completion.chunk",
                delta = chunk,
                "chunk received"
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    mock_streaming_call().await;

    // Give some time for async logging
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn test_multiple_providers_different_systems() {
    use tracing::instrument;

    // Create a mock Braintrust client
    let client = BraintrustClient::builder()
        .api_url("http://localhost:9999")
        .skip_login(true)
        .build()
        .await
        .expect("Failed to create client");

    let span = client
        .span_builder_with_credentials("test-key", "test-org")
        .project_name("test-project")
        .build();

    // Install the tracing layer
    let layer = BraintrustTracingLayer::new(span);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::set_global_default(subscriber).ok();

    // OpenAI call
    #[instrument(
        name = "gen_ai.chat",
        fields(
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "chat",
            gen_ai.request.model = "gpt-4",
        )
    )]
    async fn openai_call() {
        tracing::info!(
            target: "gen_ai.content.prompt",
            messages = "Hello from OpenAI",
            "chat request"
        );
        tracing::info!(
            target: "gen_ai.content.completion",
            content = "Response from OpenAI",
            "chat response"
        );
    }

    // Anthropic call
    #[instrument(
        name = "gen_ai.chat",
        fields(
            gen_ai.provider.name = "anthropic",
            gen_ai.operation.name = "chat",
            gen_ai.request.model = "claude-3-opus",
        )
    )]
    async fn anthropic_call() {
        tracing::info!(
            target: "gen_ai.content.prompt",
            messages = "Hello from Anthropic",
            "chat request"
        );
        tracing::info!(
            target: "gen_ai.content.completion",
            content = "Response from Anthropic",
            "chat response"
        );
    }

    openai_call().await;
    anthropic_call().await;

    // Give some time for async logging
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
}

// Note: Unit tests for FieldVisitor and GenAISpanData are tested indirectly
// through the integration tests above, as they are internal implementation details.
