# Provider Router Research

Status: research recommendation
Created: 2026-05-05
Tracks: [#575](https://github.com/yycholla/vulcan/issues/575)
Baseline: [provider capability matrix](./provider-capability-matrix.md)

## Recommendation

Do not adopt `litellm-rust` as a Vulcan provider adapter yet. Use it as reference material only.

Keep Vulcan's current OpenAI-compatible provider as the production baseline, and evaluate `genai` as the stronger future router candidate only after a conformance harness proves streaming, tool-call, usage, error, and model-naming parity against Vulcan's `LLMProvider` trait.

No implementation issue should be filed for a router adapter until the conformance harness exists. The first implementation slice should be "alternate provider behind a feature flag" only if it can pass the parity checks below for OpenAI-compatible, OpenRouter, Anthropic, Gemini, and xAI routes.

## Why

Vulcan's provider boundary is not just "send chat text." It owns:

| Vulcan requirement | Current code surface |
|---|---|
| Buffered chat and streaming chat | `LLMProvider::chat` and `LLMProvider::chat_stream` |
| Tool-call ids and JSON arguments | `ToolCall`, `ToolCallFunction`, `ToolDefinition` |
| Tool-result continuation | `Message::Tool` with `tool_call_id` |
| Thinking/reasoning passthrough | `Message::Assistant.reasoning_content`, `StreamEvent::Reasoning` |
| Streaming events for UI/gateway | `StreamEvent::{Text, Reasoning, Done, Error}` plus agent-emitted tool activity |
| Usage accounting | `Usage { prompt_tokens, completion_tokens, total_tokens }` |
| Retry behavior | `ProviderError::{Auth, RateLimited, ModelNotFound, BadRequest, ServerError, Network, Other}` |
| Provider config | `ProviderConfig` with `type`, `base_url`, `api_key`, `model`, `max_context`, retries, catalog controls, debug mode |

A router can help only if it preserves those semantics. Provider count alone is not enough.

## Candidate Matrix

| Capability | Current in-tree provider | `litellm-rust` 0.1.2 | `genai` 0.5.3 | `async-openai` 0.36.x |
|---|---|---|---|---|
| Primary fit | Production OpenAI-compatible and OpenAI Responses paths | Small Rust LiteLLM-style library | Mature Rust multi-provider client | OpenAI/OpenAI-compatible SDK, not multi-provider router |
| Provider coverage | OpenAI-compatible endpoints, OpenAI Responses | OpenAI-compatible, Anthropic, Gemini, xAI | OpenAI, Anthropic, Gemini, xAI, Ollama, Groq, DeepSeek, Cohere, Together, Fireworks, Nebius, Mimo, Zai, BigModel | OpenAI APIs; configurable enough for compatible endpoints |
| Chat | Native | Documented | Documented | Documented |
| Streaming | Native SSE normalization | Documented for OpenAI-compatible, Anthropic, xAI; Gemini streaming not documented | Documented provider-agnostic chat streams | Documented SSE streaming for OpenAI APIs |
| Tool calls | Native OpenAI shape with fallback parser | Not proven from current docs | Public chat module includes tool/tool-call/tool-response types | OpenAI tool types available, but no non-OpenAI native mapping |
| Tool-result continuation | Native `tool` role | Not proven | Public tool response type exists; adapter behavior still needs tests | OpenAI-compatible only |
| Structured output | Optional/catalog-aware, not auto-forced | Not proven | Public `JsonSpec` / response format support, provider support dependent | OpenAI structured-output types available |
| Reasoning controls/content | DeepSeek/OpenRouter reasoning passthrough exists | Not proven | Docs mention Gemini thinking and Anthropic reasoning-effort support | OpenAI reasoning/Responses support, not router semantics |
| Multimodal/files | Text-first in current provider | Images/video/embeddings documented for some providers | Binary/PDF/image/embedding support documented | Full OpenAI API surface |
| Usage accounting | `Usage` normalized when provider returns it | Cost via response headers documented; token usage shape not proven | Usage mapping documented across providers | OpenAI usage types |
| Retry/error taxonomy | Vulcan-owned deterministic mapping | Automatic retry documented; error mapping to Vulcan taxonomy not proven | Adapter errors need mapping | Retries rate limits except SSE; OpenAI errors only |
| Config mapping | Vulcan-owned `base_url`, `model`, env/config key, catalog controls | `provider/model` plus provider env vars | Model aliases, custom endpoint/auth/header resolver | OpenAI env/config and custom base/path support |
| Maturity signal | In-tree, tested in Vulcan | 0.1.2, active development, low docs coverage, small repository signal | Long release history, high docs coverage, broader repository signal | Mature OpenAI SDK, MIT |
| Adoption verdict | Keep | Copy patterns only; do not wrap yet | Best future research candidate; require harness first | Useful for OpenAI types/patterns, not a router replacement |

## `litellm-rust` Assessment

`litellm-rust` is close to the idea Vulcan wants: a small Rust library, `provider/model` routing, OpenAI-compatible/Anthropic/Gemini/xAI coverage, streaming, retries, cost headers, model registry data, and MIT licensing.

The blockers are concrete:

| Blocker | Impact on Vulcan |
|---|---|
| Public docs do not prove tool-call support | Vulcan's agent loop depends on tool-call ids, JSON args, and tool-result continuation. |
| Gemini streaming is not documented | Gemini cannot be considered parity with OpenAI/OpenRouter/xAI through this router. |
| Error taxonomy is not mapped to Vulcan's `ProviderError` | Retry and user-facing remediation would become less deterministic. |
| Cost tracking via headers is not enough | Vulcan needs token usage when available and harmless absence when unavailable. |
| 0.1.x active-development status | API instability is too high for a core runtime dependency. |
| Low documentation coverage | Integration risk would move from provider adapters into Vulcan's adapter layer. |

Verdict: copy its simple routing/config ideas, but do not add it as a dependency or feature-flagged provider yet.

Sources: [`litellm-rust` crate page](https://docs.rs/crate/litellm-rust/latest), [`litellm-rust` API docs](https://docs.rs/litellm-rust).

## `genai` Assessment

`genai` is more mature and currently looks like the better Rust-router candidate. It documents broad provider support, native protocol handling for Anthropic and Gemini, a unified streaming engine, binary/PDF/image support, model aliases, custom endpoint/auth/header overrides, usage mapping, structured-output types, reasoning controls, and tool-call/tool-response types in the public chat module.

The risk is still the same: docs are not a conformance test. Before adoption, Vulcan needs executable proof that `genai` preserves:

| Required proof | Why |
|---|---|
| Streaming event mapping | Vulcan's TUI/gateway need text, reasoning, finish, usage, and errors in stable order. |
| Tool-call chunks | Vulcan needs stable ids, names, arguments, and parallel-call behavior. |
| Tool response continuation | Native Anthropic/Gemini tool-response shapes must round-trip without corrupting history. |
| Error mapping | Vulcan must preserve deterministic retry behavior and user remediation messages. |
| Model naming | Saved runs and logs must show the actual provider/model route. |

Verdict: keep on the shortlist, but require a conformance harness before an adapter issue.

Sources: [`genai` crate page](https://docs.rs/crate/genai/latest), [`genai::chat` API docs](https://docs.rs/genai/latest/genai/chat/index.html).

## `async-openai` Assessment

`async-openai` is useful if Vulcan wants generated/typed OpenAI API coverage, especially Responses, Chat Completions, streaming, tools, files, embeddings, images, and configurable OpenAI-compatible clients. It is not a provider router. It does not solve Anthropic/Gemini native protocol translation, tool-result shape differences, or cross-provider usage/error normalization.

Verdict: reference for OpenAI-compatible implementation patterns or types, not for the Research & Tools router goal.

Source: [`async-openai` crate page](https://docs.rs/crate/async-openai/latest).

## Conformance Harness Needed Before Adoption

A router adapter should not be implemented until these tests can run against the current provider and any candidate adapter:

| Test group | Required cases |
|---|---|
| Buffered chat | Simple text response, system message, model-not-found, bad request. |
| Streaming chat | Text deltas, final response, interrupted stream, provider error mid-stream where supported. |
| Tool calls | Single call, parallel calls, invalid JSON args, tool-result continuation, model response after tool result. |
| Reasoning | Reasoning delta passthrough and assistant history replay for providers that require it. |
| Usage | Buffered usage, streaming final usage, missing usage. |
| Config | Provider/model naming, base URL override, API key source, catalog disabled, context override. |
| Retry | 429 with and without retry-after, 5xx, network failure, non-retryable auth/bad request. |

The harness should make the current in-tree provider the control. Candidate adapters only become implementation candidates if they match or intentionally improve the control behavior.

## Adoption Decision

| Decision option | Result |
|---|---|
| Adopt `litellm-rust` | No. Too early and tool/error parity is not proven. |
| Wrap `litellm-rust` partially | No. The likely wrapper would be mostly bespoke mapping code, removing the benefit. |
| Copy patterns from `litellm-rust` | Yes. Provider/model routing, registry metadata, and small unified request ideas are useful. |
| Use `genai` now | No. More promising, but still needs harness proof before core runtime adoption. |
| Keep current provider trait | Yes. The trait encodes Vulcan-specific semantics that routers have not proven. |

## Follow-Up

Do not open a provider-router implementation issue from this research alone.

The next useful issue would be a provider conformance harness that runs the current in-tree provider and any future candidate adapter through the same behavioral fixtures. After that harness exists, a narrow adapter issue can be scoped to one of:

1. `genai` experimental adapter behind a feature flag.
2. `litellm-rust` re-evaluation if tool-call and Gemini streaming support become documented and testable.
3. Direct native Anthropic/Gemini adapters if router libraries still fail parity.
