use serde_json::Value;
use std::collections::HashMap;
use tracing::field::{Field, Visit};

/// Field visitor that extracts typed values from tracing spans and events
#[derive(Debug, Default)]
pub struct FieldVisitor {
    pub fields: HashMap<String, Value>,
}

impl FieldVisitor {
    pub fn new() -> Self {
        Self {
            fields: HashMap::new(),
        }
    }

    fn record_value(&mut self, field: &Field, value: Value) {
        self.fields.insert(field.name().to_string(), value);
    }
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Fallback for types that don't have specific record methods
        // Convert debug output to string
        let debug_str = format!("{:?}", value);
        self.record_value(field, Value::String(debug_str));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_value(field, Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_value(field, Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_value(field, Value::Number(value.into()));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        // JSON doesn't support i128, so convert to string if it overflows i64
        if let Ok(v) = i64::try_from(value) {
            self.record_value(field, Value::Number(v.into()));
        } else {
            self.record_value(field, Value::String(value.to_string()));
        }
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        // JSON doesn't support u128, so convert to string if it overflows u64
        if let Ok(v) = u64::try_from(value) {
            self.record_value(field, Value::Number(v.into()));
        } else {
            self.record_value(field, Value::String(value.to_string()));
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.record_value(field, Value::Number(n));
        } else {
            // NaN or Infinity - convert to string
            self.record_value(field, Value::String(value.to_string()));
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_value(field, Value::Bool(value));
    }
}

// Note: FieldVisitor functionality is thoroughly tested via integration tests in tests/tracing_layer_tests.rs
