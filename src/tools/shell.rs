use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Execute a shell command. Use for builds, installs, git, scripts, and anything else needing a shell. Output is captured and returned."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout": { "type": "integer", "description": "Max seconds to wait", "default": 60, "minimum": 1, "maximum": 300 },
                "workdir": { "type": "string", "description": "Working directory for the command" }
            },
            "required": ["command"]
        })
    }
    async fn call(&self, params: Value) -> Result<String> {
        let command = params["command"].as_str().ok_or_else(|| anyhow::anyhow!("command required"))?;
        let timeout = params["timeout"].as_i64().unwrap_or(60);
        let workdir = params["workdir"].as_str();

        let mut cmd = tokio::process::Command::new("bash");
        cmd.arg("-c").arg(command);

        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout as u64),
            cmd.output(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Command timed out after {timeout}s"))??;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut result = String::new();

        if output.status.success() {
            result.push_str(stdout.trim());
        } else {
            result.push_str(&format!("Exit code: {}\n", output.status.code().unwrap_or(-1)));
            if !stderr.is_empty() {
                result.push_str(&format!("stderr:\n{stderr}"));
            }
            if !stdout.is_empty() {
                result.push_str(&format!("\nstdout:\n{stdout}"));
            }
        }

        if result.len() > 50_000 {
            result.truncate(50_000);
            result.push_str("\n... (truncated at 50K chars)");
        }

        Ok(result)
    }
}
