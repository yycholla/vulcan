# External Inference Endpoint Recipes

Status: research reference
Created: 2026-05-05
Tracks: [#612](https://github.com/yycholla/vulcan/issues/612)
Depends on: [provider capability matrix](./provider-capability-matrix.md), [#610](https://github.com/yycholla/vulcan/issues/610)
Out of scope: [#576 BentoML/BentoCloud research](https://github.com/yycholla/vulcan/issues/576) is closed and remains out of scope for Vulcan provider recipes.

Vulcan should call inference systems through network APIs. It should not host, deploy, supervise, package, or tune llama.cpp, vLLM, Ollama, LiteLLM proxy, BentoML, or any other inference server. The recipes below assume an operator has already exposed a reachable HTTPS endpoint.

## Baseline OpenAI-Compatible Pattern

Use this pattern when the remote service exposes an OpenAI-compatible `/v1/chat/completions` API.

```toml
[provider]
type = "openai-compat"
base_url = "https://inference.example.com/v1"
model = "operator/model-or-alias"
max_context = 32768
max_retries = 4
disable_catalog = true
# api_key = "do-not-commit-real-secrets"
# max_output_tokens = 4096
```

Secret handling:

| Method | Use when | Notes |
|---|---|---|
| `VULCAN_API_KEY` | One active provider key for the current shell/session | Environment value wins over config. |
| `provider.api_key` | Local private config file is acceptable | Do not commit real keys. |
| `provider.auth_source = "codex"` | Reusing local Codex auth for a compatible endpoint | Only works for auth sources implemented by Vulcan. |

Vulcan always sends `Authorization: Bearer <key>` when a key is available. Some local-style OpenAI-compatible services ignore the key but still require a placeholder for OpenAI SDK compatibility; use an operator-provided value or a harmless placeholder only when the service documents that behavior.

`disable_catalog = true` is the conservative default for operator-managed endpoints because `/v1/models` may be missing, incomplete, or not aligned with the model aliases users should configure. If the endpoint has a reliable model catalog, it can be set to `false`.

Vulcan's provider HTTP client currently uses a fixed long request timeout internally; there is no provider-level `timeout` config knob. Use `max_retries`, endpoint health checks, and operator-side timeouts for reliability until a first-class timeout setting exists.

## Endpoint Recipes

| Endpoint family | Base URL shape | Auth | Model naming | Streaming | Tool calls | Usage / cost | Main caveat |
|---|---|---|---|---|---|---|---|
| llama.cpp `llama-server` | `https://host.example/v1` | Optional bearer key if operator enabled it | Server alias or configured model name | Supported on chat completions | Documented as supported, but model/template dependent | Usage varies by server/model | Treat tool quality and JSON mode as endpoint-specific. |
| vLLM OpenAI-compatible server | `https://host.example/v1` | Operator policy, usually bearer key | Served model name or `--served-model-name` alias | Supported on chat completions | Supported, with model/template parser caveats | Usage fields expected, cost external | `tool_choice = "required"` may not be supported by all versions. |
| Ollama OpenAI compatibility | `https://host.example/v1` | Often placeholder or ignored key; operator may enforce auth at proxy | Ollama model tag such as `qwen2.5-coder:latest` | Supported | Supported by documented OpenAI-compatible fields | Usage may differ by model/path | Compatibility is partial and model-local; disable catalog unless operator proves it. |
| LiteLLM proxy | `https://gateway.example` or `https://gateway.example/v1` depending on operator route | Bearer key or virtual key | Proxy model alias, often provider-prefixed behind the proxy | OpenAI-format streaming | Router translates provider tool behavior | Proxy can track spend/usage | Adds a trust boundary; errors and capabilities may reflect upstream provider plus proxy policy. |
| OpenAI | `https://api.openai.com/v1` | `VULCAN_API_KEY` or config key | OpenAI model id | Supported | Supported | Native usage | Highest OpenAI-compatible baseline. |
| OpenRouter | `https://openrouter.ai/api/v1` | OpenRouter API key | Namespaced model slug such as `anthropic/claude-sonnet-4.5` | Supported | Routed and model-dependent | Native usage; cost varies by route | Capability checks must be model-scoped. |
| Anthropic | Native Messages API | `x-api-key` in native API | Anthropic model id | Supported natively | Native content-block tools | Native usage | Not OpenAI wire-compatible without a router or future native adapter. |
| Gemini | Native Gemini API | Google API key | Gemini model id | Supported natively | Native function calling | Native usage metadata | Not OpenAI wire-compatible without a router or future native adapter. |
| xAI / Grok | `https://api.x.ai/v1` | xAI API key | xAI model id | Supported | Supported; function call chunks have documented whole-call behavior | Native usage and cost fields | Verify reasoning/model-specific structured-output behavior. |

## Minimal Config Examples

### Remote llama.cpp `llama-server`

```toml
[provider]
type = "openai-compat"
base_url = "https://llama.example.com/v1"
model = "gpt-4"
max_context = 8192
max_retries = 4
disable_catalog = true
# api_key = "operator-issued-key"
```

Notes:

| Area | Guidance |
|---|---|
| Endpoint | Use the OpenAI-compatible `/v1/chat/completions` surface, not the raw `/completion` endpoint. |
| Auth | Operators can enable bearer API keys; otherwise the endpoint may accept no key or a placeholder. |
| Model | Prefer an operator-defined alias. llama.cpp can expose aliases, so the configured name may not match the GGUF filename. |
| Streaming | Supported by the chat endpoint; verify long streams through the actual reverse proxy. |
| Tools / JSON | The server documents function calling and JSON mode, but behavior depends on model, chat template, and operator settings. |
| Reliability | Loading models can surface as temporary unavailable responses; health checks belong to the operator, not Vulcan. |

Source: [llama.cpp server docs](https://www.mintlify.com/ggml-org/llama.cpp/inference/server).

### Remote vLLM

```toml
[provider]
type = "openai-compat"
base_url = "https://vllm.example.com/v1"
model = "meta-llama/Llama-3.1-70B-Instruct"
max_context = 131072
max_retries = 4
disable_catalog = true
```

Notes:

| Area | Guidance |
|---|---|
| Endpoint | Use the OpenAI-compatible Chat Completions API. |
| Auth | Follow the operator's gateway policy; Vulcan sends bearer auth if configured. |
| Model | Use the served model name or alias exposed by the vLLM operator. |
| Streaming | Supported by the OpenAI-compatible server. |
| Tools | vLLM documents named function calling plus `auto` and `none`; `required` support is version-dependent and should not be assumed. |
| Reliability | Great for high-throughput remote serving, but latency and queueing depend on the operator's batching/load policy. |

Source: [vLLM OpenAI-compatible server docs](https://docs.vllm.ai/en/latest/serving/openai_compatible_server/).

### Remote Ollama

```toml
[provider]
type = "openai-compat"
base_url = "https://ollama.example.com/v1"
api_key = "ollama"
model = "qwen2.5-coder:latest"
max_context = 32768
max_retries = 2
disable_catalog = true
```

Notes:

| Area | Guidance |
|---|---|
| Endpoint | Use Ollama's OpenAI-compatible `/v1/chat/completions` surface. |
| Auth | Ollama's OpenAI compatibility examples use a required-but-ignored key; remote operators may still enforce real auth at a proxy. |
| Model | Use the exact model tag the operator has pulled and exposed. |
| Streaming | Supported by documented OpenAI-compatible fields. |
| Tools / JSON | Ollama documents tools, JSON mode, vision, and stream usage fields for chat completions, but behavior remains model-dependent. |
| Reliability | Treat as a remote service despite the local-first docs; reverse proxy timeouts and model load delays matter. |

Source: [Ollama OpenAI compatibility docs](https://docs.ollama.com/openai).

### Remote LiteLLM Proxy

```toml
[provider]
type = "openai-compat"
base_url = "https://litellm.example.com"
model = "research-fast"
max_context = 128000
max_retries = 4
disable_catalog = true
# api_key = "sk-virtual-key-issued-by-proxy"
```

Notes:

| Area | Guidance |
|---|---|
| Endpoint | Operators may expose the proxy at the root or behind `/v1`; configure the `base_url` exactly as provided. |
| Auth | Prefer proxy-issued virtual keys over raw upstream provider keys. |
| Model | Use the proxy model alias, not necessarily the upstream provider's model slug. |
| Streaming | LiteLLM documents OpenAI-format streaming chunks. |
| Tools | LiteLLM translates across providers; verify that the chosen model route preserves tool-call ids, arguments, and continuation behavior. |
| Errors | LiteLLM maps exceptions to OpenAI-compatible error classes, but Vulcan still sees proxy policy plus upstream failures. |
| Reliability | Proxy fallback/retry may interact with Vulcan retries; avoid double-retry storms on 429/5xx. |

Source: [LiteLLM docs](https://docs.litellm.ai/).

### Hosted Provider APIs

Use hosted providers directly when Vulcan already supports their wire protocol. Today that means OpenAI-compatible APIs through `type = "openai-compat"` or OpenAI Responses through `type = "openai-responses"` where configured. Native Anthropic and Gemini APIs require a future adapter or a router.

```toml
[provider]
type = "openai-compat"
base_url = "https://openrouter.ai/api/v1"
model = "openai/gpt-4.1"
max_context = 128000
max_retries = 4
disable_catalog = false
```

Notes:

| Provider | Direct config pattern | Caveat |
|---|---|---|
| OpenAI | `base_url = "https://api.openai.com/v1"`, OpenAI model id | Best baseline for OpenAI-compatible semantics. |
| OpenRouter | `base_url = "https://openrouter.ai/api/v1"`, namespaced model slug | Capabilities and pricing are model/provider route-specific. |
| xAI | `base_url = "https://api.x.ai/v1"`, xAI model id | High OpenAI-compatible parity; verify whole-chunk function-call streaming. |
| Anthropic | Use through OpenRouter/LiteLLM until native adapter exists | Native Messages API is not OpenAI wire-compatible. |
| Gemini | Use through OpenRouter/LiteLLM until native adapter exists | Native Gemini API is not OpenAI wire-compatible. |

Sources: [OpenAI Chat API](https://platform.openai.com/docs/api-reference/chat?api-mode=chat), [OpenRouter chat API](https://openrouter.ai/docs/api/api-reference/chat/send-chat-completion-request), [Anthropic Messages API](https://docs.anthropic.com/en/api/messages-examples), [Gemini text generation](https://ai.google.dev/gemini-api/docs/text-generation), [xAI chat API](https://docs.x.ai/docs/api-reference).

## Performance And Reliability Caveats

| Caveat | What to document per endpoint | Vulcan behavior |
|---|---|---|
| Latency | Region, queueing, batching, model load time, reverse proxy timeout | User sees delayed first token or request failure; retries only help transient failures. |
| Streaming stability | Whether streams survive proxy buffering, idle timeouts, and long tool arguments | TUI quality depends on incremental chunks; broken streams should be treated as provider/network failures. |
| Context limits | Actual context window for the configured model/alias | Set `max_context` manually when catalog data is absent or untrusted. |
| Output limits | Maximum useful `max_tokens` / output token budget | Set `max_output_tokens` for small-context or heavily queued endpoints. |
| Tool-call support | Tool schema support, `tool_choice` support, parallel tool calls, and whether arguments stream partially or whole | Do not assume tool parity from OpenAI-compatible branding. |
| Usage reporting | Whether buffered and streamed responses include prompt/completion totals | Missing usage should not break chat; cost reporting remains best effort. |
| Rate limits | 429 shape, `Retry-After` header, proxy retry/fallback behavior | Vulcan retries 429, 5xx, and network errors within `max_retries`. |
| Error shape | Whether errors follow OpenAI `error.message` shape or provider-specific bodies | Unrecognized shapes become `ProviderError::Other` with truncated body text. |
| Catalog reliability | Whether `/v1/models` exists and returns usable context metadata | Use `disable_catalog = true` unless the operator promises useful catalog data. |

## Endpoint Readiness Checklist

Before recommending a remote endpoint to Vulcan users, run a manual smoke test outside Vulcan and record:

1. Base URL ending in the correct API prefix.
2. Model name or alias accepted by `/v1/chat/completions`.
3. Whether bearer auth is required and which secret source should supply it.
4. Buffered chat request succeeds.
5. Streaming chat request yields incremental chunks.
6. Tool-call request either works or is documented as unsupported.
7. JSON mode / structured output either works or is documented as unsupported.
8. Usage object is present, absent, or only present in buffered responses.
9. Rate-limit and bad-request errors produce readable messages.
10. `disable_catalog` decision is documented.

If any item is unknown, document it as unknown rather than implying OpenAI-compatible means OpenAI-complete.
