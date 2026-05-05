# Provider Capability Matrix

Status: research reference
Created: 2026-05-05
Tracks: [#610](https://github.com/yycholla/vulcan/issues/610)
Feeds: [#575](https://github.com/yycholla/vulcan/issues/575), [#612](https://github.com/yycholla/vulcan/issues/612)

This matrix defines the minimum provider behavior Vulcan needs before it adopts a routed provider abstraction such as `litellm-rust`, `genai`, or a native provider adapter. It is not an adapter design and does not replace Vulcan's current provider trait.

Local inference hosting, BentoML/BentoCloud, server deployment, GPUs, Docker, systemd, and operator recipes are out of scope here. Remote API behavior is in scope, including remote OpenAI-compatible endpoints supplied by users or operators.

## Vulcan Core Parity Set

The core agent loop needs these capabilities before a provider path can be considered first-class:

| Requirement | Why Vulcan needs it | Current Vulcan shape |
|---|---|---|
| Chat request/response | Every turn is a conversation with system, user, assistant, and tool-result history. | `Message` enum in `src/provider/mod.rs` maps to OpenAI-compatible message roles. |
| Streaming | TUI, CLI, and gateway users need incremental text, finish events, and eventual usage when available. | `ChatStream` sends normalized `StreamEvent` values through a bounded channel. |
| Tool/function calls | The agent loop depends on stable tool ids, JSON arguments, and continuation after tool results. | OpenAI-compatible tool calls are parsed into `ToolCall`; fallback parsing exists for weaker endpoints. |
| Provider/model naming | Config must identify both the provider route and concrete model slug without hard-coded aliases. | `ProviderConfig` carries `provider_type`, `base_url`, `model`, and catalog controls. |
| Output and context limits | The loop needs bounded requests and useful error messages when model limits are wrong. | `max_output_tokens`, `max_context`, and optional provider catalog metadata exist. |
| Retry/error taxonomy | Retrying auth or bad request failures wastes budget; retrying rate limits, 5xx, and network failures is useful. | `ProviderError` classifies auth, rate limit, model-not-found, bad request, server, network, and other errors. |
| Usage accounting | Costs, rate limits, and future routing need best-effort prompt/completion totals. | Usage is provider-dependent today and should remain best effort when missing from streams. |

Optional capabilities are valuable but must not block the core loop:

| Optional capability | Adoption rule |
|---|---|
| Structured output / JSON schema | Use when the provider or model supports it natively; otherwise keep it as model-specific adapter glue. |
| Reasoning controls / reasoning content | Preserve fields when present, but do not require them for non-reasoning models. |
| File attachments and media | Treat as future multimodal surface; do not force it into the text/tool loop prematurely. |
| Embeddings | Useful for knowledge features, but not required for chat/tool parity. |
| Cost accounting | Use provider-reported cost fields or external pricing tables only as advisory metadata. |

Legend:

| Value | Meaning |
|---|---|
| Native | Provider API exposes the capability directly. |
| Adapter glue | Capability exists, but Vulcan or a router must translate protocol shapes. |
| Optional | Available for some models, routes, or request modes. |
| Defer | Not needed for the core loop or not mature enough to depend on. |
| Unknown | Not proven from current primary docs. |

## Capability Matrix

| Provider path | Protocol / endpoint | Chat | Streaming | Tool calls | Structured output / JSON | Files / media | Usage and cost | Error / retry taxonomy | Provider and model naming | Vulcan guidance |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|
| Vulcan OpenAI-compatible provider | OpenAI-style `/chat/completions` at configurable `base_url` | Native | Native | Native when endpoint supports OpenAI tool shape; fallback parser otherwise | Optional catalog metadata; not forced today | Optional; text-first today | Optional provider response fields | Native `ProviderError` mapping | `provider.type = "openai-compat"`, explicit `base_url`, `model` | Keep as baseline control implementation. |
| OpenAI | Chat Completions and Responses APIs | Native | Native SSE | Native `tools` / `tool_choice` | Native structured outputs and JSON mode on supported models | Optional image/audio/file surfaces by API/model | Native usage fields, including stream usage options | Native HTTP errors mapped by Vulcan | OpenAI model ids | Highest parity; keep direct support. |
| OpenRouter | OpenAI-compatible `/api/v1/chat/completions` router | Native | Native | Adapter glue from OpenAI-shaped `tools` to routed provider | Optional by model; router exposes structured-output parameter support | Optional by model and modality | Native usage fields; cost/rate details vary by routed provider | Adapter glue; errors may wrap upstream provider errors | Namespaced slugs such as `openai/gpt-4` | Good routed remote provider; require capability probing/model metadata before assuming parity. |
| Anthropic | Native Messages API | Native | Native SSE with Anthropic event names | Adapter glue; tools and tool results are content blocks, not OpenAI `tool` role messages | Adapter glue; structured JSON commonly modeled through tool/schema patterns | Optional images and PDFs in message content | Native usage fields and token-counting endpoint | Native 400/401/403/404/413/429/500/529 taxonomy | Anthropic model ids such as `claude-*` | Use through OpenAI-compatible routers today; native adapter must prove stream/tool mapping. |
| Gemini | Native `generateContent` API | Native | Native incremental `GenerateContentResponse` stream | Adapter glue; function calls are Gemini content parts | Native structured outputs with JSON schema subset | Native file/media upload and multimodal parts | Native `usageMetadata` / token APIs | Adapter glue; map Google errors into `ProviderError` | Gemini model ids such as `gemini-*` | Native adapter possible, but requires role/content/tool/media normalization. |
| xAI / Grok | OpenAI-compatible `/v1/chat/completions` and Responses API | Native | Native with `stream: true` | Native function calling; streaming returns whole function call chunks | Native structured outputs on language models; tools plus structured output on supported Grok 4 family models | Optional image understanding/files APIs | Native usage object, cost field, and rate-limit docs | Native 400/422/429 plus endpoint status docs | xAI model ids such as `grok-*`; OpenAI SDK `baseURL` works | High parity for OpenAI-compatible route; verify model-specific reasoning and structured-output fields. |
| Remote OpenAI-compatible endpoint | Operator-provided OpenAI-style endpoint | Adapter glue | Optional | Optional | Optional | Optional | Optional | Adapter glue | Explicit `base_url`, `model`, often `disable_catalog = true` | Supported as best-effort; require manual config and do not assume tool/usage/catalog parity. |
| `litellm-rust` | Rust crate exposing LiteLLM-style routing | Adapter glue | Native streaming section exists | Unknown from current docs | Unknown from current docs | Unknown from current docs | Unknown from current docs | Unknown | Provider/model routing likely adapter-owned | Evaluate in #575 only after streaming, tool-call, and error semantics are proven in tests. |
| `genai` | Rust multi-provider library | Adapter glue | Native chat streaming across several providers | Defer; crate docs say function calling is future work | Optional/future demos exist, but not parity-ready | Optional image/PDF/embedding support | Adapter glue; docs map usage across providers | Adapter glue | Provider aliases, target resolver, model mapper | Strong research candidate, but function-call gap blocks first-class agent-loop adoption today. |

## Provider Notes

### OpenAI

OpenAI is the control case for the current Vulcan provider shape. The Chat Completions API exposes an OpenAI-compatible message list, streaming, `tools`, `tool_choice`, response formats, usage fields, and model ids. Structured outputs are available through function calling or `response_format` / JSON schema depending on the API mode and model.

Sources: [Chat API reference](https://platform.openai.com/docs/api-reference/chat?api-mode=chat), [Structured outputs guide](https://platform.openai.com/docs/guides/structured-outputs?api-mode=chat).

### OpenRouter

OpenRouter is the strongest remote routed-provider fit because it intentionally accepts OpenAI-style chat-completion requests. Its docs cover `/api/v1/chat/completions`, streaming and non-streaming modes, usage fields, tool parameters, structured outputs, provider routing, and namespaced model slugs. The risk is not request shape; the risk is model/provider variance behind the route.

Vulcan should treat OpenRouter capabilities as model-scoped, not provider-global. A model catalog or probe must decide whether tool calls, structured outputs, modalities, and cost data are available.

Sources: [chat completion API](https://openrouter.ai/docs/api/api-reference/chat/send-chat-completion-request), [parameters](https://openrouter.ai/docs/api/reference/parameters), [structured outputs](https://openrouter.ai/docs/features/structured-outputs), [tool calling](https://openrouter.ai/docs/features/tool-calling).

### Anthropic

Anthropic's Messages API is semantically close to Vulcan's loop but not wire-compatible with OpenAI. Streaming uses named SSE events such as message/content-block events. Tool use is expressed as `tool_use` and `tool_result` content blocks within user and assistant messages rather than a separate OpenAI-style `tool` role. Errors include 429 rate limits and 529 overloaded errors; rate-limit responses can include `retry-after`.

A native Anthropic adapter should be scoped around exact stream and tool-result normalization. Until then, Anthropic works best through a router that already performs OpenAI compatibility translation.

Sources: [Messages API guide](https://docs.anthropic.com/en/api/messages-examples), [streaming guide](https://docs.anthropic.com/en/api/messages-streaming), [tool use guide](https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/implement-tool-use), [errors](https://docs.anthropic.com/en/api/errors), [rate limits](https://docs.anthropic.com/en/api/rate-limits).

### Gemini

Gemini has native chat/generation, streaming, function calling, structured output, files, media, and token APIs. It is not OpenAI wire-compatible: messages are `contents`, tools and function responses are content parts, structured output uses Gemini generation config, and file/media references use Gemini-specific APIs.

Gemini is a good native-adapter candidate only after Vulcan defines a provider-neutral representation for multimodal content and function-call continuations. For #575, the router must prove it can preserve Gemini tool and stream semantics rather than flattening them into lossy text.

Sources: [text generation](https://ai.google.dev/gemini-api/docs/text-generation), [function calling](https://ai.google.dev/gemini-api/docs/function-calling), [structured output](https://ai.google.dev/gemini-api/docs/structured-output), [Files API](https://ai.google.dev/api/files), [token API](https://ai.google.dev/api/tokens).

### xAI / Grok

xAI exposes an OpenAI-compatible chat-completions endpoint at `https://api.x.ai/v1` and documents chat, streaming, function calling, image understanding, structured outputs, usage, cost, and rate limits. Function-call streaming has an important detail: the function call is returned whole in a single chunk, not as incrementally streamed arguments.

That makes xAI high-parity for Vulcan's current OpenAI-compatible provider path, but tests should cover reasoning fields, whole-chunk tool calls, and `usage`/cost mapping.

Sources: [chat API reference](https://docs.x.ai/docs/api-reference), [streaming](https://docs.x.ai/docs/guides/streaming-response), [function calling](https://docs.x.ai/developers/tools/function-calling), [structured outputs](https://docs.x.ai/developers/model-capabilities/text/structured-outputs), [usage and rate limits](https://docs.x.ai/developers/rate-limits), [debugging errors](https://docs.x.ai/developers/debugging).

### Remote OpenAI-Compatible Endpoints

Remote OpenAI-compatible endpoints are useful because Vulcan already supports configurable `base_url`, `api_key`, `model`, catalog disabling, and context/output-token limits. They should remain best-effort. Operators may supply endpoints that implement chat but not streaming, tools, structured output, or usage reporting.

Default policy:

| Capability | Remote endpoint policy |
|---|---|
| Chat | Required. |
| Streaming | Required for good TUI experience, but may fall back to buffered only if explicitly configured later. |
| Tool calls | Probe or document manually; do not assume. |
| Structured output | Probe or document manually; do not assume. |
| Usage | Best effort; absence should not break chat. |
| Catalog/model metadata | Usually manual; `disable_catalog = true` is expected for many endpoints. |

### `litellm-rust`

`litellm-rust` remains a candidate adapter, not a provider-layer replacement. The current docs.rs page is early-stage and exposes sections for quick start, streaming, and supported providers, but it does not establish the full Vulcan parity set from docs alone.

#575 should therefore evaluate it with executable fixtures:

| Fixture | Required proof |
|---|---|
| Streaming text | Chunks map cleanly to `StreamEvent::TextDelta` and finish events. |
| Streaming tool call | Tool id, name, and JSON arguments are preserved, including partial or whole-chunk behavior. |
| Tool result continuation | A tool result can be sent back without provider-specific history corruption. |
| Errors | 401/403, 404 model, 400 bad request, 429, 5xx, and network failures map to `ProviderError`. |
| Usage | Buffered and streaming usage is surfaced when available and absent when unavailable. |
| Model naming | Provider/model routing does not hide the actual upstream model slug from logs and run records. |

Source: [`litellm-rust` docs.rs](https://docs.rs/litellm-rust).

### `genai`

`genai` is a serious Rust-router candidate because it documents native support for OpenAI, Anthropic, Gemini, xAI, and other providers; chat streaming; target resolvers; image/PDF/embedding support; model aliases; and usage mapping. It also documents a current limitation: function calling is listed as future work. That gap blocks first-class adoption for Vulcan's core agent loop today.

Source: [`genai` docs.rs](https://docs.rs/crate/genai/0.5.3).

## Implications For Follow-Up Issues

### #575: Research native provider router via `litellm-rust` / Rust LiteLLM ports

#575 should not ask "can a router send a chat request?" It should ask whether a router preserves Vulcan's provider semantics under stress.

Required #575 output:

| Decision input | Minimum evidence |
|---|---|
| Streaming parity | Text, tool-call, finish, error, and usage event behavior across OpenAI, OpenRouter, Anthropic, Gemini, xAI, and a remote OpenAI-compatible route. |
| Tool-call parity | Stable ids, JSON arguments, parallel call behavior, tool-result continuation, and bad-request recovery. |
| Error parity | Concrete mapping into Vulcan's `ProviderError` variants. |
| Model naming | Clear provider/model naming in config, logs, saved runs, and user-facing errors. |
| Adoption shape | Feature-flagged optional adapter if parity is good; no replacement of the provider trait until proven. |

### #612: External inference backend recipes

#612 should consume this matrix and document remote API recipes only. It should not evaluate server hosting or deployment operations. For any recipe, include a small compatibility note:

| Recipe field | Required content |
|---|---|
| Endpoint shape | Chat endpoint path and whether it is OpenAI-compatible. |
| Config | `provider.type`, `base_url`, `model`, API key source, catalog setting. |
| Capabilities | Streaming, tool calls, structured output, media, usage. |
| Caveats | Missing capabilities or model-specific behavior. |

## Decision Rule

Do not replace Vulcan's provider trait or current OpenAI-compatible provider until a candidate adapter proves:

1. Streaming text and tool events are lossless enough for the TUI, CLI, and gateway.
2. Tool-call ids, arguments, and results survive multi-turn loops.
3. Error mapping preserves retry behavior.
4. Usage accounting is surfaced when available and harmless when absent.
5. Provider/model naming remains explicit to users and saved run records.

Until then, routed provider libraries belong behind optional adapters or research spikes.
