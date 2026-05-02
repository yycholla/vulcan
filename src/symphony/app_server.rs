//! Generic line-delimited app-server client for Symphony worker processes.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use crate::symphony::workflow::NormalizedTask;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerClient {
    config: AppServerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerRequest {
    pub task: NormalizedTask,
    pub workspace: PathBuf,
    pub prompt: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppServerOutcome {
    Completed(AppServerTelemetry),
    Failed { message: String },
    Cancelled,
    TimedOut,
    ProcessExited { status: String },
    MalformedMessage { line: String, message: String },
    UnsupportedToolCall { name: Option<String> },
    InputRequired { prompt: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerTelemetry {
    pub session_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub messages: Vec<AppServerTurnMessage>,
    pub rate_limits: Vec<AppServerRateLimit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerTurnMessage {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerRateLimit {
    pub name: String,
    pub limit: u64,
    pub remaining: u64,
    pub reset_at_ms: Option<u64>,
}

#[derive(Debug, Error)]
pub enum AppServerError {
    #[error("failed to launch app-server process `{command}` in `{cwd}`: {source}")]
    Launch {
        command: String,
        cwd: String,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write app-server protocol message: {0}")]
    Write(std::io::Error),
}

impl AppServerClient {
    pub fn new(config: AppServerConfig) -> Self {
        Self { config }
    }

    pub fn run_turn(&self, request: AppServerRequest) -> Result<AppServerOutcome, AppServerError> {
        let mut child = Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(&request.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| AppServerError::Launch {
                command: self.config.command.clone(),
                cwd: request.workspace.display().to_string(),
                source,
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            for message in handshake_messages(&request) {
                serde_json::to_writer(&mut stdin, &message).map_err(|err| {
                    AppServerError::Write(std::io::Error::new(std::io::ErrorKind::InvalidData, err))
                })?;
                stdin.write_all(b"\n").map_err(AppServerError::Write)?;
            }
        }

        let Some(output) = wait_with_timeout(child, self.config.timeout) else {
            return Ok(AppServerOutcome::TimedOut);
        };
        if !output.stderr.is_empty() {
            tracing::debug!(
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "app-server process wrote diagnostics"
            );
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut terminal = None;
        let mut messages = Vec::new();
        for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
            let message = match serde_json::from_str::<JsonValue>(line) {
                Ok(message) => message,
                Err(err) => {
                    return Ok(AppServerOutcome::MalformedMessage {
                        line: line.to_string(),
                        message: err.to_string(),
                    });
                }
            };
            match map_protocol_message(&request.task.identifier, &message, &messages) {
                ProtocolAction::Terminal(outcome) => {
                    terminal = Some(outcome);
                    break;
                }
                ProtocolAction::Message(message) => messages.push(message),
                ProtocolAction::Ignore => {}
            }
        }

        Ok(terminal.unwrap_or_else(|| AppServerOutcome::ProcessExited {
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |code| code.to_string()),
        }))
    }
}

fn handshake_messages(request: &AppServerRequest) -> [JsonValue; 4] {
    [
        json!({
            "type": "initialize",
            "protocol": "vulcan.app_server.v1",
        }),
        json!({
            "type": "initialized",
        }),
        json!({
            "type": "thread/start",
            "task_id": request.task.id,
            "identifier": request.task.identifier,
            "attempt": request.attempt,
        }),
        json!({
            "type": "turn/start",
            "prompt": request.prompt,
        }),
    ]
}

fn wait_with_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> Option<std::process::Output> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().ok().flatten().is_some() {
            return child.wait_with_output().ok();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtocolAction {
    Terminal(AppServerOutcome),
    Message(AppServerTurnMessage),
    Ignore,
}

fn map_protocol_message(
    identifier: &str,
    message: &JsonValue,
    messages: &[AppServerTurnMessage],
) -> ProtocolAction {
    match message.get("type").and_then(JsonValue::as_str) {
        Some("turn/completed") => {
            ProtocolAction::Terminal(AppServerOutcome::Completed(AppServerTelemetry {
                session_id: compose_session_id(identifier, string_field(message, "session_id")),
                input_tokens: message
                    .get("usage")
                    .and_then(|usage| usage.get("input_tokens"))
                    .and_then(JsonValue::as_u64)
                    .unwrap_or_default(),
                output_tokens: message
                    .get("usage")
                    .and_then(|usage| usage.get("output_tokens"))
                    .and_then(JsonValue::as_u64)
                    .unwrap_or_default(),
                messages: messages.to_vec(),
                rate_limits: rate_limits(message),
            }))
        }
        Some("turn/failed") => ProtocolAction::Terminal(AppServerOutcome::Failed {
            message: string_field(message, "message").unwrap_or_default(),
        }),
        Some("turn/cancelled") => ProtocolAction::Terminal(AppServerOutcome::Cancelled),
        Some("input_required") => ProtocolAction::Terminal(AppServerOutcome::InputRequired {
            prompt: string_field(message, "prompt"),
        }),
        Some("tool_call") | Some("tool/call") => {
            ProtocolAction::Terminal(AppServerOutcome::UnsupportedToolCall {
                name: string_field(message, "name"),
            })
        }
        Some("turn/message") | Some("turn/delta") => {
            ProtocolAction::Message(AppServerTurnMessage {
                text: string_field(message, "text").unwrap_or_default(),
            })
        }
        _ => ProtocolAction::Ignore,
    }
}

fn compose_session_id(identifier: &str, session_id: Option<String>) -> String {
    match session_id {
        Some(session_id) if !session_id.is_empty() => format!("{identifier}:{session_id}"),
        _ => identifier.to_string(),
    }
}

fn rate_limits(message: &JsonValue) -> Vec<AppServerRateLimit> {
    message
        .get("rate_limits")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .map(|limit| AppServerRateLimit {
            name: string_field(limit, "name").unwrap_or_default(),
            limit: limit
                .get("limit")
                .and_then(JsonValue::as_u64)
                .unwrap_or_default(),
            remaining: limit
                .get("remaining")
                .and_then(JsonValue::as_u64)
                .unwrap_or_default(),
            reset_at_ms: limit.get("reset_at_ms").and_then(JsonValue::as_u64),
        })
        .collect()
}

fn string_field(message: &JsonValue, field: &str) -> Option<String> {
    message
        .get(field)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use serde_json::Value as JsonValue;
    use tempfile::TempDir;

    use super::*;
    use crate::symphony::workflow::NormalizedTask;

    #[test]
    fn completed_turn_launches_in_workspace_sends_handshake_and_extracts_telemetry() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let fake = fake_process(
            &temp,
            r#"#!/bin/sh
pwd > "$1/cwd.txt"
cat > "$1/stdin.jsonl"
printf '{"type":"turn/message","text":"working"}\n'
printf '{"type":"turn/com'
printf 'pleted","session_id":"worker-session","usage":{"input_tokens":42,"output_tokens":7},"rate_limits":[{"name":"requests","limit":100,"remaining":93,"reset_at_ms":60000}]}\n'
"#,
        );
        let client = AppServerClient::new(AppServerConfig {
            command: fake.display().to_string(),
            args: vec![temp.path().display().to_string()],
            timeout: Duration::from_secs(1),
        });

        let outcome = client
            .run_turn(AppServerRequest {
                task: task("600", "GH-600"),
                workspace: workspace.clone(),
                prompt: "Implement the app-server client".into(),
                attempt: 2,
            })
            .unwrap();

        assert_eq!(
            outcome,
            AppServerOutcome::Completed(AppServerTelemetry {
                session_id: "GH-600:worker-session".into(),
                input_tokens: 42,
                output_tokens: 7,
                messages: vec![AppServerTurnMessage {
                    text: "working".into(),
                }],
                rate_limits: vec![AppServerRateLimit {
                    name: "requests".into(),
                    limit: 100,
                    remaining: 93,
                    reset_at_ms: Some(60_000),
                }],
            })
        );
        assert_eq!(
            fs::read_to_string(temp.path().join("cwd.txt"))
                .unwrap()
                .trim(),
            workspace.display().to_string()
        );
        let messages = fs::read_to_string(temp.path().join("stdin.jsonl")).unwrap();
        let types = messages
            .lines()
            .map(|line| serde_json::from_str::<JsonValue>(line).unwrap()["type"].to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            types,
            [
                "\"initialize\"",
                "\"initialized\"",
                "\"thread/start\"",
                "\"turn/start\"",
            ]
        );
    }

    #[test]
    fn terminal_protocol_events_map_to_documented_outcomes() {
        let cases = [
            (
                r#"{"type":"turn/failed","message":"provider rejected request"}"#,
                AppServerOutcome::Failed {
                    message: "provider rejected request".into(),
                },
            ),
            (r#"{"type":"turn/cancelled"}"#, AppServerOutcome::Cancelled),
            (
                r#"{"type":"input_required","prompt":"approve next step"}"#,
                AppServerOutcome::InputRequired {
                    prompt: Some("approve next step".into()),
                },
            ),
            (
                r#"{"type":"tool_call","name":"shell"}"#,
                AppServerOutcome::UnsupportedToolCall {
                    name: Some("shell".into()),
                },
            ),
            (
                r#"{"type":"turn/completed""#,
                AppServerOutcome::MalformedMessage {
                    line: r#"{"type":"turn/completed""#.into(),
                    message: "EOF while parsing an object at line 1 column 24".into(),
                },
            ),
        ];

        for (line, expected) in cases {
            let temp = TempDir::new().unwrap();
            let workspace = temp.path().join("workspace");
            fs::create_dir_all(&workspace).unwrap();
            let escaped = line.replace('\'', "'\\''");
            let fake = fake_process(
                &temp,
                &format!("#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{}'\n", escaped),
            );
            let client = AppServerClient::new(AppServerConfig {
                command: fake.display().to_string(),
                args: Vec::new(),
                timeout: Duration::from_secs(1),
            });

            let outcome = client
                .run_turn(AppServerRequest {
                    task: task("600", "GH-600"),
                    workspace,
                    prompt: "Prompt".into(),
                    attempt: 1,
                })
                .unwrap();

            assert_eq!(outcome, expected);
        }
    }

    #[test]
    fn process_exit_timeout_and_stderr_diagnostics_map_without_stdout_pollution() {
        let exited = run_fake(
            "#!/bin/sh\ncat >/dev/null\necho diagnostics >&2\nexit 7\n",
            Duration::from_secs(1),
        );
        assert_eq!(
            exited,
            AppServerOutcome::ProcessExited { status: "7".into() }
        );

        let timed_out = run_fake(
            "#!/bin/sh\ncat >/dev/null\nsleep 2\n",
            Duration::from_millis(20),
        );
        assert_eq!(timed_out, AppServerOutcome::TimedOut);

        let completed_with_stderr = run_fake(
            r#"#!/bin/sh
cat >/dev/null
echo diagnostic-only >&2
printf '{"type":"turn/completed","session_id":"session","usage":{"input_tokens":1,"output_tokens":2}}\n'
"#,
            Duration::from_secs(1),
        );
        assert_eq!(
            completed_with_stderr,
            AppServerOutcome::Completed(AppServerTelemetry {
                session_id: "GH-600:session".into(),
                input_tokens: 1,
                output_tokens: 2,
                messages: Vec::new(),
                rate_limits: Vec::new(),
            })
        );
    }

    fn run_fake(body: &str, timeout: Duration) -> AppServerOutcome {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let fake = fake_process(&temp, body);
        AppServerClient::new(AppServerConfig {
            command: fake.display().to_string(),
            args: Vec::new(),
            timeout,
        })
        .run_turn(AppServerRequest {
            task: task("600", "GH-600"),
            workspace,
            prompt: "Prompt".into(),
            attempt: 1,
        })
        .unwrap()
    }

    fn fake_process(temp: &TempDir, body: &str) -> std::path::PathBuf {
        let path = temp.path().join("fake-worker.sh");
        fs::write(&path, body).unwrap();
        let mut perms = fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).unwrap();
        path
    }

    fn task(id: &str, identifier: &str) -> NormalizedTask {
        NormalizedTask {
            id: id.into(),
            identifier: identifier.into(),
            title: "App server client".into(),
            body: "Body".into(),
            priority: None,
            state: "ready-for-agent".into(),
            branch: None,
            labels: Vec::new(),
            blockers: Vec::new(),
            url: None,
            path: None,
            created_at: None,
            updated_at: None,
            source: Default::default(),
        }
    }
}
