<!-- generated-by: gsd-doc-writer -->
# Configuration

## Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `VULCAN_API_KEY` | Required for providers that need an API key unless configured in provider files | None | API key fallback for the active provider. |
| `VULCAN_PLATFORM` | Optional | Set by gateway shell command execution | Platform name passed to configured gateway shell commands. |
| `VULCAN_CHAT_ID` | Optional | Set by gateway shell command execution | Chat or lane id passed to configured gateway shell commands. |
| `VULCAN_USER_ID` | Optional | Set by gateway shell command execution | User id passed to configured gateway shell commands. |

## Config File Format

The primary config file is `~/.vulcan/config.toml`. `config.example.toml` documents the main sections:

```toml
[provider]
type = "openai-compat"
base_url = "https://openrouter.ai/api/v1"
model = "deepseek/deepseek-v4-flash"

[tools]
yolo_mode = false
native_enforcement = "block"

[tui]
theme = "system"
```

Provider fragments can also live in `~/.vulcan/providers.toml`, and key bindings can be split during migration from the monolithic config.

## Required vs Optional Settings

| Setting | Required When | Source |
|---------|---------------|--------|
| `provider.api_key` or `VULCAN_API_KEY` | The active provider requires authenticated requests | `config.example.toml`, `src/config_registry.rs` |
| `gateway.api_token` | Running `vulcan gateway run` | `src/gateway/mod.rs`, `src/cli_gateway.rs` |
| `gateway.discord.bot_token` | `gateway.discord.enabled = true` | `src/gateway/discord.rs` |
| `gateway.telegram.bot_token` | `gateway.telegram.enabled = true` | `src/gateway/telegram.rs` |

Most other settings have Rust defaults in `src/config/mod.rs` or TOML examples in `config.example.toml`.

## Defaults

| Setting | Default | Source |
|---------|---------|--------|
| `provider.type` | `openai-compat` | `config.example.toml` |
| `provider.base_url` | `https://openrouter.ai/api/v1` in the example config | `config.example.toml` |
| `provider.max_context` | `128000` in the example config | `config.example.toml` |
| `tools.yolo_mode` | `false` in the example config | `config.example.toml` |
| `tools.native_enforcement` | `block` in the example config | `config.example.toml` |
| `observability.enabled` | `false` in the example config | `config.example.toml` |
| `tui.theme` | `system` in the example config | `config.example.toml` |

## Per-Environment Overrides

No checked-in `.env.development`, `.env.production`, or `.env.test` files define environment-specific overrides. Use separate Vulcan home directories or config files when testing different provider/gateway settings locally. Deployment-specific secret storage is not described in the repository.
<!-- VERIFY: Deployment-specific secret storage is not described in the repository. -->
