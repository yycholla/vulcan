use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpToolCallResult {
    #[serde(default)]
    pub content: Vec<McpContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

pub struct McpClient<R, W> {
    reader: R,
    writer: W,
    next_id: AtomicU64,
}

impl<R, W> McpClient<R, W>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: AtomicU64::new(1),
        }
    }

    pub async fn initialize(&mut self) -> Result<()> {
        let _: Value = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "vulcan",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    pub async fn list_tools(&mut self) -> Result<Vec<McpTool>> {
        #[derive(Deserialize)]
        struct ToolsResult {
            #[serde(default)]
            tools: Vec<McpTool>,
        }

        let result: ToolsResult = self.request("tools/list", json!({})).await?;
        Ok(result.tools)
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<McpToolCallResult> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .await
    }

    async fn request<T>(&mut self, method: &str, params: Value) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        write_frame(
            &mut self.writer,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
        )
        .await?;

        loop {
            let frame = read_frame(&mut self.reader).await?;
            if frame.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = frame.get("error") {
                anyhow::bail!("MCP `{method}` failed: {error}");
            }
            let result = frame
                .get("result")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("MCP `{method}` response missing result"))?;
            return serde_json::from_value(result)
                .with_context(|| format!("MCP `{method}` response had unexpected shape"));
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        write_frame(
            &mut self.writer,
            &json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
            }),
        )
        .await
    }
}

pub async fn write_frame<W>(writer: &mut W, value: &Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Value>
where
    R: AsyncBufRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("MCP server closed stdout");
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(raw) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(raw.trim().parse::<usize>()?);
        }
    }

    let len = content_length.ok_or_else(|| anyhow::anyhow!("MCP frame missing Content-Length"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{BufReader, duplex};

    use super::*;

    #[tokio::test]
    async fn writes_and_reads_content_length_frames() {
        let (client, server) = duplex(4096);
        let (client_read, mut client_write) = tokio::io::split(client);
        let (server_read, server_write) = tokio::io::split(server);
        let mut server_reader = BufReader::new(server_read);

        write_frame(&mut client_write, &json!({"jsonrpc": "2.0", "id": 1}))
            .await
            .unwrap();

        let sent = read_frame(&mut server_reader).await.unwrap();
        assert_eq!(sent["id"], 1);

        let mut reader = BufReader::new(client_read);
        let mut writer = server_write;
        write_frame(
            &mut writer,
            &json!({"jsonrpc": "2.0", "id": 2, "result": {"ok": true}}),
        )
        .await
        .unwrap();
        let got = read_frame(&mut reader).await.unwrap();
        assert_eq!(got["id"], 2);
        assert_eq!(got["result"]["ok"], true);
    }

    #[tokio::test]
    async fn client_initializes_lists_and_calls_tools() {
        let (client_side, server_side) = duplex(8192);
        let (client_read, client_write) = tokio::io::split(client_side);
        let (server_read, mut server_write) = tokio::io::split(server_side);
        let mut server_reader = BufReader::new(server_read);

        let server = tokio::spawn(async move {
            let initialize = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(initialize["method"], "initialize");
            write_frame(
                &mut server_write,
                &json!({
                    "jsonrpc": "2.0",
                    "id": initialize["id"],
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "serverInfo": {"name": "fake", "version": "0.1.0"}
                    }
                }),
            )
            .await
            .unwrap();

            let initialized = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(initialized["method"], "notifications/initialized");

            let list = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(list["method"], "tools/list");
            write_frame(
                &mut server_write,
                &json!({
                    "jsonrpc": "2.0",
                    "id": list["id"],
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Echo input",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"text": {"type": "string"}},
                                "required": ["text"]
                            }
                        }]
                    }
                }),
            )
            .await
            .unwrap();

            let call = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(call["method"], "tools/call");
            assert_eq!(call["params"]["name"], "echo");
            assert_eq!(call["params"]["arguments"]["text"], "hello");
            write_frame(
                &mut server_write,
                &json!({
                    "jsonrpc": "2.0",
                    "id": call["id"],
                    "result": {
                        "content": [{"type": "text", "text": "hello"}],
                        "isError": false
                    }
                }),
            )
            .await
            .unwrap();
        });

        let mut client = McpClient::new(BufReader::new(client_read), client_write);
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        let result = client
            .call_tool("echo", json!({"text": "hello"}))
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(result.content[0].text.as_deref(), Some("hello"));
        server.await.unwrap();
    }
}
