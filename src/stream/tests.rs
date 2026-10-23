use super::*;
use serde_json::json;

#[test]
fn aggregates_content_from_streaming_values() {
    let chunks = vec![
        json!({
            "id": "chunk1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hello" }
            }],
            "created": 1
        }),
        json!({
            "id": "chunk2",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": { "content": " world" }
            }],
            "created": 1
        }),
        json!({
            "id": "chunk3",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": { "content": "!" },
                "finish_reason": "stop"
            }],
            "created": 1
        }),
    ];

    let mut stream = BraintrustStream::new();
    for chunk in chunks {
        stream.push(chunk);
    }

    let finalized = stream.final_value().expect("should finalize");

    // Check output is array of choices
    assert_eq!(finalized.output().len(), 1);

    let choice = &finalized.output()[0];
    assert_eq!(choice.index(), 0);
    assert_eq!(choice.message().role(), Some("assistant"));
    assert_eq!(choice.message().content(), Some("Hello world!"));
    assert_eq!(choice.finish_reason(), Some("stop"));

    // Check metadata
    assert_eq!(finalized.metadata().model(), Some("gpt-4"));
}

#[test]
fn aggregates_usage_from_final_chunk() {
    let chunks = vec![
        json!({
            "id": "chunk1",
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" },
                "finish_reason": "stop"
            }],
            "created": 1
        }),
        json!({
            "id": "chunk2",
            "model": "gpt-4",
            "choices": [],
            "created": 1,
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5
            }
        }),
    ];

    let mut stream = BraintrustStream::new();
    for chunk in chunks {
        stream.push(chunk);
    }

    let finalized = stream.final_value().expect("should finalize");

    let usage = finalized.usage().expect("should have usage");
    assert_eq!(usage.prompt_tokens(), Some(10));
    assert_eq!(usage.completion_tokens(), Some(5));
    assert_eq!(usage.total_tokens(), Some(15));
}

#[test]
fn aggregates_usage_with_input_output_token_aliases() {
    let chunks = vec![
        json!({
            "id": "chunk1",
            "model": "gpt-4.1",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Hi" },
                "finish_reason": "stop"
            }],
            "created": 1
        }),
        json!({
            "id": "chunk2",
            "model": "gpt-4.1",
            "choices": [],
            "created": 1,
            "usage": {
                "input_tokens": 12,
                "output_tokens": 7
            }
        }),
    ];

    let mut stream = BraintrustStream::new();
    for chunk in chunks {
        stream.push(chunk);
    }

    let finalized = stream.final_value().expect("should finalize");
    let usage = finalized.usage().expect("should have usage");

    assert_eq!(usage.prompt_tokens(), Some(12));
    assert_eq!(usage.completion_tokens(), Some(7));
    assert_eq!(usage.total_tokens(), Some(19));
}

#[test]
fn aggregates_tool_calls_from_streaming_values() {
    let chunks = vec![
        json!({
            "id": "chunk1",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\""
                        }
                    }]
                }
            }],
            "created": 1
        }),
        json!({
            "id": "chunk2",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "NYC\",\"unit\":\""
                        }
                    }]
                }
            }],
            "created": 1
        }),
        json!({
            "id": "chunk3",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "celsius\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "created": 1
        }),
    ];

    let mut stream = BraintrustStream::new();
    for chunk in chunks {
        stream.push(chunk);
    }

    let finalized = stream.final_value().expect("should finalize");
    let choice = &finalized.output()[0];
    let tool_calls = choice
        .message()
        .tool_calls()
        .expect("should aggregate tool calls");

    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id(), "call_abc");
    assert_eq!(tool_calls[0].call_type(), "function");
    assert_eq!(tool_calls[0].function().name(), "get_weather");
    assert_eq!(
        tool_calls[0].function().arguments(),
        "{\"location\":\"NYC\",\"unit\":\"celsius\"}"
    );
    assert_eq!(choice.finish_reason(), Some("tool_calls"));
}

#[test]
fn aggregates_detailed_usage_from_final_chunk() {
    let chunks = vec![
        json!({
            "id": "chunk1",
            "model": "o4-mini",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "content": "Reasoned answer" },
                "finish_reason": "stop"
            }],
            "created": 1
        }),
        json!({
            "id": "chunk2",
            "model": "o4-mini",
            "choices": [],
            "created": 1,
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "input_tokens_details": {
                    "audio_tokens": 3,
                    "cached_tokens": 20,
                    "cache_creation_tokens": 5
                },
                "output_tokens_details": {
                    "audio_tokens": 2,
                    "reasoning_tokens": 17
                },
                "accepted_prediction_tokens": 11,
                "rejected_prediction_tokens": 4
            }
        }),
    ];

    let mut stream = BraintrustStream::new();
    for chunk in chunks {
        stream.push(chunk);
    }

    let finalized = stream.final_value().expect("should finalize");
    let usage = finalized.usage().expect("should have usage");

    assert_eq!(usage.prompt_tokens(), Some(100));
    assert_eq!(usage.completion_tokens(), Some(50));
    assert_eq!(usage.total_tokens(), Some(150));
    assert_eq!(usage.completion_reasoning_tokens(), Some(17));
    assert_eq!(usage.reasoning_tokens(), None);

    let prompt_details = usage
        .prompt_tokens_details()
        .expect("should have prompt token details");
    assert_eq!(prompt_details.audio_tokens(), Some(3));
    assert_eq!(prompt_details.cached_tokens(), Some(20));
    assert_eq!(prompt_details.cache_creation_tokens(), Some(5));

    let completion_details = usage
        .completion_tokens_details()
        .expect("should have completion token details");
    assert_eq!(completion_details.audio_tokens(), Some(2));
    assert_eq!(completion_details.reasoning_tokens(), Some(17));
    assert_eq!(completion_details.accepted_prediction_tokens(), Some(11));
    assert_eq!(completion_details.rejected_prediction_tokens(), Some(4));
}

#[test]
fn skips_keep_alive_markers() {
    let mut stream = BraintrustStream::new();

    // Push a keep-alive marker
    stream.push(json!({"_keep_alive": true}));

    assert!(stream.is_empty());
}

#[test]
fn caches_finalized_result() {
    let chunk = json!({
        "id": "chunk1",
        "model": "gpt-4",
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "test" }
        }],
        "created": 1
    });

    let mut stream = BraintrustStream::new();
    stream.push(chunk);

    // First call computes - extract content and drop borrow
    let first_content = {
        let first = stream.final_value().expect("should finalize");
        first
            .output()
            .first()
            .and_then(|c| c.message().content().map(String::from))
    };

    // Second call returns cached
    let second_content = {
        let second = stream.final_value().expect("should finalize");
        second
            .output()
            .first()
            .and_then(|c| c.message().content().map(String::from))
    };

    assert_eq!(first_content, second_content);
    assert_eq!(first_content, Some("test".to_string()));
}

#[test]
fn snapshots_follow_new_chunks_and_preserve_sparse_tool_indices() {
    let mut stream = BraintrustStream::new();
    stream.push(json!({"choices":[{"delta":{"content":"first", "tool_calls":[{"index":7,"id":"tool-seven","type":"function","function":{"name":"lookup","arguments":"{\"query\":"}}]}}]}));
    assert_eq!(
        stream.final_value().unwrap().output()[0]
            .message()
            .content(),
        Some("first")
    );
    stream.push(json!({"choices":[{"delta":{"content":" second", "tool_calls":[{"index":7,"function":{"arguments":"\"value\"}"}}]}}]}));
    let snapshot = stream.final_value().unwrap();
    let message = snapshot.output()[0].message();
    assert_eq!(message.content(), Some("first second"));
    let calls = message.tool_calls().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id(), "tool-seven");
    assert_eq!(calls[0].function().arguments(), "{\"query\":\"value\"}");
}

#[test]
fn ignores_unknown_and_malformed_chunks_without_losing_valid_output() {
    let mut stream = BraintrustStream::new();
    for chunk in [
        json!(null),
        json!({"unknown": true}),
        json!({"choices":"invalid"}),
        json!({"choices":[{"delta":{"content":"valid"}}]}),
        json!({"usage":{"prompt_tokens":4294967295_u64, "completion_tokens":1}}),
    ] {
        stream.push(chunk);
    }
    let snapshot = stream.current_value().unwrap();
    assert_eq!(snapshot.output()[0].message().content(), Some("valid"));
    assert_eq!(snapshot.usage().unwrap().total_tokens(), None);
    assert_eq!(stream.chunk_count(), 5);
}
