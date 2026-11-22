use super::build_api_client;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
const PROTOCOL_VERSION: &str = "2025-03-26";
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

impl Tool {
    /// Describe a tool and its JSON input schema.
    pub fn new(name: impl Into<String>, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            input_schema,
        }
    }
}
#[derive(Debug, Deserialize)]
struct ToolsResult {
    tools: Vec<Tool>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}
#[derive(Debug, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}
#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}
#[derive(Debug, Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ToolResult {
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
    #[serde(rename = "structuredContent", skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
pub struct McpSession {
    http: crate::Client,
    session_id: Option<String>,
    protocol_version: Option<String>,
    next_id: u64,
    client_name: String,
    client_version: String,
}
impl McpSession {
    pub fn new(
        api_url: &str,
        api_key: &str,
        client_name: &str,
        client_version: &str,
    ) -> Result<Self> {
        let endpoint = endpoint(api_url);
        Ok(Self {
            http: build_api_client(
                &endpoint,
                &endpoint,
                api_key,
                std::time::Duration::from_secs(60),
            )?,
            session_id: None,
            protocol_version: None,
            next_id: 1,
            client_name: client_name.into(),
            client_version: client_version.into(),
        })
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let response = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": self.client_name, "version": self.client_version }
                }),
            )
            .await?;
        self.protocol_version = response
            .get("protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if self.protocol_version.is_none() {
            bail!("MCP server returned an invalid initialize response");
        }
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    pub async fn list_tools(&mut self) -> Result<Vec<Tool>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = Map::new();
            if let Some(value) = cursor.take() {
                params.insert("cursor".to_string(), Value::String(value));
            }
            let value = self.request("tools/list", Value::Object(params)).await?;
            let page: ToolsResult =
                serde_json::from_value(value).context("invalid tools/list response")?;
            tools.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<ToolResult> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            bail!(
                "MCP tool `{name}` failed: {}",
                result
                    .get("content")
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            );
        }
        serde_json::from_value(result).context("invalid MCP tool result")
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = RpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let response = self.post(serde_json::to_value(request)?).await?;
        let rpc: RpcResponse =
            serde_json::from_value(response).context("invalid MCP JSON-RPC response")?;
        if let Some(error) = rpc.error {
            bail!("MCP server error {}: {}", error.code, error.message);
        }
        rpc.result.context("MCP server response had no result")
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let value = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let request = self.request_builder(value).await?;
        let response = request.send().await.context("MCP notification failed")?;
        self.capture_session(&response);
        Ok(())
    }

    async fn post(&mut self, value: Value) -> Result<Value> {
        let response = self
            .request_builder(value)
            .await?
            .send()
            .await
            .context("MCP request failed")?;
        self.capture_session(&response);
        let body = response
            .text()
            .await
            .context("failed reading MCP response")?;
        parse_rpc_body(&body)
    }

    async fn request_builder(&self, value: Value) -> Result<crate::Request> {
        let mut request = self
            .http
            .api_request(reqwest::Method::POST, "")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(&value);
        if let Some(session_id) = &self.session_id {
            request = request.header("Mcp-Session-Id", session_id);
        }
        if let Some(protocol_version) = &self.protocol_version {
            request = request.header("MCP-Protocol-Version", protocol_version);
        }
        Ok(request)
    }

    fn capture_session(&mut self, response: &reqwest::Response) {
        if let Some(value) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(value.to_owned());
        }
    }
}

fn parse_rpc_body(body: &str) -> Result<Value> {
    if let Ok(value) = serde_json::from_str(body) {
        return Ok(value);
    }
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:").map(str::trim)
            && !data.is_empty()
            && data != "[DONE]"
        {
            return serde_json::from_str(data).context("invalid MCP event stream response");
        }
    }
    Err(anyhow!(
        "MCP server returned an empty or unsupported response"
    ))
}

pub fn endpoint(api_url: &str) -> String {
    format!("{}/mcp", api_url.trim_end_matches('/'))
}
