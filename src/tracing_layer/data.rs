use serde_json::Value;
use std::collections::HashMap;

/// Storage for accumulated GenAI span data from OpenTelemetry semantic conventions
#[derive(Debug, Clone, Default)]
pub struct GenAISpanData {
    // System metadata (OpenTelemetry GenAI v1.29.0+)
    pub provider_name: Option<String>, // gen_ai.provider.name (was: gen_ai.system)
    pub operation_name: Option<String>, // gen_ai.operation.name (was: gen_ai.operation)
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub top_p: Option<f64>,
    pub choice_count: Option<u8>,
    pub stop_sequences: Vec<String>,
    pub seed: Option<i64>,
    pub logprobs: Option<bool>,
    pub logit_bias: Option<HashMap<String, f64>>,
    pub response_format: Option<String>,
    pub reasoning_effort: Option<String>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,

    // Request content
    pub prompt: Vec<String>,

    // Response metadata
    pub response_id: Option<String>,
    pub response_model: Option<String>,
    pub finish_reasons: Vec<String>,

    // Response content
    pub completion: Vec<String>,
    pub chunks: Vec<String>,

    // Error information
    pub error_type: Option<String>,
    pub error_code: Option<String>,

    // Server information
    pub server_address: Option<String>,
    pub server_port: Option<u16>,

    // Agent and workflow context (user-provided)
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub workflow_id: Option<String>,
    pub workflow_step: Option<u32>,

    // Embeddings-specific attributes
    pub embedding_dimensions: Option<i64>,
    pub embedding_format: Option<String>,

    // Image generation attributes
    pub image_size: Option<String>,
    pub image_quality: Option<String>,
    pub image_style: Option<String>,

    // Usage metrics (OpenTelemetry GenAI v1.29.0+)
    pub input_tokens: Option<i64>, // gen_ai.usage.input_tokens (was: prompt_tokens)
    pub output_tokens: Option<i64>, // gen_ai.usage.output_tokens (was: completion_tokens)
    pub cache_creation_input_tokens: Option<i64>, // gen_ai.usage.cache_creation.input_tokens (NEW)
    pub cache_read_input_tokens: Option<i64>, // gen_ai.usage.cache_read.input_tokens (NEW)
    pub reasoning_input_tokens: Option<i64>, // gen_ai.usage.reasoning.input_tokens (o1/o3 models)
    pub reasoning_output_tokens: Option<i64>, // gen_ai.usage.reasoning.output_tokens (o1/o3 models)

    // Additional fields captured from events
    pub additional_fields: HashMap<String, Value>,
}

impl GenAISpanData {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge a field from the visitor into the appropriate storage location
    pub fn record_field(&mut self, key: &str, value: Value) {
        match key {
            // System metadata - NEW spec-compliant names
            "gen_ai.provider.name" => {
                if let Value::String(s) = value {
                    self.provider_name = Some(s);
                }
            }
            "gen_ai.operation.name" => {
                if let Value::String(s) = value {
                    self.operation_name = Some(s);
                }
            }

            // Backwards compatibility for OLD attribute names (will be deprecated)
            "gen_ai.system" => {
                if let Value::String(s) = value {
                    self.provider_name = Some(s);
                }
            }
            "gen_ai.operation" => {
                if let Value::String(s) = value {
                    self.operation_name = Some(s);
                }
            }

            // Request attributes
            "gen_ai.request.model" => {
                if let Value::String(s) = value {
                    self.model = Some(s);
                }
            }
            "gen_ai.request.temperature" => {
                if let Value::Number(n) = value {
                    self.temperature = n.as_f64();
                }
            }
            "gen_ai.request.max_tokens" => {
                if let Value::Number(n) = value {
                    self.max_tokens = n.as_i64();
                }
            }
            "gen_ai.request.top_p" => {
                if let Value::Number(n) = value {
                    self.top_p = n.as_f64();
                }
            }
            "gen_ai.request.choice.count" => {
                if let Value::Number(n) = value {
                    self.choice_count = n.as_u64().map(|v| v as u8);
                }
            }
            "gen_ai.request.stop_sequences" => {
                if let Value::Array(arr) = value {
                    self.stop_sequences = arr
                        .into_iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                } else if let Value::String(s) = value {
                    self.stop_sequences.push(s);
                }
            }
            "gen_ai.request.seed" => {
                if let Value::Number(n) = value {
                    self.seed = n.as_i64();
                }
            }
            "gen_ai.request.logprobs" => {
                if let Value::Bool(b) = value {
                    self.logprobs = Some(b);
                }
            }
            "gen_ai.request.logit_bias" => {
                if let Value::Object(map) = value {
                    self.logit_bias = Some(
                        map.into_iter()
                            .filter_map(|(k, v)| v.as_f64().map(|n| (k, n)))
                            .collect(),
                    );
                }
            }
            "gen_ai.request.response_format" => {
                if let Value::String(s) = value {
                    self.response_format = Some(s);
                }
            }
            "gen_ai.request.reasoning_effort" => {
                if let Value::String(s) = value {
                    self.reasoning_effort = Some(s);
                }
            }
            "gen_ai.request.presence_penalty" => {
                if let Value::Number(n) = value {
                    self.presence_penalty = n.as_f64();
                }
            }
            "gen_ai.request.frequency_penalty" => {
                if let Value::Number(n) = value {
                    self.frequency_penalty = n.as_f64();
                }
            }

            // Response metadata
            "gen_ai.response.id" => {
                if let Value::String(s) = value {
                    self.response_id = Some(s);
                }
            }
            "gen_ai.response.model" => {
                if let Value::String(s) = value {
                    self.response_model = Some(s);
                }
            }
            "gen_ai.response.finish_reasons" => {
                if let Value::Array(arr) = value {
                    self.finish_reasons = arr
                        .into_iter()
                        .filter_map(|v| {
                            if let Value::String(s) = v {
                                Some(s)
                            } else {
                                None
                            }
                        })
                        .collect();
                } else if let Value::String(s) = value {
                    self.finish_reasons.push(s);
                }
            }

            // Usage metrics - NEW spec-compliant names
            "gen_ai.usage.input_tokens" => {
                if let Value::Number(n) = value {
                    self.input_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.output_tokens" => {
                if let Value::Number(n) = value {
                    self.output_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.cache_creation.input_tokens" => {
                if let Value::Number(n) = value {
                    self.cache_creation_input_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.cache_read.input_tokens" => {
                if let Value::Number(n) = value {
                    self.cache_read_input_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.reasoning.input_tokens" => {
                if let Value::Number(n) = value {
                    self.reasoning_input_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.reasoning.output_tokens" => {
                if let Value::Number(n) = value {
                    self.reasoning_output_tokens = n.as_i64();
                }
            }
            "error.type" => {
                if let Value::String(s) = value {
                    self.error_type = Some(s);
                }
            }
            "gen_ai.response.error_code" => {
                if let Value::String(s) = value {
                    self.error_code = Some(s);
                }
            }
            "server.address" => {
                if let Value::String(s) = value {
                    self.server_address = Some(s);
                }
            }
            "server.port" => {
                if let Value::Number(n) = value {
                    self.server_port = n.as_u64().map(|v| v as u16);
                }
            }
            "gen_ai.agent.id" => {
                if let Value::String(s) = value {
                    self.agent_id = Some(s);
                }
            }
            "gen_ai.agent.name" => {
                if let Value::String(s) = value {
                    self.agent_name = Some(s);
                }
            }
            "gen_ai.workflow.id" => {
                if let Value::String(s) = value {
                    self.workflow_id = Some(s);
                }
            }
            "gen_ai.workflow.step" => {
                if let Value::Number(n) = value {
                    self.workflow_step = n.as_u64().map(|v| v as u32);
                }
            }
            "gen_ai.request.embedding_dimensions" => {
                if let Value::Number(n) = value {
                    self.embedding_dimensions = n.as_i64();
                }
            }
            "gen_ai.request.embedding_format" => {
                if let Value::String(s) = value {
                    self.embedding_format = Some(s);
                }
            }
            "gen_ai.request.image_size" => {
                if let Value::String(s) = value {
                    self.image_size = Some(s);
                }
            }
            "gen_ai.request.image_quality" => {
                if let Value::String(s) = value {
                    self.image_quality = Some(s);
                }
            }
            "gen_ai.request.image_style" => {
                if let Value::String(s) = value {
                    self.image_style = Some(s);
                }
            }

            // Backwards compatibility for OLD usage attribute names (will be deprecated)
            "gen_ai.usage.prompt_tokens" => {
                if let Value::Number(n) = value {
                    self.input_tokens = n.as_i64();
                }
            }
            "gen_ai.usage.completion_tokens" => {
                if let Value::Number(n) = value {
                    self.output_tokens = n.as_i64();
                }
            }
            // NOTE: gen_ai.usage.total_tokens is NOT in the spec and is ignored

            // Store everything else in additional_fields for debugging
            _ => {
                self.additional_fields.insert(key.to_string(), value);
            }
        }
    }

    /// Add a prompt message
    pub fn add_prompt(&mut self, message: String) {
        self.prompt.push(message);
    }

    /// Add a completion message
    pub fn add_completion(&mut self, message: String) {
        self.completion.push(message);
    }

    /// Add a streaming chunk
    pub fn add_chunk(&mut self, chunk: String) {
        self.chunks.push(chunk);
    }

    /// Check if this span has any meaningful data
    pub fn is_empty(&self) -> bool {
        self.provider_name.is_none()
            && self.operation_name.is_none()
            && self.model.is_none()
            && self.prompt.is_empty()
            && self.completion.is_empty()
            && self.chunks.is_empty()
    }
}
