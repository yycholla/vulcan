//! CommandDispatcher: routes `/<command>` text to a builtin or custom
//! handler. Builtins are pre-registered; the user's TOML
//! `[gateway.commands]` adds (or overrides) custom commands.
//!
//! Worker invokes `dispatch(...)` against `/`-prefixed inbound text.
//! Returns:
//!   * `Ok(Some(reply))` — command handled, send `reply` to the user.
//!   * `Ok(None)`        — text isn't a registered slash command;
//!                         worker falls through to the streaming agent.
//!   * `Err(_)`          — command failed (e.g., shell crashed); worker
//!                         marks inbound failed.
//!
//! Naming note: the `RegisteredCommand` enum below would collide with
//! `tokio::process::Command` if it were named `Command`. Aliased the
//! enum (rather than the tokio import) so the type that lives on
//! `CommandDispatcher` carries a self-describing name.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command as TokioCommand;

use crate::client::ClientError;
use crate::config::CommandConfig;
use crate::gateway::lane::LaneKey;
use crate::gateway::lane_router::DaemonLaneRouter;

/// Built-in command. Each variant maps to a single dispatch fn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Help,
    Status,
    Clear,
    Resume,
}

impl Builtin {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "help" => Some(Self::Help),
            "status" => Some(Self::Status),
            "clear" => Some(Self::Clear),
            "resume" => Some(Self::Resume),
            _ => None,
        }
    }
}

/// What's stored in the dispatcher's command map. Either a builtin
/// (handler lives on `CommandDispatcher`) or a shell entry (spawned via
/// `tokio::process::Command`).
#[derive(Debug, Clone)]
pub enum RegisteredCommand {
    Builtin(Builtin),
    Shell {
        command: String,
        args: Vec<String>,
        timeout: Duration,
        working_dir: Option<std::path::PathBuf>,
    },
}

/// Minimal dispatch context — what every handler can read.
pub struct DispatchCtx<'a> {
    pub lane: &'a LaneKey,
    pub user_id: &'a str,
    /// Daemon-backed lane → session-id resolver. Builtins that
    /// previously poked at the in-process Agent cache directly now
    /// route through the daemon's RPC surface (Slice 3 Task 3.4).
    pub lane_router: &'a DaemonLaneRouter,
    /// User input AFTER the leading `/` and command name. e.g. for
    /// `"/resume abc-def"` the body is `"abc-def"`. For `/help` it's
    /// the empty string.
    pub body: &'a str,
}

pub struct CommandDispatcher {
    commands: HashMap<String, RegisteredCommand>,
}

impl CommandDispatcher {
    /// Build a dispatcher with the four builtins registered. The
    /// `user_overrides` map (typically `Config.gateway.commands`) is
    /// applied on top — an entry that names a builtin replaces it.
    pub fn new(user_overrides: &HashMap<String, CommandConfig>) -> Self {
        let mut commands = HashMap::new();
        commands.insert("help".into(), RegisteredCommand::Builtin(Builtin::Help));
        commands.insert("status".into(), RegisteredCommand::Builtin(Builtin::Status));
        commands.insert("clear".into(), RegisteredCommand::Builtin(Builtin::Clear));
        commands.insert("resume".into(), RegisteredCommand::Builtin(Builtin::Resume));

        for (name, cfg) in user_overrides {
            let cmd = match cfg {
                CommandConfig::Builtin { name: bname } => {
                    let Some(b) = Builtin::from_name(bname) else {
                        tracing::warn!(
                            target: "gateway::commands",
                            name = %name,
                            unknown = %bname,
                            "unknown builtin name in [gateway.commands]; ignoring",
                        );
                        continue;
                    };
                    RegisteredCommand::Builtin(b)
                }
                CommandConfig::Shell {
                    command,
                    args,
                    timeout_secs,
                    working_dir,
                } => RegisteredCommand::Shell {
                    command: command.clone(),
                    args: args.clone(),
                    timeout: Duration::from_secs(*timeout_secs),
                    working_dir: working_dir.clone(),
                },
            };
            commands.insert(name.clone(), cmd);
        }
        Self { commands }
    }

    /// Returns `Some(reply)` if `text` is a recognized slash command,
    /// `None` otherwise. Errors propagate (e.g., shell process panic).
    /// Command names are matched case-insensitively (`/Help`, `/HELP`,
    /// and `/help` all resolve the same builtin) — chat platforms
    /// autocapitalize on mobile, and a case-sensitive miss would
    /// silently route to the streaming agent.
    pub async fn dispatch(&self, text: &str, ctx: DispatchCtx<'_>) -> Result<Option<String>> {
        let stripped = match text.strip_prefix('/') {
            Some(rest) => rest.trim(),
            None => return Ok(None),
        };
        let (name, body) = match stripped.split_once(char::is_whitespace) {
            Some((n, b)) => (n, b.trim_start()),
            None => (stripped, ""),
        };
        let lower = name.to_ascii_lowercase();
        let Some(cmd) = self.commands.get(&lower) else {
            return Ok(None);
        };
        let body_ctx = DispatchCtx { body, ..ctx };
        match cmd {
            RegisteredCommand::Builtin(b) => self.run_builtin(*b, &body_ctx).await.map(Some),
            RegisteredCommand::Shell {
                command,
                args,
                timeout,
                working_dir,
            } => run_shell(command, args, *timeout, working_dir.as_deref(), &body_ctx)
                .await
                .map(Some),
        }
    }

    async fn run_builtin(&self, b: Builtin, ctx: &DispatchCtx<'_>) -> Result<String> {
        match b {
            Builtin::Help => Ok(self.help_text()),
            Builtin::Status => self.status_text(ctx).await,
            Builtin::Clear => self.clear(ctx).await,
            Builtin::Resume => self.resume(ctx).await,
        }
    }

    fn help_text(&self) -> String {
        let mut names: Vec<&String> = self.commands.keys().collect();
        names.sort();
        let body = names
            .iter()
            .map(|n| format!("• /{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("Available commands:\n{body}")
    }

    async fn status_text(&self, ctx: &DispatchCtx<'_>) -> Result<String> {
        // Slice 3 Task 3.4: status now queries the daemon for the
        // per-lane session's Agent. Lazy-build inside the daemon
        // produces an `AGENT_BUILD_FAILED` error if the active
        // provider profile can't initialize — surface that verbatim
        // so the operator sees the underlying cause.
        let session_id = ctx.lane_router.ensure_session(ctx.lane).await?;
        let mut client = ctx.lane_router.fresh_client().await?;
        let resp = match client
            .call_at_session(&session_id, "agent.status", serde_json::json!({}))
            .await
        {
            Ok(r) => r,
            Err(ClientError::Daemon(err)) => {
                anyhow::bail!("agent.status [{}]: {}", err.code, err.message)
            }
            Err(e) => return Err(anyhow::anyhow!("{e}")),
        };

        let unknown = serde_json::Value::String("[unknown]".into());
        let model = resp.get("model").unwrap_or(&unknown);
        let provider = resp
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("[default]");
        let max_ctx = resp
            .get("max_context")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let session = resp
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or(session_id.as_str());
        Ok(format!(
            "Session: {session}\nProvider: {provider}\nModel: {model}\nContext: {max_ctx}"
        ))
    }

    async fn clear(&self, ctx: &DispatchCtx<'_>) -> Result<String> {
        // /clear maps to `session.destroy` on the daemon. The next
        // inbound message for this lane will lazy-create a fresh
        // session via `ensure_session`.
        let session_id = DaemonLaneRouter::derive_session_id(ctx.lane);
        let mut client = ctx
            .lane_router
            .fresh_client()
            .await
            .with_context(|| "open daemon client for /clear")?;
        match client
            .call(
                "session.destroy",
                serde_json::json!({ "session_id": session_id }),
            )
            .await
        {
            Ok(_) => Ok("Cleared session — next message starts fresh.".into()),
            Err(ClientError::Daemon(err)) if err.code == "SESSION_NOT_FOUND" => {
                Ok("No active session to clear.".into())
            }
            Err(e) => Err(anyhow::anyhow!("session.destroy: {e}")),
        }
    }

    async fn resume(&self, ctx: &DispatchCtx<'_>) -> Result<String> {
        let session_id = ctx.body.trim();
        if session_id.is_empty() {
            return Ok("Usage: /resume <session-id>".into());
        }
        // The daemon's `session.resume` handler is still stubbed
        // (METHOD_NOT_IMPLEMENTED). Surface a clean message rather
        // than failing the inbound row outright; the legacy in-process
        // resume relied on the gateway-owned Agent which no longer
        // exists.
        let mut client = ctx.lane_router.fresh_client().await?;
        match client
            .call(
                "session.resume",
                serde_json::json!({ "session_id": session_id }),
            )
            .await
        {
            Ok(_) => Ok(format!("Resumed session {session_id}.")),
            Err(ClientError::Daemon(err)) if err.code == "METHOD_NOT_IMPLEMENTED" => Ok(
                "/resume is not yet supported with the daemon backend (YYC-266 follow-up).".into(),
            ),
            Err(e) => Err(anyhow::anyhow!("session.resume: {e}")),
        }
    }
}

const SHELL_OUTPUT_CAP_BYTES: usize = 16 * 1024;

/// SECURITY: `command` and `args` are sourced from operator config and
/// executed verbatim via `TokioCommand::new` (no shell, no expansion).
/// Inbound user text reaches the child only via stdin. Do NOT change
/// this contract without re-evaluating injection — composing `command`
/// or `args` from inbound text would let users execute arbitrary
/// processes under the gateway daemon's privileges.
async fn run_shell(
    command: &str,
    args: &[String],
    timeout: Duration,
    working_dir: Option<&std::path::Path>,
    ctx: &DispatchCtx<'_>,
) -> Result<String> {
    use tokio::io::AsyncWriteExt;

    let mut cmd = TokioCommand::new(command);
    cmd.args(args)
        .env("VULCAN_PLATFORM", &ctx.lane.platform)
        .env("VULCAN_CHAT_ID", &ctx.lane.chat_id)
        .env("VULCAN_USER_ID", ctx.user_id)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(wd) = working_dir {
        cmd.current_dir(wd);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn shell command '{command}'"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(ctx.body.as_bytes()).await.ok();
        // Explicitly drop so the child sees EOF — wait_with_output below
        // would block forever on a process that reads stdin to end.
        drop(stdin);
    }

    // wait_with_output consumes the child; safe here because we already
    // took stdin out above (so the child sees EOF and can exit).
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => out.context("collect shell command output")?,
        Err(_) => anyhow::bail!("shell command '{command}' timed out after {timeout:?}"),
    };
    let mut stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if stdout.len() > SHELL_OUTPUT_CAP_BYTES {
        // String::truncate panics if `n` lands mid-codepoint. UTF-8
        // multibyte chars near the byte cap (e.g. a `ä` straddling
        // byte 16384) would otherwise crash the worker. floor_char_boundary
        // walks back to the nearest valid char boundary <= n.
        let n = stdout.floor_char_boundary(SHELL_OUTPUT_CAP_BYTES);
        stdout.truncate(n);
        stdout.push_str("\n…(truncated)");
    }
    if !output.status.success() {
        let stderr_tail = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr_tail
            .chars()
            .rev()
            .take(1024)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return Ok(format!(
            "Command failed (exit {:?}):\n{stdout}\n--- stderr ---\n{tail}",
            output.status.code(),
        ));
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_overrides() -> HashMap<String, CommandConfig> {
        HashMap::new()
    }

    #[test]
    fn dispatcher_registers_four_builtins_by_default() {
        let d = CommandDispatcher::new(&empty_overrides());
        assert!(matches!(
            d.commands.get("help"),
            Some(RegisteredCommand::Builtin(Builtin::Help))
        ));
        assert!(matches!(
            d.commands.get("status"),
            Some(RegisteredCommand::Builtin(Builtin::Status))
        ));
        assert!(matches!(
            d.commands.get("clear"),
            Some(RegisteredCommand::Builtin(Builtin::Clear))
        ));
        assert!(matches!(
            d.commands.get("resume"),
            Some(RegisteredCommand::Builtin(Builtin::Resume))
        ));
    }

    #[test]
    fn builtin_from_name_recognizes_four_builtins() {
        assert_eq!(Builtin::from_name("help"), Some(Builtin::Help));
        assert_eq!(Builtin::from_name("status"), Some(Builtin::Status));
        assert_eq!(Builtin::from_name("clear"), Some(Builtin::Clear));
        assert_eq!(Builtin::from_name("resume"), Some(Builtin::Resume));
        assert_eq!(Builtin::from_name("nope"), None);
    }

    #[test]
    fn user_override_can_replace_a_builtin_with_shell() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "status".into(),
            CommandConfig::Shell {
                command: "/bin/echo".into(),
                args: vec![],
                timeout_secs: 1,
                working_dir: None,
            },
        );
        let d = CommandDispatcher::new(&overrides);
        match d.commands.get("status") {
            Some(RegisteredCommand::Shell { .. }) => {}
            _ => panic!("override should replace builtin with shell"),
        }
    }

    #[test]
    fn user_override_with_unknown_builtin_name_is_ignored() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "nope".into(),
            CommandConfig::Builtin {
                name: "not-a-real-builtin".into(),
            },
        );
        let d = CommandDispatcher::new(&overrides);
        assert!(!d.commands.contains_key("nope"));
    }

    /// Stand-in router for tests where the dispatched command never
    /// touches the daemon (`/help`, non-slash text). The factory
    /// returns an error if invoked, which would surface as an
    /// assertion failure if the dispatcher unexpectedly tried to
    /// connect.
    fn router_no_daemon() -> DaemonLaneRouter {
        DaemonLaneRouter::with_client_factory(|| {
            Box::pin(async {
                Err(ClientError::Protocol(
                    "test router: client factory must not be invoked".into(),
                ))
            })
        })
    }

    #[tokio::test]
    async fn dispatch_is_case_insensitive_for_builtins() {
        let d = CommandDispatcher::new(&empty_overrides());
        let lane = LaneKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
        };
        let lane_router = router_no_daemon();
        let ctx = DispatchCtx {
            lane: &lane,
            user_id: "u",
            lane_router: &lane_router,
            body: "",
        };
        let reply = d
            .dispatch("/HELP", ctx)
            .await
            .expect("dispatch ok")
            .expect("uppercase /HELP should hit builtin /help");
        assert!(reply.starts_with("Available commands:"));
    }

    #[tokio::test]
    async fn dispatch_returns_none_for_non_slash_text() {
        let d = CommandDispatcher::new(&empty_overrides());
        let lane = LaneKey {
            platform: "loopback".into(),
            chat_id: "c".into(),
        };
        let lane_router = router_no_daemon();
        let ctx = DispatchCtx {
            lane: &lane,
            user_id: "u",
            lane_router: &lane_router,
            body: "",
        };
        assert!(d.dispatch("hello world", ctx).await.unwrap().is_none());
    }
}
