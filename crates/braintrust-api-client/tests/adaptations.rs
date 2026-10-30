#[path = "../build_support/adapt.rs"]
mod adapt;

use adapt::adapt_spec;
use serde_json::{Value, json};

fn spec(schemas: Value) -> Value {
    json!({
        "openapi": "3.0.3", "info": {"title": "API", "version": "1"},
        "paths": {}, "components": {"schemas": schemas}
    })
}

#[test]
fn normalizes_nulls_and_inline_titles_without_changing_payloads() {
    let mut input = spec(json!({"Example": {
        "title": "Example", "type": "object",
        "properties": {
            "title": {"type": "string", "title": "InlineTitle"},
            "choice": {"anyOf": [{"type": "string"}, {"type": "null"}]},
            "nested": {"type": "array", "items": {"oneOf": [
                {"type": "object", "title": "Example", "properties": {"id": {"type": "string"}}},
                {"type": "null"}
            ]}}
        },
        "example": {"type": "null", "title": "User data"},
        "default": {"type": "null", "title": "Default data"}
    }}));
    let changes = adapt_spec(&mut input).unwrap();
    let schema = &input["components"]["schemas"]["Example"];
    assert_eq!(changes.null_schemas, 2);
    assert_eq!(changes.inline_titles, 2);
    assert_eq!(schema["title"], "Example");
    assert_eq!(schema["properties"]["title"], json!({"type": "string"}));
    assert_eq!(
        schema["properties"]["choice"]["anyOf"][1],
        json!({"enum": [null]})
    );
    assert_eq!(
        schema["example"],
        json!({"type": "null", "title": "User data"})
    );
    assert_eq!(
        schema["default"],
        json!({"type": "null", "title": "Default data"})
    );
    assert_eq!(input["info"]["title"], "API");
}

#[test]
fn splits_nullable_enums_without_clobbering_existing_components() {
    let mut input = spec(json!({
        "Kind": {"type": "string", "enum": ["task", null], "nullable": true, "default": null},
        "KindValue": {"type": "integer"}
    }));
    assert_eq!(adapt_spec(&mut input).unwrap().nullable_enums, 1);
    let schemas = &input["components"]["schemas"];
    assert_eq!(schemas["KindValue"], json!({"type": "integer"}));
    assert_eq!(schemas["KindValueValue"]["enum"], json!(["task"]));
    assert!(schemas["KindValueValue"].get("nullable").is_none());
    assert!(schemas["KindValueValue"].get("default").is_none());
    assert_eq!(schemas["Kind"]["default"], Value::Null);
    assert_eq!(
        schemas["Kind"]["anyOf"],
        json!([
            {"$ref": "#/components/schemas/KindValueValue"}, {"enum": [null]}
        ])
    );
    let adapted = input.clone();
    assert_eq!(
        adapt_spec(&mut input).unwrap(),
        adapt::Adaptations::default()
    );
    assert_eq!(input, adapted);
}

#[test]
fn excludes_preflights_and_catch_all_but_preserves_concrete_proxy_routes() {
    let mut input = spec(json!({}));
    input["paths"] = json!({
        "/v1/project/{id}": {"get": {"operationId": "getProject"}, "options": {}},
        "/v1/proxy/{path+}": {"post": {}},
        "/v1/proxy/embeddings": {"post": {"operationId": "embeddings"}}
    });
    let changes = adapt_spec(&mut input).unwrap();
    assert_eq!(changes.preflights, 1);
    assert_eq!(changes.catch_all_routes, 1);
    assert!(input["paths"].get("/v1/proxy/{path+}").is_none());
    assert!(input["paths"]["/v1/project/{id}"].get("options").is_none());
    assert_eq!(
        input["paths"]["/v1/project/{id}"]["get"]["operationId"],
        "getProject"
    );
    assert_eq!(
        input["paths"]["/v1/proxy/embeddings"]["post"]["operationId"],
        "embeddings"
    );
}

#[test]
fn adapts_parameter_and_response_schemas() {
    let mut input = spec(json!({}));
    input["paths"] = json!({"/example": {"get": {
        "parameters": [{"name": "limit", "in": "query", "schema": {"type": "null"}}],
        "responses": {"200": {"content": {"application/json": {"schema": {
            "title": "InlineResponse", "anyOf": [{"type": "string"}, {"type": "null"}]
        }, "example": {"type": "null", "title": "Unchanged"}}}}}
    }}});
    let changes = adapt_spec(&mut input).unwrap();
    assert_eq!(changes.null_schemas, 2);
    assert_eq!(changes.inline_titles, 1);
    assert_eq!(
        input["paths"]["/example"]["get"]["responses"]["200"]["content"]["application/json"]["example"],
        json!({"type": "null", "title": "Unchanged"})
    );
}

#[test]
fn rejects_unsupported_spec_versions() {
    let mut input = spec(json!({}));
    input["openapi"] = json!("3.1.1");
    assert!(
        adapt_spec(&mut input)
            .unwrap_err()
            .contains("expected OpenAPI 3.0.x")
    );
}
