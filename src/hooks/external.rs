//! User-configured subprocess hook handlers.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::extensions::{
    ExtensionAuditEvent, ExtensionAuditLog, ExternalHookAuditAction, ExternalHookAuditEvent,
};
use crate::tools::ToolResult;

use super::{HookHandler, HookOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalHookEvent {
    BeforeToolCall,
    AfterToolCall,
}

impl ExternalHookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalHookMatch {
    pub tool: Option<String>,
}

impl ExternalHookMatch {
    fn matches_tool(&self, tool: &str) -> bool {
        self.tool.as_deref().is_none_or(|expected| expected == tool)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalHookPolicy {
    Allow,
    #[default]
    Deny,
    RequireApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalHookConfig {
    pub id: String,
    pub event: ExternalHookEvent,
    #[serde(rename = "match")]
    pub match_rule: ExternalHookMatch,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub enabled: bool,
    pub policy: ExternalHookPolicy,
    pub priority: i32,
    pub timeout_secs: u64,
}

impl Default for ExternalHookConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            event: ExternalHookEvent::BeforeToolCall,
            match_rule: ExternalHookMatch::default(),
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            enabled: false,
            policy: ExternalHookPolicy::Deny,
            priority: 50,
            timeout_secs: 10,
        }
    }
}

impl ExternalHookConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }

    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        if self.command.trim().is_empty() {
            bail!("external hook `{}` command must not be empty", self.id);
        }
        if self.command.contains('/') || self.command.contains('\\') {
            let path = Path::new(&self.command);
            if !path.is_absolute() {
                bail!(
                    "external hook `{}` command path must be absolute: {}",
                    self.id,
                    self.command
                );
            }
            if !path.is_file() {
                bail!(
                    "external hook `{}` command path does not exist or is not a file: {}",
                    self.id,
                    self.command
                );
            }
        }
        if !(0..=1000).contains(&self.priority) {
            bail!(
                "external hook `{}` priority must be between 0 and 1000",
                self.id
            );
        }
        if self.timeout_secs == 0 || self.timeout_secs > 300 {
            bail!(
                "external hook `{}` timeout_secs must be between 1 and 300",
                self.id
            );
        }
        if matches!(
            self.event,
            ExternalHookEvent::BeforeToolCall | ExternalHookEvent::AfterToolCall
        ) && self.match_rule.tool.as_deref().is_some_and(str::is_empty)
        {
            bail!("external hook `{}` match.tool must not be empty", self.id);
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("external hook id must not be empty");
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("external hook id `{id}` contains unsupported characters");
    }
    Ok(())
}

pub fn configured_handlers(
    configs: &[ExternalHookConfig],
    audit_log: Option<Arc<ExtensionAuditLog>>,
) -> Vec<Arc<dyn HookHandler>> {
    configs
        .iter()
        .filter(|cfg| cfg.enabled)
        .filter_map(
            |cfg| match ExternalHookHandler::new(cfg.clone(), audit_log.clone()) {
                Ok(handler) => Some(Arc::new(handler) as Arc<dyn HookHandler>),
                Err(err) => {
                    tracing::warn!(hook_id = %cfg.id, %err, "skipping invalid external hook");
                    None
                }
            },
        )
        .collect()
}

pub struct ExternalHookHandler {
    config: ExternalHookConfig,
    audit_log: Option<Arc<ExtensionAuditLog>>,
}

impl ExternalHookHandler {
    pub fn new(
        config: ExternalHookConfig,
        audit_log: Option<Arc<ExtensionAuditLog>>,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self { config, audit_log })
    }

    fn allowed_by_policy(&self) -> bool {
        match self.config.policy {
            ExternalHookPolicy::Allow => true,
            ExternalHookPolicy::Deny => {
                self.record(ExternalHookAuditAction::Denied {
                    reason: "external hook policy is deny".to_string(),
                });
                false
            }
            ExternalHookPolicy::RequireApproval => {
                self.record(ExternalHookAuditAction::ApprovalRequired {
                    reason: "external hook requires approval; no approval flow is available for subprocess hooks yet".to_string(),
                });
                false
            }
        }
    }

    async fn dispatch(&self, event: ExternalHookEvent, payload: Value) -> Result<HookOutcome> {
        if !self.allowed_by_policy() {
            return Ok(HookOutcome::Continue);
        }

        let input = json!({
            "vulcan_hook_version": 1,
            "event": event.as_str(),
            "hook": {
                "id": self.config.id,
                "priority": self.config.priority,
            },
            "payload": payload,
        });
        let input = serde_json::to_vec(&input)?;

        let mut child = match Command::new(&self.config.command)
            .args(&self.config.args)
            .envs(&self.config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                self.record(ExternalHookAuditAction::Failed {
                    reason: format!("spawn failed: {err}"),
                });
                return Err(err).with_context(|| {
                    format!("failed to spawn external hook `{}`", self.config.id)
                });
            }
        };

        if let Some(stdin) = child.stdin.as_mut() {
            stdin.write_all(&input).await?;
            stdin.shutdown().await?;
        }

        let output = match timeout(self.config.timeout(), child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                self.record(ExternalHookAuditAction::Failed {
                    reason: "timeout".to_string(),
                });
                bail!("external hook `{}` timed out", self.config.id);
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            self.record(ExternalHookAuditAction::Failed {
                reason: format!("exit status {}: {stderr}", output.status),
            });
            bail!(
                "external hook `{}` exited with {}",
                self.config.id,
                output.status
            );
        }

        let outcome: HookOutcome = match serde_json::from_slice(&output.stdout) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.record(ExternalHookAuditAction::Failed {
                    reason: format!("malformed HookOutcome JSON: {err}"),
                });
                return Err(err).with_context(|| {
                    format!(
                        "external hook `{}` returned malformed HookOutcome JSON",
                        self.config.id
                    )
                });
            }
        };
        self.record(ExternalHookAuditAction::Ran {
            outcome: outcome_name(&outcome).to_string(),
        });
        Ok(outcome)
    }

    fn record(&self, action: ExternalHookAuditAction) {
        let Some(log) = self.audit_log.as_ref() else {
            return;
        };
        log.record(ExtensionAuditEvent::ExternalHook(ExternalHookAuditEvent {
            extension_id: self.config.id.clone(),
            event: self.config.event.as_str().to_string(),
            command: self.config.command.clone(),
            action,
            occurred_at: Utc::now(),
        }));
    }
}

fn outcome_name(outcome: &HookOutcome) -> &'static str {
    match outcome {
        HookOutcome::Continue => "continue",
        HookOutcome::Block { .. } => "block",
        HookOutcome::ReplaceArgs(_) => "replace_args",
        HookOutcome::ReplaceResult(_) => "replace_result",
        HookOutcome::InjectMessages { .. } => "inject_messages",
        HookOutcome::RewriteMessages(_) => "rewrite_messages",
        HookOutcome::RewriteHistory(_) => "rewrite_history",
        HookOutcome::ForceContinue { .. } => "force_continue",
        HookOutcome::BlockInput { .. } => "block_input",
        HookOutcome::ReplaceInput(_) => "replace_input",
    }
}

#[async_trait::async_trait]
impl HookHandler for ExternalHookHandler {
    fn name(&self) -> &str {
        &self.config.id
    }

    fn priority(&self) -> i32 {
        self.config.priority
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if self.config.event != ExternalHookEvent::BeforeToolCall
            || !self.config.match_rule.matches_tool(tool)
        {
            return Ok(HookOutcome::Continue);
        }
        self.dispatch(
            ExternalHookEvent::BeforeToolCall,
            json!({ "tool": tool, "args": args }),
        )
        .await
    }

    async fn after_tool_call(
        &self,
        tool: &str,
        result: &ToolResult,
        _cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        if self.config.event != ExternalHookEvent::AfterToolCall
            || !self.config.match_rule.matches_tool(tool)
        {
            return Ok(HookOutcome::Continue);
        }
        self.dispatch(
            ExternalHookEvent::AfterToolCall,
            json!({ "tool": tool, "result": result }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::hooks::{HookRegistry, ToolCallDecision};

    #[tokio::test]
    async fn subprocess_hook_can_block_before_tool_call() {
        let hook = ExternalHookHandler::new(
            ExternalHookConfig {
                id: "block-bash".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "payload=$(cat); case \"$payload\" in *'\"event\":\"before_tool_call\"'*'\"tool\":\"bash\"'*) printf '%s' '{\"Block\":{\"reason\":\"no shell\"}}';; *) printf '%s' \"$payload\" >&2; exit 9;; esac".to_string(),
                ],
                enabled: true,
                policy: ExternalHookPolicy::Allow,
                ..Default::default()
            },
            None,
        )
        .unwrap();

        let outcome = hook
            .before_tool_call(
                "bash",
                &json!({ "command": "rm -rf target" }),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(matches!(outcome, HookOutcome::Block { reason } if reason == "no shell"));
    }

    #[tokio::test]
    async fn denied_policy_continues_and_records_audit() {
        let audit = Arc::new(ExtensionAuditLog::new(8));
        let hook = ExternalHookHandler::new(
            ExternalHookConfig {
                id: "denied-hook".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                enabled: true,
                policy: ExternalHookPolicy::Deny,
                ..Default::default()
            },
            Some(audit.clone()),
        )
        .unwrap();

        let outcome = hook
            .before_tool_call("bash", &json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));

        let recent = audit.recent(1);
        assert!(matches!(
            &recent[0],
            ExtensionAuditEvent::ExternalHook(ExternalHookAuditEvent {
                extension_id,
                action: ExternalHookAuditAction::Denied { .. },
                ..
            }) if extension_id == "denied-hook"
        ));
    }

    #[tokio::test]
    async fn require_approval_policy_continues_and_records_audit() {
        let audit = Arc::new(ExtensionAuditLog::new(8));
        let hook = ExternalHookHandler::new(
            ExternalHookConfig {
                id: "approval-hook".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                enabled: true,
                policy: ExternalHookPolicy::RequireApproval,
                ..Default::default()
            },
            Some(audit.clone()),
        )
        .unwrap();

        let outcome = hook
            .before_tool_call("bash", &json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));

        let recent = audit.recent(1);
        assert!(matches!(
            &recent[0],
            ExtensionAuditEvent::ExternalHook(ExternalHookAuditEvent {
                extension_id,
                action: ExternalHookAuditAction::ApprovalRequired { reason },
                ..
            }) if extension_id == "approval-hook" && reason.contains("requires approval")
        ));
    }

    #[tokio::test]
    async fn registry_turns_subprocess_failure_into_safe_continue() {
        let reg = HookRegistry::new();
        for hook in configured_handlers(
            &[ExternalHookConfig {
                id: "failing-hook".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "printf '%s' 'bad news' >&2; exit 17".to_string(),
                ],
                enabled: true,
                policy: ExternalHookPolicy::Allow,
                ..Default::default()
            }],
            None,
        ) {
            reg.register(hook);
        }

        let decision = reg
            .before_tool_call("bash", &json!({}), CancellationToken::new())
            .await;
        assert!(matches!(decision, ToolCallDecision::Continue));
        assert_eq!(reg.failure_metrics().errors, 1);
    }

    #[tokio::test]
    async fn registry_turns_subprocess_timeout_into_safe_continue() {
        let audit = Arc::new(ExtensionAuditLog::new(8));
        let reg = HookRegistry::new().with_audit_log(audit.clone());
        for hook in configured_handlers(
            &[ExternalHookConfig {
                id: "slow-hook".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "sleep 2".to_string()],
                enabled: true,
                policy: ExternalHookPolicy::Allow,
                timeout_secs: 1,
                ..Default::default()
            }],
            reg.audit_log(),
        ) {
            reg.register(hook);
        }

        let decision = reg
            .before_tool_call("bash", &json!({}), CancellationToken::new())
            .await;
        assert!(matches!(decision, ToolCallDecision::Continue));
        assert_eq!(reg.failure_metrics().errors, 1);

        let recent = audit.recent(1);
        assert!(matches!(
            &recent[0],
            ExtensionAuditEvent::ExternalHook(ExternalHookAuditEvent {
                extension_id,
                action: ExternalHookAuditAction::Failed { reason },
                ..
            }) if extension_id == "slow-hook" && reason == "timeout"
        ));
    }

    #[tokio::test]
    async fn registry_turns_malformed_subprocess_stdout_into_safe_continue() {
        let audit = Arc::new(ExtensionAuditLog::new(8));
        let reg = HookRegistry::new().with_audit_log(audit.clone());
        for hook in configured_handlers(
            &[ExternalHookConfig {
                id: "malformed-hook".to_string(),
                event: ExternalHookEvent::BeforeToolCall,
                match_rule: ExternalHookMatch {
                    tool: Some("bash".to_string()),
                },
                command: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "cat >/dev/null; printf '%s' 'not json'".to_string(),
                ],
                enabled: true,
                policy: ExternalHookPolicy::Allow,
                ..Default::default()
            }],
            reg.audit_log(),
        ) {
            reg.register(hook);
        }

        let decision = reg
            .before_tool_call("bash", &json!({}), CancellationToken::new())
            .await;
        assert!(matches!(decision, ToolCallDecision::Continue));
        assert_eq!(reg.failure_metrics().errors, 1);

        let recent = audit.recent(1);
        assert!(matches!(
            &recent[0],
            ExtensionAuditEvent::ExternalHook(ExternalHookAuditEvent {
                extension_id,
                action: ExternalHookAuditAction::Failed { reason },
                ..
            }) if extension_id == "malformed-hook" && reason.contains("malformed HookOutcome JSON")
        ));
    }
}
