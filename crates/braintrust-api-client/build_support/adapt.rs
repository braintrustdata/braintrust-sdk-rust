use serde_json::{Value, json};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Adaptations {
    pub null_schemas: usize,
    pub inline_titles: usize,
    pub nullable_enums: usize,
    pub preflights: usize,
    pub catch_all_routes: usize,
}

/// Adapt the public spec to Progenitor 0.15.1 without editing generated Rust.
pub fn adapt_spec(spec: &mut Value) -> Result<Adaptations, String> {
    let version = spec["openapi"].as_str().ok_or("missing OpenAPI version")?;
    if !version.starts_with("3.0.") {
        return Err(format!("expected OpenAPI 3.0.x, received {version}"));
    }
    let mut changes = Adaptations::default();
    let paths = spec["paths"]
        .as_object_mut()
        .ok_or("missing OpenAPI paths")?;
    for item in paths.values_mut() {
        // reqwest has no .options() convenience method, which Progenitor emits.
        // Preflight operations are for browsers and are not SDK operations.
        if let Some(item) = item.as_object_mut() {
            changes.preflights += usize::from(item.remove("options").is_some());
        }
    }
    // Progenitor serializes path parameters with ToString, which cannot handle
    // this route's Vec<String>. Preserve the concrete proxy endpoints instead.
    changes.catch_all_routes = usize::from(paths.remove("/v1/proxy/{path+}").is_some());

    if let Some(schemas) = spec
        .pointer_mut("/components/schemas")
        .and_then(Value::as_object_mut)
    {
        for schema in schemas.values_mut() {
            adapt_schema(schema, true, &mut changes);
        }

        // typify otherwise emits both the enum and its Option wrapper with the
        // same name. Give the non-null enum an explicit component of its own.
        let nullable: Vec<_> = schemas
            .iter()
            .filter(|(_, schema)| {
                schema["type"] == "string"
                    && schema["enum"]
                        .as_array()
                        .is_some_and(|values| values.contains(&Value::Null))
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in nullable {
            let mut value_name = format!("{name}Value");
            while schemas.contains_key(&value_name) {
                value_name.push_str("Value");
            }
            let schema = schemas.get_mut(&name).expect("component exists");
            let mut inner = schema.clone();
            inner["enum"]
                .as_array_mut()
                .expect("enum is an array")
                .retain(|value| !value.is_null());
            inner
                .as_object_mut()
                .expect("schema is an object")
                .remove("nullable");
            if inner.get("default").is_some_and(Value::is_null) {
                inner
                    .as_object_mut()
                    .expect("schema is an object")
                    .remove("default");
            }
            let mut outer = schema.as_object().expect("schema is an object").clone();
            for key in [
                "type",
                "enum",
                "nullable",
                "minLength",
                "maxLength",
                "pattern",
                "format",
            ] {
                outer.remove(key);
            }
            outer.insert(
                "anyOf".into(),
                json!([
                    { "$ref": format!("#/components/schemas/{value_name}") },
                    { "enum": [null] }
                ]),
            );
            *schema = Value::Object(outer);
            schemas.insert(value_name, inner);
            changes.nullable_enums += 1;
        }
    }
    adapt_schema_locations(spec, &mut changes);
    Ok(changes)
}

fn adapt_schema(schema: &mut Value, component: bool, changes: &mut Adaptations) {
    let Some(schema) = schema.as_object_mut() else {
        return;
    };
    // A schema property named "title" is data, not a schema annotation. Only
    // remove string annotations on inline schemas, preserving component names.
    if !component && schema.get("title").is_some_and(Value::is_string) {
        schema.remove("title");
        changes.inline_titles += 1;
    }
    if schema.get("type").and_then(Value::as_str) == Some("null") {
        schema.remove("type");
        // This is the null-only representation understood by Progenitor's
        // OpenAPI 3.0 -> JSON Schema converter, unlike `type: null`.
        schema.insert("enum".into(), json!([null]));
        changes.null_schemas += 1;
    }
    for key in ["properties", "patternProperties", "$defs", "definitions"] {
        if let Some(properties) = schema.get_mut(key).and_then(Value::as_object_mut) {
            for property in properties.values_mut() {
                adapt_schema(property, false, changes);
            }
        }
    }
    for key in ["items", "additionalProperties", "not"] {
        if let Some(child) = schema.get_mut(key) {
            adapt_schema(child, false, changes);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                adapt_schema(child, false, changes);
            }
        }
    }
}

// Locate inline schemas in parameters, request/response media types and
// headers. Reusable component schemas were handled above. Never walk payload
// examples, defaults, or enum values as if they were schema definitions.
fn adapt_schema_locations(value: &mut Value, changes: &mut Adaptations) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match key.as_str() {
                    "schema" => adapt_schema(value, false, changes),
                    "schemas" | "example" | "examples" | "default" | "enum" => (),
                    _ => adapt_schema_locations(value, changes),
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                adapt_schema_locations(value, changes);
            }
        }
        _ => (),
    }
}
