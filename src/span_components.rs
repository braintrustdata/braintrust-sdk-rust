use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

use crate::error::{BraintrustError, Result};
use crate::types::{ParentSpanInfo, SpanObjectType};

const ENCODING_VERSION_V3: u8 = 3;
const ENCODING_VERSION_V4: u8 = 4;

/// Field IDs for binary encoding
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // All variants intentionally end with "Id"
enum FieldId {
    ObjectId = 1,
    RowId = 2,
    SpanId = 3,
    RootSpanId = 4,
}

impl FieldId {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(FieldId::ObjectId),
            2 => Some(FieldId::RowId),
            3 => Some(FieldId::SpanId),
            4 => Some(FieldId::RootSpanId),
            _ => None,
        }
    }

    fn field_name(&self) -> &'static str {
        match self {
            FieldId::ObjectId => "object_id",
            FieldId::RowId => "row_id",
            FieldId::SpanId => "span_id",
            FieldId::RootSpanId => "root_span_id",
        }
    }
}

/// SpanComponents represents the serialized form of parent span information
/// that can be passed in HTTP headers or exported/imported between SDKs.
///
/// This supports both V3 and V4 encoding formats for compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanComponents {
    /// The type of object this span belongs to
    pub object_type: SpanObjectType,

    /// Object ID (experiment_id, project_id, or prompt_session_id depending on object_type)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,

    /// Metadata arguments for computing object metadata
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compute_object_metadata_args: Option<Map<String, Value>>,

    /// Row ID (for multi-tenant logging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_id: Option<String>,

    /// Span ID (8-byte hex string, 16 characters)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,

    /// Root span ID / Trace ID (16-byte hex string, 32 characters)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_span_id: Option<String>,

    /// Direct parent span IDs for this span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_parents: Option<Vec<String>>,

    /// Event data to propagate to child spans (e.g., prompt versions, metadata)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub propagated_event: Option<Map<String, Value>>,
}

impl SpanComponents {
    /// Create a new SpanComponents with required fields
    pub fn new(object_type: SpanObjectType) -> Self {
        Self {
            object_type,
            object_id: None,
            compute_object_metadata_args: None,
            row_id: None,
            span_id: None,
            root_span_id: None,
            span_parents: None,
            propagated_event: None,
        }
    }

    /// Parse SpanComponents from a base64-encoded string
    /// Supports V3 and V4 encoding formats
    pub fn parse(s: &str) -> Result<Self> {
        let bytes = BASE64
            .decode(s)
            .map_err(|e| BraintrustError::InvalidConfig(format!("Invalid base64: {}", e)))?;

        if bytes.is_empty() {
            return Err(BraintrustError::InvalidConfig(
                "Empty SpanComponents string".to_string(),
            ));
        }

        let version = bytes[0];

        match version {
            ENCODING_VERSION_V3 => Self::parse_v3(&bytes),
            ENCODING_VERSION_V4 => Self::parse_v4(&bytes),
            v if v < ENCODING_VERSION_V3 => {
                // V1/V2 - delegate to V3 parser for now
                Self::parse_v3(&bytes)
            }
            _ => Err(BraintrustError::InvalidConfig(format!(
                "Unsupported SpanComponents encoding version: {}. This SDK supports versions up to {}",
                version, ENCODING_VERSION_V4
            ))),
        }
    }

    /// Serialize to a V4-encoded base64 string
    pub fn to_str(&self) -> String {
        let mut buffers: Vec<Vec<u8>> = Vec::new();

        // Version and object type
        buffers.push(vec![ENCODING_VERSION_V4, self.object_type as u8]);

        // Collect hex-encodable fields
        let mut hex_entries: Vec<Vec<u8>> = Vec::new();
        let mut json_obj = Map::new();

        // Try to encode object_id as hex (UUID)
        if let Some(ref object_id) = self.object_id {
            if let Some(hex_bytes) = try_parse_uuid(object_id) {
                let mut entry = vec![FieldId::ObjectId as u8];
                entry.extend_from_slice(&hex_bytes);
                hex_entries.push(entry);
            } else {
                json_obj.insert("object_id".to_string(), Value::String(object_id.clone()));
            }
        }

        // Try to encode row_id as hex (UUID)
        if let Some(ref row_id) = self.row_id {
            if let Some(hex_bytes) = try_parse_uuid(row_id) {
                let mut entry = vec![FieldId::RowId as u8];
                entry.extend_from_slice(&hex_bytes);
                hex_entries.push(entry);
            } else {
                json_obj.insert("row_id".to_string(), Value::String(row_id.clone()));
            }
        }

        // Try to encode span_id as hex (8-byte, 16 hex chars)
        if let Some(ref span_id) = self.span_id {
            if let Some(hex_bytes) = try_parse_hex_span_id(span_id) {
                let mut entry = vec![FieldId::SpanId as u8];
                entry.extend_from_slice(&hex_bytes);
                hex_entries.push(entry);
            } else {
                json_obj.insert("span_id".to_string(), Value::String(span_id.clone()));
            }
        }

        // Try to encode root_span_id as hex (16-byte, 32 hex chars)
        if let Some(ref root_span_id) = self.root_span_id {
            if let Some(hex_bytes) = try_parse_hex_trace_id(root_span_id) {
                let mut entry = vec![FieldId::RootSpanId as u8];
                entry.extend_from_slice(&hex_bytes);
                hex_entries.push(entry);
            } else {
                json_obj.insert(
                    "root_span_id".to_string(),
                    Value::String(root_span_id.clone()),
                );
            }
        }

        // Number of hex entries
        buffers.push(vec![hex_entries.len() as u8]);

        // Append hex entries
        for entry in hex_entries {
            buffers.push(entry);
        }

        // Add JSON remainder if needed
        if let Some(ref args) = self.compute_object_metadata_args {
            json_obj.insert(
                "compute_object_metadata_args".to_string(),
                Value::Object(args.clone()),
            );
        }
        if let Some(ref event) = self.propagated_event {
            json_obj.insert("propagated_event".to_string(), Value::Object(event.clone()));
        }
        if let Some(ref span_parents) = self.span_parents {
            json_obj.insert(
                "span_parents".to_string(),
                Value::Array(
                    span_parents
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
            );
        }

        if !json_obj.is_empty() {
            let json_str = serde_json::to_string(&json_obj).unwrap();
            buffers.push(json_str.into_bytes());
        }

        // Concatenate all buffers
        let total_len: usize = buffers.iter().map(|b| b.len()).sum();
        let mut result = Vec::with_capacity(total_len);
        for buffer in buffers {
            result.extend(buffer);
        }

        BASE64.encode(&result)
    }

    /// Parse V3 format
    fn parse_v3(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 2 {
            return Err(BraintrustError::InvalidConfig(
                "Invalid V3 SpanComponents: too short".to_string(),
            ));
        }

        let object_type = SpanObjectType::from_u8(bytes[1]).ok_or_else(|| {
            BraintrustError::InvalidConfig(format!("Invalid object_type: {}", bytes[1]))
        })?;

        // V3 format: version + object_type + num_uuid_fields + uuid_entries + json_remainder
        let num_uuid_fields = bytes[2];
        let mut offset = 3;
        let mut json_obj = Map::new();

        json_obj.insert("object_type".to_string(), Value::Number(bytes[1].into()));

        // Parse UUID fields (each is 1 byte field_id + 16 bytes UUID)
        for _ in 0..num_uuid_fields {
            if offset + 17 > bytes.len() {
                return Err(BraintrustError::InvalidConfig(
                    "Invalid V3 SpanComponents: truncated UUID field".to_string(),
                ));
            }

            let field_id = FieldId::from_u8(bytes[offset]).ok_or_else(|| {
                BraintrustError::InvalidConfig(format!("Invalid field_id: {}", bytes[offset]))
            })?;

            let uuid_bytes = &bytes[offset + 1..offset + 17];
            let uuid_str = format_uuid(uuid_bytes);
            json_obj.insert(field_id.field_name().to_string(), Value::String(uuid_str));

            offset += 17;
        }

        // Parse JSON remainder
        if offset < bytes.len() {
            let json_str = std::str::from_utf8(&bytes[offset..]).map_err(|e| {
                BraintrustError::InvalidConfig(format!("Invalid UTF-8 in JSON: {}", e))
            })?;
            let remainder: Map<String, Value> = serde_json::from_str(json_str)
                .map_err(|e| BraintrustError::InvalidConfig(format!("Invalid JSON: {}", e)))?;
            json_obj.extend(remainder);
        }

        // Convert json_obj to SpanComponents
        Self::from_json_obj(json_obj, object_type)
    }

    /// Parse V4 format
    fn parse_v4(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 3 {
            return Err(BraintrustError::InvalidConfig(
                "Invalid V4 SpanComponents: too short".to_string(),
            ));
        }

        let object_type = SpanObjectType::from_u8(bytes[1]).ok_or_else(|| {
            BraintrustError::InvalidConfig(format!("Invalid object_type: {}", bytes[1]))
        })?;

        let num_hex_entries = bytes[2];
        let mut offset = 3;
        let mut json_obj = Map::new();

        json_obj.insert("object_type".to_string(), Value::Number(bytes[1].into()));

        // Parse hex entries
        for _ in 0..num_hex_entries {
            if offset >= bytes.len() {
                return Err(BraintrustError::InvalidConfig(
                    "Invalid V4 SpanComponents: truncated hex entry".to_string(),
                ));
            }

            let field_id = FieldId::from_u8(bytes[offset]).ok_or_else(|| {
                BraintrustError::InvalidConfig(format!("Invalid field_id: {}", bytes[offset]))
            })?;
            offset += 1;

            let (hex_str, bytes_consumed) = match field_id {
                FieldId::SpanId => {
                    // 8-byte span ID
                    if offset + 8 > bytes.len() {
                        return Err(BraintrustError::InvalidConfig(
                            "Invalid V4 SpanComponents: truncated span_id".to_string(),
                        ));
                    }
                    let hex_bytes = &bytes[offset..offset + 8];
                    (format_hex(hex_bytes), 8)
                }
                FieldId::RootSpanId => {
                    // 16-byte trace ID
                    if offset + 16 > bytes.len() {
                        return Err(BraintrustError::InvalidConfig(
                            "Invalid V4 SpanComponents: truncated root_span_id".to_string(),
                        ));
                    }
                    let hex_bytes = &bytes[offset..offset + 16];
                    (format_hex(hex_bytes), 16)
                }
                _ => {
                    // UUID fields (16 bytes)
                    if offset + 16 > bytes.len() {
                        return Err(BraintrustError::InvalidConfig(
                            "Invalid V4 SpanComponents: truncated UUID field".to_string(),
                        ));
                    }
                    let uuid_bytes = &bytes[offset..offset + 16];
                    (format_uuid(uuid_bytes), 16)
                }
            };

            json_obj.insert(field_id.field_name().to_string(), Value::String(hex_str));
            offset += bytes_consumed;
        }

        // Parse JSON remainder
        if offset < bytes.len() {
            let json_str = std::str::from_utf8(&bytes[offset..]).map_err(|e| {
                BraintrustError::InvalidConfig(format!("Invalid UTF-8 in JSON: {}", e))
            })?;
            let remainder: Map<String, Value> = serde_json::from_str(json_str)
                .map_err(|e| BraintrustError::InvalidConfig(format!("Invalid JSON: {}", e)))?;
            json_obj.extend(remainder);
        }

        Self::from_json_obj(json_obj, object_type)
    }

    /// Convert a JSON object to SpanComponents
    fn from_json_obj(
        mut json_obj: Map<String, Value>,
        object_type: SpanObjectType,
    ) -> Result<Self> {
        Ok(Self {
            object_type,
            object_id: json_obj
                .remove("object_id")
                .and_then(|v| v.as_str().map(String::from)),
            compute_object_metadata_args: json_obj
                .remove("compute_object_metadata_args")
                .and_then(|v| v.as_object().cloned()),
            row_id: json_obj
                .remove("row_id")
                .and_then(|v| v.as_str().map(String::from)),
            span_id: json_obj
                .remove("span_id")
                .and_then(|v| v.as_str().map(String::from)),
            root_span_id: json_obj
                .remove("root_span_id")
                .and_then(|v| v.as_str().map(String::from)),
            span_parents: match json_obj.remove("span_parents") {
                None => None,
                Some(Value::Array(values)) => Some(
                    values
                        .into_iter()
                        .map(|value| match value {
                            Value::String(value) => Ok(value),
                            _ => Err(BraintrustError::InvalidConfig(
                                "span_parents must be an array of strings".to_string(),
                            )),
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
                Some(_) => {
                    return Err(BraintrustError::InvalidConfig(
                        "span_parents must be an array of strings".to_string(),
                    ))
                }
            },
            propagated_event: json_obj
                .remove("propagated_event")
                .and_then(|v| v.as_object().cloned()),
        })
    }

    /// Convert SpanComponents to ParentSpanInfo for creating child spans
    pub fn to_parent_span_info(&self) -> Result<ParentSpanInfo> {
        let span_id = self.span_id.clone().ok_or_else(|| {
            BraintrustError::InvalidConfig("span_id required for parent span".to_string())
        })?;
        let root_span_id = self.root_span_id.clone().ok_or_else(|| {
            BraintrustError::InvalidConfig("root_span_id required for parent span".to_string())
        })?;

        match self.object_type {
            SpanObjectType::ProjectLogs => {
                if self.object_id.is_none() && self.compute_object_metadata_args.is_none() {
                    return Err(BraintrustError::InvalidConfig(
                        "project-log parent span requires object_id or compute_object_metadata_args"
                            .to_string(),
                    ));
                }
            }
            SpanObjectType::Experiment | SpanObjectType::PlaygroundLogs => {
                if self.object_id.is_none() {
                    return Err(BraintrustError::InvalidConfig(
                        "object_id required for parent span".to_string(),
                    ));
                }
            }
        }

        Ok(ParentSpanInfo::FullSpan {
            object_type: self.object_type,
            object_id: self.object_id.clone(),
            compute_object_metadata_args: self.compute_object_metadata_args.clone(),
            span_id,
            root_span_id,
            span_parents: self.span_parents.clone(),
            propagated_event: self.propagated_event.clone(),
        })
    }

    /// Create SpanComponents from ParentSpanInfo
    pub fn from_parent_span_info(parent: &ParentSpanInfo) -> Option<Self> {
        match parent {
            ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                compute_object_metadata_args,
                span_id,
                root_span_id,
                span_parents,
                propagated_event,
            } => Some(Self {
                object_type: *object_type,
                object_id: object_id.clone(),
                compute_object_metadata_args: compute_object_metadata_args.clone(),
                row_id: None,
                span_id: Some(span_id.clone()),
                root_span_id: Some(root_span_id.clone()),
                span_parents: span_parents.clone(),
                propagated_event: propagated_event.clone(),
            }),
            _ => None,
        }
    }
}

impl fmt::Display for SpanComponents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_str())
    }
}

impl std::str::FromStr for SpanComponents {
    type Err = BraintrustError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

/// Try to parse a UUID string into 16 bytes
fn try_parse_uuid(s: &str) -> Option<Vec<u8>> {
    // Remove hyphens if present
    let clean = s.replace('-', "");

    // UUID should be 32 hex characters (16 bytes)
    if clean.len() != 32 {
        return None;
    }

    let mut bytes = Vec::with_capacity(16);
    for i in 0..16 {
        let hex = &clean[i * 2..i * 2 + 2];
        let byte = u8::from_str_radix(hex, 16).ok()?;
        bytes.push(byte);
    }

    Some(bytes)
}

/// Try to parse an 8-byte span ID (16 hex characters)
fn try_parse_hex_span_id(s: &str) -> Option<Vec<u8>> {
    if s.len() != 16 {
        return None;
    }

    let mut bytes = Vec::with_capacity(8);
    for i in 0..8 {
        let hex = &s[i * 2..i * 2 + 2];
        let byte = u8::from_str_radix(hex, 16).ok()?;
        bytes.push(byte);
    }

    Some(bytes)
}

/// Try to parse a 16-byte trace ID (32 hex characters)
fn try_parse_hex_trace_id(s: &str) -> Option<Vec<u8>> {
    if s.len() != 32 {
        return None;
    }

    let mut bytes = Vec::with_capacity(16);
    for i in 0..16 {
        let hex = &s[i * 2..i * 2 + 2];
        let byte = u8::from_str_radix(hex, 16).ok()?;
        bytes.push(byte);
    }

    Some(bytes)
}

/// Format bytes as lowercase hex string
fn format_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Format 16 bytes as a UUID string with hyphens
fn format_uuid(bytes: &[u8]) -> String {
    if bytes.len() != 16 {
        return format_hex(bytes);
    }

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_components_roundtrip() {
        let mut components = SpanComponents::new(SpanObjectType::ProjectLogs);
        components.object_id = Some("550e8400-e29b-41d4-a716-446655440000".to_string());
        components.span_id = Some("0123456789abcdef".to_string());
        components.root_span_id = Some("0123456789abcdef0123456789abcdef".to_string());
        components.span_parents = Some(vec!["parent-a".to_string()]);

        let encoded = components.to_str();
        let decoded = SpanComponents::parse(&encoded).unwrap();

        assert_eq!(decoded.object_type, SpanObjectType::ProjectLogs);
        assert_eq!(decoded.object_id, components.object_id);
        assert_eq!(decoded.span_id, components.span_id);
        assert_eq!(decoded.root_span_id, components.root_span_id);
        assert_eq!(decoded.span_parents, components.span_parents);
    }

    #[test]
    fn test_span_components_with_propagated_event() {
        let mut components = SpanComponents::new(SpanObjectType::Experiment);
        components.object_id = Some("550e8400-e29b-41d4-a716-446655440000".to_string());

        let mut propagated_event = Map::new();
        propagated_event.insert(
            "prompt_version".to_string(),
            Value::String("v1.2.3".to_string()),
        );
        components.propagated_event = Some(propagated_event);

        let encoded = components.to_str();
        let decoded = SpanComponents::parse(&encoded).unwrap();

        assert_eq!(decoded.object_type, SpanObjectType::Experiment);
        assert!(decoded.propagated_event.is_some());
        let event = decoded.propagated_event.unwrap();
        assert_eq!(
            event.get("prompt_version").and_then(|v| v.as_str()),
            Some("v1.2.3")
        );
    }

    #[test]
    fn test_hex_parsing() {
        // 8-byte span ID
        assert!(try_parse_hex_span_id("0123456789abcdef").is_some());
        assert!(try_parse_hex_span_id("0123456789abcde").is_none()); // too short

        // 16-byte trace ID
        assert!(try_parse_hex_trace_id("0123456789abcdef0123456789abcdef").is_some());
        assert!(try_parse_hex_trace_id("0123456789abcdef").is_none()); // too short

        // UUID
        assert!(try_parse_uuid("550e8400-e29b-41d4-a716-446655440000").is_some());
        assert!(try_parse_uuid("550e8400e29b41d4a716446655440000").is_some());
    }

    #[test]
    fn test_to_parent_span_info() {
        let mut components = SpanComponents::new(SpanObjectType::ProjectLogs);
        components.object_id = Some("project-123".to_string());
        components.span_id = Some("span-456".to_string());
        components.root_span_id = Some("root-789".to_string());
        components.span_parents = Some(vec!["parent-a".to_string()]);

        let mut propagated = Map::new();
        propagated.insert(
            "test_key".to_string(),
            Value::String("test_value".to_string()),
        );
        components.propagated_event = Some(propagated);

        let parent = components.to_parent_span_info().unwrap();

        match parent {
            ParentSpanInfo::FullSpan {
                object_type,
                object_id,
                span_id,
                root_span_id,
                span_parents,
                propagated_event,
                ..
            } => {
                assert_eq!(object_type, SpanObjectType::ProjectLogs);
                assert_eq!(object_id, Some("project-123".to_string()));
                assert_eq!(span_id, "span-456");
                assert_eq!(root_span_id, "root-789");
                assert_eq!(span_parents, Some(vec!["parent-a".to_string()]));
                assert!(propagated_event.is_some());
                let event = propagated_event.unwrap();
                assert_eq!(
                    event.get("test_key").and_then(|v| v.as_str()),
                    Some("test_value")
                );
            }
            _ => panic!("Expected FullSpan variant"),
        }
    }

    #[test]
    fn test_from_parent_span_info() {
        let mut propagated = Map::new();
        propagated.insert("key".to_string(), Value::String("value".to_string()));

        let parent = ParentSpanInfo::FullSpan {
            object_type: SpanObjectType::Experiment,
            object_id: Some("exp-123".to_string()),
            span_id: "span-456".to_string(),
            root_span_id: "root-789".to_string(),
            compute_object_metadata_args: None,
            span_parents: Some(vec!["parent-a".to_string()]),
            propagated_event: Some(propagated),
        };

        let components = SpanComponents::from_parent_span_info(&parent).unwrap();

        assert_eq!(components.object_type, SpanObjectType::Experiment);
        assert_eq!(components.object_id, Some("exp-123".to_string()));
        assert_eq!(components.span_id, Some("span-456".to_string()));
        assert_eq!(components.root_span_id, Some("root-789".to_string()));
        assert_eq!(components.span_parents, Some(vec!["parent-a".to_string()]));
        assert!(components.propagated_event.is_some());
    }

    #[test]
    fn test_parse_v3_json_remainder_preserves_span_parents() {
        let payload = serde_json::json!({
            "object_type": SpanObjectType::ProjectLogs as u8,
            "object_id": "project-123",
            "row_id": "row-123",
            "span_id": "span-123",
            "root_span_id": "root-123",
            "span_parents": ["parent-a"],
        });
        let encoded = BASE64.encode(
            [
                vec![ENCODING_VERSION_V3, SpanObjectType::ProjectLogs as u8, 0],
                serde_json::to_vec(&payload).unwrap(),
            ]
            .concat(),
        );

        let decoded = SpanComponents::parse(&encoded).unwrap();

        assert_eq!(decoded.span_parents, Some(vec!["parent-a".to_string()]));
    }

    #[test]
    fn test_parse_rejects_invalid_span_parents() {
        let payload = serde_json::json!({
            "object_type": SpanObjectType::ProjectLogs as u8,
            "span_parents": [123],
        });
        let encoded = BASE64.encode(
            [
                vec![ENCODING_VERSION_V4, SpanObjectType::ProjectLogs as u8, 0],
                serde_json::to_vec(&payload).unwrap(),
            ]
            .concat(),
        );

        let err = SpanComponents::parse(&encoded).unwrap_err();
        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
    }
}
