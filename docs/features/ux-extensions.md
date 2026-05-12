---
title: UX & Interaction Surface
type: feature
status: proposed
phase: Phase 3 planning spec
created: 2026-05-08
updated: 2026-05-08
tracking: GitHub #272 plus #313/#560 for learning/dashboard/UI surfaces; Linear YYC-172 extension UX reference from issue audit
tags: [extensions, ui, tui, human-in-the-loop]
---

# UX & Interaction Surface

## Status

| Field | Value |
|---|---|
| Status | Proposed Phase 3 spec |
| Current implementation state | foundation only: TUI, approval pauses, logs, and frontend extension slices exist; extension management views and companion web dashboard are proposed |
| Tracking | GitHub #272 plus #313/#560 for learning/dashboard/UI surfaces; Linear YYC-172 extension UX reference from issue audit |
| Dependencies / non-goals | Extension lifecycle (#556), UI/frontend extension surface (#555/#559/#560), and governance (#269). This document does not claim the proposed behavior is currently available. |

> Language note: sections below describe the target design. Unless the status table explicitly calls out a shipped foundation, read capability statements as proposed behavior.


Bring extensions into the human loop with rich interfaces, approvals, and visibility.

## In-Agent UI Extensions

Proposed extensions would be able to render interactive terminal UI panels so users can inspect state and intervene.

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

A proposed dedicated channel would let extensions communicate naturally with the user during sessions.

- `[@RedisMemory] Loaded 12 facts for session X`
- `[@DeployCheck] Required checks passed: lint, tests, review`
- Proposed extensions could ask clarifying questions or report warnings inline.

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
