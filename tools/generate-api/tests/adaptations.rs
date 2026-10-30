#[path = "../src/adapt.rs"]
mod adapt;

use serde_json::{Value, json};

fn spec() -> Value {
    json!({
        "openapi":"3.1.1", "info":{"title":"Test", "version":"1"}, "paths":{},
        "components":{"schemas":{
            "Nullable": {"anyOf":[
                {"type":"object", "properties":{"type":{"const":"function"},"id":{"type":"string"}}, "required":["type","id"]},
                {"type":"object", "properties":{"type":{"const":"global"},"name":{"type":"string"}}, "required":["type","name"]},
                {"type":"null"}
            ]},
            "Container": {"type":"array", "items":{"allOf":[
                {"$ref":"#/components/schemas/Nullable"},
                {"anyOf":[
                    {"type":"object", "properties":{"type":{"const":"function"},"id":{"type":"string"}}, "required":["type","id"]},
                    {"type":"object", "properties":{"type":{"const":"global"},"name":{"type":"string"}}, "required":["type","name"]}
                ]}
            ]}}
        }}
    })
}

#[test]
fn preserves_intersection_semantics_and_payloads() {
    let original = spec();
    let mut adapted = original.clone();
    adapted["components"]["schemas"]["Container"]["example"] =
        json!({"allOf":[{"$ref":"unchanged"}],"type":"null"});
    let example = adapted["components"]["schemas"]["Container"]["example"].clone();
    assert_eq!(adapt::adapt_spec(&mut adapted).unwrap(), 1);
    assert_eq!(
        adapted["components"]["schemas"]["Container"]["example"],
        example
    );
    let validators: Vec<_> = [&original, &adapted]
        .into_iter()
        .map(|document| {
            jsonschema::validator_for(&json!({
                "$ref":"#/components/schemas/Container", "components":document["components"]
            }))
            .unwrap()
        })
        .collect();
    for (sample, valid) in [
        (json!([{"type":"function","id":"f"}]), true),
        (json!([{"type":"global","name":"g"}]), true),
        (json!([null]), false),
        (json!([{"type":"function"}]), false),
        (json!([{"type":"other","id":"f"}]), false),
    ] {
        for validator in &validators {
            assert_eq!(validator.is_valid(&sample), valid, "{sample}");
        }
    }
    let once = adapted.clone();
    assert_eq!(adapt::adapt_spec(&mut adapted).unwrap(), 0);
    assert_eq!(adapted, once);
}

#[test]
fn leaves_nonredundant_intersections_unchanged() {
    let mut input = spec();
    input["components"]["schemas"]["Container"]["items"]["allOf"][0]["maxProperties"] = json!(2);
    let original = input.clone();
    assert_eq!(adapt::adapt_spec(&mut input).unwrap(), 0);
    assert_eq!(input, original);
    input["openapi"] = json!("3.2.0");
    assert!(adapt::adapt_spec(&mut input).is_err());
}

#[test]
fn generates_native_31_nullable_and_exclusive_forms() {
    let mut spec = json!({
        "openapi":"3.1.1", "info":{"title":"Test", "version":"1"}, "paths":{},
        "components":{"schemas":{"Example":{
            "type":"object", "properties":{
                "name":{"type":["string","null"]},
                "count":{"type":"number","exclusiveMinimum":0},
                "choice":{"anyOf":[{"type":"string"},{"type":"null"}]}
            }
        }}}
    });
    assert_eq!(adapt::adapt_spec(&mut spec).unwrap(), 0);
    assert_eq!(spec["openapi"], "3.1.1");
    let mut analysis = openapi_to_rust::SchemaAnalyzer::new(spec)
        .unwrap()
        .analyze()
        .unwrap();
    let output = openapi_to_rust::CodeGenerator::new(openapi_to_rust::GeneratorConfig {
        enable_async_client: false,
        enable_sse_client: false,
        ..Default::default()
    })
    .generate_all(&mut analysis)
    .unwrap();
    let types = &output
        .files
        .iter()
        .find(|file| file.path.to_str() == Some("types.rs"))
        .unwrap()
        .content;
    assert!(types.contains("Option<Option<String>>"));
    assert!(types.contains("exclusiveMinimum=0"));
}
