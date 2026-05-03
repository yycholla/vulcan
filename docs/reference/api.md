<!-- generated-by: gsd-doc-writer -->
# Gateway API

The gateway API is available when Vulcan is built with the `gateway` feature and run through the gateway command. Routes are defined in `src/gateway/server.rs` and handled by modules under `src/gateway/routes/`.

## Authentication

`GET /health` is public. Routes under `/v1/*` require an `Authorization: Bearer <token>` header matching `gateway.api_token`. Webhook routes live outside the `/v1` bearer-auth nest and use per-platform webhook authentication.

## Endpoints Overview

| Method | Path | Description | Auth Required |
|--------|------|-------------|---------------|
| GET | `/health` | Health check route. | No |
| GET | `/v1/lanes` | Diagnostic snapshot of gateway lane to daemon-session mappings. | Bearer token |
| POST | `/v1/inbound` | Enqueue an inbound message for gateway processing. | Bearer token |
| GET | `/v1/scheduler` | Inspect configured scheduler jobs and persisted run history when scheduler storage is enabled. | Bearer token |
| POST | `/webhook/{platform}` | Receive platform webhook payloads for registered platforms. | Platform-specific |

## Request and Response Formats

`/v1/inbound` accepts JSON handled by `src/gateway/routes/inbound.rs` and writes to the inbound queue. The gateway stores accepted work in SQLite-backed queue tables through `src/gateway/queue.rs`.

`/v1/lanes` returns a JSON snapshot derived from `DaemonLaneRouter`.

`/v1/scheduler` returns scheduler job status from `Config.scheduler` and optional scheduler run history from `src/gateway/scheduler_store.rs`.

## Error Codes

| Status | Meaning |
|--------|---------|
| `401 Unauthorized` | Missing, malformed, or incorrect bearer token on `/v1/*`. |
| `413 Payload Too Large` | Request body exceeds the configured Axum body limit for `/v1/*` or webhook routes. |
| `5xx` | Queue, registry, or platform handling failed while processing the request. |

## Rate Limits

No rate-limit middleware is defined in the gateway router. Body-size limits are enforced in `src/gateway/server.rs` with a 64 KiB default for `/v1/*` and webhook routes.
