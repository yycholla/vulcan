use std::collections::HashMap;

use serde_json::json;
use tokio_util::sync::CancellationToken;
use vulcan::mcp::{McpExposeMode, McpServerConfig, connect_configured_servers};
use vulcan::tools::ToolRegistry;

const FAKE_MCP_SERVER: &str = r#"
import json
import sys

def read_frame():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        line = line.decode().strip()
        if not line:
            break
        if line.lower().startswith("content-length:"):
            length = int(line.split(":", 1)[1].strip())
    body = sys.stdin.buffer.read(length)
    return json.loads(body.decode())

def write_frame(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(body)}\r\n\r\n".encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

while True:
    frame = read_frame()
    method = frame.get("method")
    if method == "initialize":
        write_frame({
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "serverInfo": {"name": "fake", "version": "0.1.0"},
            },
        })
    elif method == "notifications/initialized":
        continue
    elif method == "tools/list":
        write_frame({
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {
                "tools": [{
                    "name": "echo",
                    "description": "Echo text through a fake MCP server",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    },
                }]
            },
        })
    elif method == "tools/call":
        text = frame.get("params", {}).get("arguments", {}).get("text", "")
        write_frame({
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {
                "content": [{"type": "text", "text": "echo:" + text}],
                "isError": False,
            },
        })
    else:
        write_frame({
            "jsonrpc": "2.0",
            "id": frame.get("id"),
            "error": {"code": -32601, "message": "unknown method"},
        })
"#;

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn fake_server_config(enabled: bool, expose_as: McpExposeMode) -> McpServerConfig {
    McpServerConfig {
        name: "fake".to_string(),
        command: "python3".to_string(),
        args: vec![
            "-u".to_string(),
            "-c".to_string(),
            FAKE_MCP_SERVER.to_string(),
        ],
        env: HashMap::new(),
        enabled,
        expose_as,
        timeout_secs: 5,
        ..Default::default()
    }
}

#[tokio::test]
async fn disabled_mcp_server_does_not_expose_tools() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new_with_diff_and_lsp(None, None, dir.path().to_path_buf());
    let handles = connect_configured_servers(
        &[fake_server_config(false, McpExposeMode::Auto)],
        &mut registry,
    )
    .await;

    assert!(handles.is_empty());
    assert!(!registry.contains("mcp_fake_echo"));
}

#[tokio::test]
async fn configured_stdio_mcp_tool_registers_and_dispatches() {
    if !python3_available() {
        eprintln!("skipping MCP stdio integration test: python3 unavailable");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new_with_diff_and_lsp(None, None, dir.path().to_path_buf());
    let handles = connect_configured_servers(
        &[fake_server_config(true, McpExposeMode::Auto)],
        &mut registry,
    )
    .await;

    assert_eq!(handles.len(), 1);
    assert!(registry.contains("mcp_fake_echo"));
    let tool = registry
        .definitions()
        .into_iter()
        .find(|tool| tool.function.name == "mcp_fake_echo")
        .expect("MCP tool definition exposed");
    assert!(tool.function.description.contains("fake MCP server"));

    let result = registry
        .execute(
            "mcp_fake_echo",
            &json!({"text": "hello"}).to_string(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(!result.is_error);
    assert_eq!(result.output, "echo:hello");
    assert!(result.details.is_some());
}
