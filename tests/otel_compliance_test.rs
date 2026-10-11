//! OpenTelemetry GenAI Semantic Conventions Compliance Tests
//!
//! These tests verify that instrumentation produces spans and events that comply
//! with the OpenTelemetry GenAI semantic conventions v1.29.0+

/// Test utilities for capturing and inspecting tracing spans
pub mod test_utils {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::registry::LookupSpan;

    /// A test layer that captures spans and events for inspection
    #[derive(Clone, Debug)]
    pub struct TestLayer {
        pub spans: Arc<Mutex<HashMap<u64, TestSpan>>>,
        pub events: Arc<Mutex<Vec<TestEvent>>>,
    }

    #[derive(Clone, Debug)]
    pub struct TestSpan {
        pub name: String,
        pub attributes: HashMap<String, String>,
        pub events: Vec<TestEvent>,
    }

    #[derive(Clone, Debug)]
    pub struct TestEvent {
        pub target: String,
        pub fields: HashMap<String, String>,
    }

    impl TestLayer {
        pub fn new() -> Self {
            Self {
                spans: Arc::new(Mutex::new(HashMap::new())),
                events: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Get all captured spans
        pub fn get_spans(&self) -> HashMap<u64, TestSpan> {
            self.spans.lock().unwrap().clone()
        }

        /// Get all captured events
        pub fn get_events(&self) -> Vec<TestEvent> {
            self.events.lock().unwrap().clone()
        }

        /// Find spans by name
        pub fn find_spans_by_name(&self, name: &str) -> Vec<TestSpan> {
            self.spans
                .lock()
                .unwrap()
                .values()
                .filter(|s| s.name == name)
                .cloned()
                .collect()
        }

        /// Find events by target
        pub fn find_events_by_target(&self, target: &str) -> Vec<TestEvent> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.target == target)
                .cloned()
                .collect()
        }
    }

    impl Default for TestLayer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<S> Layer<S> for TestLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _ctx: Context<'_, S>) {
            let mut attributes = HashMap::new();
            let mut visitor = FieldVisitor::new(&mut attributes);
            attrs.record(&mut visitor);

            let span = TestSpan {
                name: attrs.metadata().name().to_string(),
                attributes,
                events: Vec::new(),
            };

            self.spans.lock().unwrap().insert(id.into_u64(), span);
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
            if let Some(span) = self.spans.lock().unwrap().get_mut(&id.into_u64()) {
                let mut visitor = FieldVisitor::new(&mut span.attributes);
                values.record(&mut visitor);
            }
        }

        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut fields = HashMap::new();
            let mut visitor = FieldVisitor::new(&mut fields);
            event.record(&mut visitor);

            let test_event = TestEvent {
                target: event.metadata().target().to_string(),
                fields,
            };

            self.events.lock().unwrap().push(test_event);
        }
    }

    struct FieldVisitor<'a> {
        fields: &'a mut HashMap<String, String>,
    }

    impl<'a> FieldVisitor<'a> {
        fn new(fields: &'a mut HashMap<String, String>) -> Self {
            Self { fields }
        }
    }

    impl<'a> tracing::field::Visit for FieldVisitor<'a> {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{:?}", value));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_utils::TestLayer;
    use tracing_subscriber::layer::SubscriberExt;

    /// Helper to set up test subscriber with our test layer
    fn setup_test_subscriber() -> (TestLayer, tracing::subscriber::DefaultGuard) {
        let test_layer = TestLayer::new();
        let subscriber = tracing_subscriber::registry().with(test_layer.clone());
        let guard = tracing::subscriber::set_default(subscriber);
        (test_layer, guard)
    }

    #[test]
    fn test_span_has_required_otel_attributes() {
        let (test_layer, _guard) = setup_test_subscriber();

        // Simulate a GenAI span
        let span = tracing::info_span!(
            "gen_ai.chat",
            otel.kind = "client",
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "chat",
            gen_ai.request.model = "gpt-4",
        );
        let _enter = span.enter();

        // Verify required attributes
        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        assert_eq!(spans.len(), 1);

        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("otel.kind"),
            Some(&"client".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.provider.name"),
            Some(&"openai".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.operation.name"),
            Some(&"chat".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.request.model"),
            Some(&"gpt-4".to_string())
        );
    }

    #[test]
    fn test_usage_tokens_recorded() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
        );
        let _enter = span.enter();

        // Simulate recording usage
        span.record("gen_ai.usage.input_tokens", 100);
        span.record("gen_ai.usage.output_tokens", 50);

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        assert_eq!(spans.len(), 1);

        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("gen_ai.usage.input_tokens"),
            Some(&"100".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.usage.output_tokens"),
            Some(&"50".to_string())
        );
    }

    #[test]
    fn test_reasoning_tokens_captured() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            gen_ai.usage.reasoning.output_tokens = tracing::field::Empty,
        );
        let _enter = span.enter();

        span.record("gen_ai.usage.reasoning.output_tokens", 1500);

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        let captured = &spans[0];
        assert_eq!(
            captured
                .attributes
                .get("gen_ai.usage.reasoning.output_tokens"),
            Some(&"1500".to_string())
        );
    }

    #[test]
    fn test_cache_tokens_captured() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            gen_ai.usage.cache_read.input_tokens = tracing::field::Empty,
            gen_ai.usage.cache_creation.input_tokens = tracing::field::Empty,
        );
        let _enter = span.enter();

        span.record("gen_ai.usage.cache_read.input_tokens", 200);
        span.record("gen_ai.usage.cache_creation.input_tokens", 300);

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        let captured = &spans[0];
        assert_eq!(
            captured
                .attributes
                .get("gen_ai.usage.cache_read.input_tokens"),
            Some(&"200".to_string())
        );
        assert_eq!(
            captured
                .attributes
                .get("gen_ai.usage.cache_creation.input_tokens"),
            Some(&"300".to_string())
        );
    }

    #[test]
    fn test_error_attributes_captured() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            error.type = tracing::field::Empty,
            gen_ai.response.error_code = tracing::field::Empty,
        );
        let _enter = span.enter();

        span.record("error.type", "rate_limit");
        span.record("gen_ai.response.error_code", "429");

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("error.type"),
            Some(&"rate_limit".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.response.error_code"),
            Some(&"429".to_string())
        );
    }

    #[test]
    fn test_server_attributes_captured() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            server.address = tracing::field::Empty,
            server.port = tracing::field::Empty,
        );
        let _enter = span.enter();

        span.record("server.address", "api.openai.com");
        span.record("server.port", 443u16);

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("server.address"),
            Some(&"api.openai.com".to_string())
        );
        assert_eq!(
            captured.attributes.get("server.port"),
            Some(&"443".to_string())
        );
    }

    #[test]
    fn test_embeddings_operation() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.embeddings",
            otel.kind = "client",
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "embeddings",
            gen_ai.request.model = "text-embedding-3-small",
            gen_ai.request.embedding_dimensions = 1536,
        );
        let _enter = span.enter();

        let spans = test_layer.find_spans_by_name("gen_ai.embeddings");
        assert_eq!(spans.len(), 1);

        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("gen_ai.operation.name"),
            Some(&"embeddings".to_string())
        );
        assert_eq!(
            captured
                .attributes
                .get("gen_ai.request.embedding_dimensions"),
            Some(&"1536".to_string())
        );
    }

    #[test]
    fn test_image_generation_operation() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.image_generation",
            otel.kind = "client",
            gen_ai.provider.name = "openai",
            gen_ai.operation.name = "image_generation",
            gen_ai.request.image_size = "1024x1024",
            gen_ai.request.image_quality = "hd",
        );
        let _enter = span.enter();

        let spans = test_layer.find_spans_by_name("gen_ai.image_generation");
        let captured = &spans[0];
        assert_eq!(
            captured.attributes.get("gen_ai.operation.name"),
            Some(&"image_generation".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.request.image_size"),
            Some(&"1024x1024".to_string())
        );
    }

    #[test]
    fn test_tool_call_event() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!("gen_ai.chat");
        let _enter = span.enter();

        tracing::info!(
            target: "gen_ai.tool.call",
            call_id = "call_123",
            name = "get_weather",
            arguments = r#"{"location":"SF"}"#,
            "tool call"
        );

        let events = test_layer.find_events_by_target("gen_ai.tool.call");
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(event.fields.get("call_id"), Some(&"call_123".to_string()));
        assert_eq!(event.fields.get("name"), Some(&"get_weather".to_string()));
    }

    #[test]
    fn test_tool_result_event() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!("gen_ai.chat");
        let _enter = span.enter();

        tracing::info!(
            target: "gen_ai.tool.result",
            tool_call_id = "call_123",
            result = "72°F and sunny",
            "tool result"
        );

        let events = test_layer.find_events_by_target("gen_ai.tool.result");
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(
            event.fields.get("tool_call_id"),
            Some(&"call_123".to_string())
        );
        assert_eq!(
            event.fields.get("result"),
            Some(&"72°F and sunny".to_string())
        );
    }

    #[test]
    fn test_prompt_and_completion_events() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!("gen_ai.chat");
        let _enter = span.enter();

        tracing::info!(
            target: "gen_ai.content.prompt",
            content = "What is 2+2?",
            "prompt"
        );

        tracing::info!(
            target: "gen_ai.content.completion",
            content = "2+2 equals 4",
            "completion"
        );

        let prompt_events = test_layer.find_events_by_target("gen_ai.content.prompt");
        assert_eq!(prompt_events.len(), 1);

        let completion_events = test_layer.find_events_by_target("gen_ai.content.completion");
        assert_eq!(completion_events.len(), 1);
    }

    #[test]
    fn test_all_request_parameters() {
        let (test_layer, _guard) = setup_test_subscriber();

        let span = tracing::info_span!(
            "gen_ai.chat",
            gen_ai.request.temperature = 0.7,
            gen_ai.request.max_tokens = 1000,
            gen_ai.request.top_p = 0.9,
            gen_ai.request.frequency_penalty = 0.5,
            gen_ai.request.presence_penalty = 0.5,
            gen_ai.request.seed = 12345,
        );
        let _enter = span.enter();

        let spans = test_layer.find_spans_by_name("gen_ai.chat");
        let captured = &spans[0];

        assert_eq!(
            captured.attributes.get("gen_ai.request.temperature"),
            Some(&"0.7".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.request.max_tokens"),
            Some(&"1000".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.request.top_p"),
            Some(&"0.9".to_string())
        );
        assert_eq!(
            captured.attributes.get("gen_ai.request.seed"),
            Some(&"12345".to_string())
        );
    }
}
