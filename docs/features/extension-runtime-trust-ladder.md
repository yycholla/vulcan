---
title: Extension Runtime Trust Ladder
type: policy
created: 2026-05-05
tags: [extensions, runtime, trust, policy, wasm, mcp]
tracks: [611, 548, 264, 273, 276, 3, 268, 274]
---

# Extension Runtime Trust Ladder

Status: canonical repository copy of the policy from `~/wiki/queries/vulcan-extension-runtime-trust-ladder.md`.

Vulcan chooses extension/runtime boundaries by author trust and operational risk:

```text
trusted first-party/internal code -> native cargo-crate extensions
third-party policy-limited code    -> WASM / Wasmtime runtime
external tools and services        -> subprocess hooks or MCP bridge
heavy inference/deployment stacks  -> external backend endpoints
```

Native in-process Rust extensions are the first-party/internal path. They are not a sandbox boundary. Third-party extension code should target WASM/Wasmtime when it needs to run inside a Vulcan-managed runtime. External tools and services should cross a process or protocol boundary through subprocess hooks or MCP. Heavy inference and deployment platforms stay outside Vulcan as backend endpoints.

Generalized in-process JS/Python runtimes are deferred. If a JS/Python use case becomes concrete, the default boundary is a subprocess or WASM component model, not embedding a broad interpreter into the daemon.

## Runtime classes

| Runtime class | Intended authors | Trust boundary | Supported use | Deferred or denied |
|---|---|---|---|---|
| Native cargo-crate extension | Vulcan core, first-party, internal workspace code | Same process, trusted code | Core hooks, first-party tools, providers, frontend renderers, status widgets, trusted state | Third-party marketplace code; arbitrary native dynamic loading |
| WASM / Wasmtime extension | Third-party code with policy-limited host access | Memory-isolated runtime with explicit host imports | Third-party tools, hooks, state APIs, policy-limited UI events, controlled network/process/MCP capabilities | Direct host filesystem/network/process access without declared capabilities |
| Subprocess hook or adapter | User scripts, local tools, language-specific integrations | OS process boundary with stdin/stdout protocol, timeout, env/path policy | External hook handlers, CLI adapters, JS/Python/Ruby scripts, one-off local automation | Silent background daemons; unbounded process trees; unaudited mutation |
| MCP bridge | External tools and services exposing MCP | Protocol boundary plus server process/remote transport policy | Discover tools/resources/prompts and expose selected capabilities through Vulcan's tool registry | Sampling recursion, remote transport, or managed hosting before core MCP policy exists |
| External backend endpoint | Inference servers, deployment platforms, heavy service stacks | Network/API boundary | OpenAI-compatible model backends, BentoML/vLLM/Ollama/LiteLLM recipes | Embedding heavy serving stacks into the Vulcan daemon |

## Capability and audit requirements

All runtime classes must declare capabilities before activation. The minimum capability set is:

- `hooks`: which hook/event methods can observe, rewrite, block, or replace data.
- `tools`: tool names, replay-safety ceiling, mutation class, and approval requirements.
- `providers`: provider ids, defaults, model/endpoint fields, and whether instances are singleton or per-session.
- `ui`: frontend capability requirements such as text I/O, dialogs, widgets, canvas, raw keys, or ticks.
- `state`: extension-owned persistent state, branch policy, checkpointing, and cross-session writes.
- `filesystem`, `network`, `process`, `mcp`: external access scopes, if any.

Every activation decision must be auditable. The host must be able to answer:

- which extension ran;
- which runtime class it used;
- which capabilities it requested;
- which capabilities were allowed or denied;
- which resource limits applied;
- what user approval was required;
- what load, policy, timeout, or crash failure occurred.

Denied capabilities fail the extension operation and record an audit/status reason. They must not crash Vulcan or silently degrade into broader access.

## Registration surface policy

| Extension capability | Native first-party | WASM third-party | Subprocess | MCP bridge |
|---|---|---|---|---|
| Hooks/events | Yes | Yes, after policy wrappers | Yes, JSON event protocol | Bridge-observer only until MCP policy expands |
| Tools | Yes | Yes, capability-gated | Yes, via adapter | Yes, selected discovered tools |
| Providers | Yes | Later, only with explicit network/model policy | Prefer external endpoint adapter | No direct provider role initially |
| Frontend UI surfaces | Yes, via frontend extension API | Limited events/render hints after capability handshake | No direct UI ownership; can emit audited status | No direct UI ownership; can expose tool/resource metadata |
| Persistent state | Yes, through `ExtensionStateStore` | Yes, scoped and quota-limited | Host-owned state API only | Host-owned state/cache API only |
| Raw filesystem/network/process | Trusted code only | Deny by default; explicit host imports | Configured command/env/path policy | Server config and transport policy |

Native dynamic library loading through `dlopen`/`libloading` is not the marketplace path. It remains restricted to trusted internal/first-party experiments until signing, source trust, crash isolation, and update policy are mature.

## Existing issue mapping

- `#548` PRD: remains the in-process first-party extension design. Its native cargo-crate model is trusted/internal, not a third-party sandbox.
- `#264` extension ecosystem epic: treats this trust ladder as the parent policy for child specs.
- `#273` sandboxed extension runtimes: is the third-party runtime path and starts with WASM/Wasmtime plus explicit host capabilities.
- `#276` marketplace/discovery: must not install arbitrary native code as the default path. Marketplace records identify runtime class, checksum/signature, publisher trust, and required capabilities.
- `#3` external hook handlers: maps to subprocess hooks with JSON event payloads, timeout, stderr/log capture, command/env/path policy, and audit entries.
- `#268` MCP client bridge: maps to external tools/services through a protocol/process boundary. MCP tools are exposed only when explicitly configured and selected.
- `#274` managed MCP hosting: remains later than the stdio MCP bridge and inherits this policy before adding hosting, remote transport, or sampling.

## Consequences

- Extension docs and issues say "native first-party cargo-crate extension" when code runs in-process.
- Third-party extension language defaults to "WASM/Wasmtime runtime" unless the topic is specifically subprocess or MCP.
- Marketplace/store work requires runtime-class metadata and capability/audit metadata before install or activation.
- JS/Python extension requests start as subprocess hooks or WASM components. In-process interpreter embedding needs a separate accepted policy.
- Backend-serving technologies such as BentoML, vLLM, Ollama, LiteLLM, and llama.cpp stay as provider endpoints or deployment recipes unless a separate issue proves a native daemon need.
