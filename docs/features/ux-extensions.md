---
title: UX & Interaction Surface
type: feature
created: 2026-05-14
tags: [extensions, ui, tui, human-in-the-loop]
---

# UX & Interaction Surface

Bring extensions into the human loop with rich interfaces, approvals, and visibility.

## In-Agent UI Extensions

Extensions can render interactive terminal UI panels so users can inspect state and intervene.

- **Panels**: File trees, memory summaries, active goals, recent tool calls.
- **Dialogs**: Confirmations, forms, pickers.
- **Progress & Logs**: Live logs per extension or per long-running tool.
- **Diff views**: Side-by-side file or state diffs before applying changes.

## Web Dashboard

Optional companion web UI (served locally or remotely) to manage extensions.

- **Inventory**: Installed extensions, versions, publishers, and signatures.
- **Config editor**: Schema-driven forms for extension configuration.
- **Usage & Logs**: Per-extension logs, token usage, event counts.
- **Enable/disable & updates**: One-click enable/disable, version pinning, updates.

## Extension Chat

A dedicated channel where extensions can communicate naturally with the user during sessions.

- `[@RedisMemory] Loaded 12 facts for session X`
- `[@DeployCheck] Required checks passed: lint, tests, review`
- Extensions can ask clarifying questions or report warnings inline.

## Approval Workflow Hooks

Extensions that intercept dangerous operations and require explicit consent.

| Level | Behavior |
|-------|----------|
| Soft  | Log and continue; notify user (non-blocking) |
| Warn  | Notify and continue unless threshold exceeded |
| Block | Stop execution until explicit approval (MFA optional) |

Use cases: production deploys, bulk deletes, spending over budget, changing firewall rules.

---

## Example: Approval Extension Snippet

```rust
pub struct ApprovalGate {
    policy: ApprovalPolicy,
}

impl Extension for ApprovalGate {
    fn capabilities(&self) -> &[Capability] {
        &[Capability::EventHandler("tool_call".into())]
    }

    fn initialize(&self, ctx: &ExtensionContext) -> Result<()> {
        ctx.register_event_handler(|event| match event {
            Event::BeforeToolCall { tool, args } if self.policy.covers(tool) => {
                if self.requires_approval(tool, args) {
                    request_approval(tool, args).await?;
                }
            }
            _ => {}
        });
        Ok(())
    }
}
```
