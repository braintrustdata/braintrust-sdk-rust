use serde_json::Value;

/// Remove a nullable union from an intersection with its identical non-null
/// union. The intersection already excludes null; this preserves its meaning
/// while avoiding openapi-to-rust's unsupported intersection of two unions.
pub fn adapt_spec(spec: &mut Value) -> Result<usize, String> {
    let version = spec["openapi"].as_str().ok_or("missing OpenAPI version")?;
    if !version.starts_with("3.0.") && !version.starts_with("3.1.") {
        return Err(format!(
            "expected OpenAPI 3.0.x or 3.1.x, received {version}"
        ));
    }
    if let Some(paths) = spec.get_mut("paths").and_then(Value::as_object_mut) {
        // Keep the existing SDK scope: browser preflights are not client calls.
        for path in paths.values_mut().filter_map(Value::as_object_mut) {
            path.remove("options");
        }
        // The generator encodes this multi-segment catch-all as one path
        // segment. Keep the concrete proxy operations instead.
        paths.remove("/v1/proxy/{path+}");
    }
    let schemas = spec
        .pointer("/components/schemas")
        .cloned()
        .unwrap_or_default();
    let mut count = 0;
    if let Some(components) = spec
        .pointer_mut("/components/schemas")
        .and_then(Value::as_object_mut)
    {
        for schema in components.values_mut() {
            adapt_schema(schema, &schemas, &mut count);
        }
    }
    adapt_locations(spec, &schemas, &mut count);
    Ok(count)
}

fn adapt_schema(schema: &mut Value, schemas: &Value, count: &mut usize) {
    if let Some(replacement) = redundant_union(schema, schemas) {
        // Retain the outer schema's constraints and annotations.
        schema["allOf"] = Value::Array(vec![replacement]);
        *count += 1;
    }
    for key in ["properties", "patternProperties", "$defs", "definitions"] {
        if let Some(properties) = schema.get_mut(key).and_then(Value::as_object_mut) {
            for property in properties.values_mut() {
                adapt_schema(property, schemas, count);
            }
        }
    }
    for key in ["items", "additionalProperties", "not"] {
        if let Some(child) = schema.get_mut(key) {
            adapt_schema(child, schemas, count);
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(children) = schema.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                adapt_schema(child, schemas, count);
            }
        }
    }
}

fn redundant_union(schema: &Value, schemas: &Value) -> Option<Value> {
    let branches = schema.get("allOf")?.as_array()?;
    if branches.len() != 2 {
        return None;
    }
    for (reference, union) in [(0, 1), (1, 0)] {
        let Some(reference) = branches[reference]
            .as_object()
            .filter(|object| object.len() == 1)
        else {
            continue;
        };
        let Some(name) = reference
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
        else {
            continue;
        };
        let name = name.replace("~1", "/").replace("~0", "~");
        let Some(target) = schemas.get(&name).and_then(Value::as_object) else {
            continue;
        };
        // Do not discard validation constraints on either the reference or its target.
        if !target.keys().all(|key| {
            matches!(
                key.as_str(),
                "anyOf"
                    | "title"
                    | "description"
                    | "example"
                    | "examples"
                    | "default"
                    | "deprecated"
            )
        }) {
            continue;
        }
        let Some(nullable) = target.get("anyOf").and_then(Value::as_array) else {
            continue;
        };
        let Some(non_null) = branches[union].get("anyOf").and_then(Value::as_array) else {
            continue;
        };
        let without_null: Vec<_> = nullable
            .iter()
            .filter(|value| **value != serde_json::json!({"type":"null"}))
            .cloned()
            .collect();
        if without_null.len() < nullable.len() && without_null == *non_null {
            return Some(branches[union].clone());
        }
    }
    None
}

fn adapt_locations(value: &mut Value, schemas: &Value, count: &mut usize) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                match key.as_str() {
                    "schema" => adapt_schema(value, schemas, count),
                    "schemas" | "example" | "examples" | "default" | "enum" => (),
                    _ => adapt_locations(value, schemas, count),
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                adapt_locations(value, schemas, count);
            }
        }
        _ => (),
    }
}
