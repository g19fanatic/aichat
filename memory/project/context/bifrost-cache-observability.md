---
summary: "How aichat captures and surfaces Bifrost extra_fields.cache_debug — the extra: Option<Value> plumbing on ChatCompletionsOutput."
created: 2026-06-16
updated: 2026-06-16
type: context
scope: project
project: aichat
importance: high
confidence: verified
sources:
  - type: file
    path: src/client/common.rs
    note: "ChatCompletionsOutput.extra field at :340; call_chat_completions CLI surface at :467"
  - type: file
    path: project_info/bifrost-cache-config.md
    note: "User-facing config guide for the feature"
  - type: soft
    key: conversation/2026-06-16/bifrost-cache-audit
    note: "Agent-loop audit + implementation of cache observability"
see_also: [memory/project/decisions/bifrost-cache-observability-design.md]
tags: [bifrost, caching, openai-compatible, observability, chatcompletionsoutput]
task_ref:
  - id: "1-9"
    label: "Bifrost cache observability implementation plan"
---

# Bifrost Cache Observability in aichat

## What this is
aichat connects to the Bifrost AI Gateway via the `openai-compatible` client
type. Bifrost attaches `extra_fields.cache_debug` (cache_hit, hit_type, cache_id,
provider_used, model_used) to non-streaming chat-completion responses. Upstream
aichat silently discarded this. This feature captures and surfaces it.

## The plumbing (verified, build+test green — 55 passed)

**Capture**: `ChatCompletionsOutput` (`src/client/common.rs:340`) gained a
`pub extra: Option<Value>` field. Struct derives `Default`, so `new()` is
unchanged; all explicit construction sites carry `extra:`.

**Extraction** (`extra: data.get("extra_fields").cloned()`):
- `src/client/openai.rs:439` & `:456` — both `openai_extract_chat_completions` sites (Bifrost path)
- `src/client/bedrock.rs:992` & `:1002` — responses-api (OpenAI-format) parser

**`extra: None`** (native/Converse parsers — no extra_fields):
- `claude.rs:392`, `cohere.rs:255`, `vertexai.rs:305` (gemini), `bedrock.rs:691` (Converse)

**CLI surface** (`src/client/common.rs:467`): `call_chat_completions` destructures
`extra`, and when `show_gateway_info` is enabled prints to stderr:
`⚡ Bifrost cache HIT/MISS (hit_type) [cache_id]`. Reads the flag via
`client.global_config().read().show_gateway_info.unwrap_or(false)`.

**Config flag**: `show_gateway_info: Option<bool>` on `Config`
(`src/config/mod.rs:148` field, `:223` default None, `:2383` env load
`AICHAT_SHOW_GATEWAY_INFO` via `read_env_bool`).

**Serve passthrough** (`src/serve.rs:742` `ret_non_stream`): `res_body` made
`mut`; injects `extra_fields` into the response JSON when `output.extra` is Some.

## Critical operating constraint
The CLI cache line ONLY works with **`stream: false`** — streaming bypasses
`call_chat_completions` entirely and Bifrost emits cache_debug only on the
non-streaming JSON body. User already runs `stream: false` in their config.

## Bifrost client config
Header injection (`x-bf-cache-namespace`) is config-only via
`patch.chat_completions.<regex>.headers` (handled by `apply_patch` at
`common.rs:303`). No hardcoded Bifrost provider code — it's just an
`openai-compatible` entry with `name: bifrost` + `api_base`.
