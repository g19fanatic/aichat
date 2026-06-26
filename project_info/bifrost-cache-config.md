# Bifrost Cache Observability — Config Guide

> **Status**: Companion documentation for the `extra_fields.cache_debug` observability
> work tracked in the Bifrost cache implementation plan. This file documents how to
> point aichat at a [Bifrost](https://github.com/maximhq/bifrost) gateway, request
> the prompt-cache debug payload, and surface cache HIT/MISS status in the CLI.

---

## 1. What This Enables

[Bifrost](https://www.getmaxim.ai/bifrost) is an LLM gateway that supports response
prompt caching. When caching is active, Bifrost attaches a `cache_debug` object to the
OpenAI-compatible chat-completions response under the top-level `extra_fields` key:

```json
{
  "id": "chatcmpl-...",
  "choices": [ /* ... */ ],
  "usage": { /* ... */ },
  "extra_fields": {
    "cache_debug": {
      "cache_hit": true,
      "hit_type": "direct",
      "cache_id": "bf_hash_9f2c...",
      "provider_used": "openai",
      "model_used": "gpt-4o"
    }
  }
}
```

Upstream aichat silently discards any unknown top-level fields (it only deserializes
`choices`/`usage`/`id`). This fork adds an `extra: Option<Value>` field to
`ChatCompletionsOutput` (`src/client/common.rs:340`) that captures the raw
`extra_fields` object from OpenAI-compatible responses so it can be:

1. **Surfaced in the CLI** as a one-line cache status (gated behind a config flag), and
2. **Passed through in serve mode** so downstream clients of aichat's proxy can see it.

---

## 2. Client Config: Point aichat at Bifrost

Bifrost exposes an **OpenAI-compatible** `/v1/chat/completions` endpoint. Use the
`openai-compatible` client type in `~/.config/aichat/config.yaml`. The cache namespace
is selected by sending the `x-bf-cache-namespace` request header, which aichat injects
via the per-client `patch.chat_completions.headers` mechanism (see
`RequestData::apply_patch` in `src/client/common.rs:303`).

```yaml
clients:
- type: openai-compatible
  name: bifrost
  api_base: http://localhost:8080/v1        # Bifrost gateway base URL (+ /v1)
  api_key: sk-your-bifrost-key              # forwarded as Bearer token
  models:
  - name: gpt-4o
    max_input_tokens: 128000
    supports_function_calling: true
    supports_vision: true
  # ---- Bifrost cache control via request header patch ----
  patch:
    chat_completions:
      ".*":                                 # regex match on model name (all models)
        headers:
          x-bf-cache-namespace: my-team-shared-cache
          x-bf-cache-ttl: "3600"                # cache entry lifetime in seconds (1 hour)
```

### Notes on the `patch` block

- `patch.chat_completions` keys are **regex patterns** matched against the model name;
  `".*"` applies the patch to every model under this client.
- The `headers` map is merged into the outgoing request by `apply_patch`. Setting a
  header value to `null` *removes* a header (see `common.rs:312-318`).
- You can also patch the request `body` here (e.g. to set Bifrost-specific cache TTL
  parameters) — the same `apply_patch` path performs a JSON merge on the body.

### Cache TTL (`x-bf-cache-ttl`)

Bifrost honors a per-request `x-bf-cache-ttl` header specifying how long (in
**seconds**) a cached response remains valid before it expires. Like the namespace
header, it is injected via the per-client `patch.chat_completions.headers` map — it
is just a sibling header, requiring no code changes (`apply_patch` already forwards
arbitrary headers).

| Value     | Effect                                          |
|-----------|-------------------------------------------------|
| `"3600"`  | Cache entries live for 1 hour                   |
| `"86400"` | 1 day                                           |
| omitted   | Bifrost gateway default TTL applies             |

- The value is a **string** (YAML header values serialize as strings).
- TTL does **not** affect the cache key/hash — it only controls expiry. Two requests
  with identical bodies but different TTLs still resolve to the same cache entry; the
  TTL of the *writing* request sets the entry's lifetime.
- For per-model TTLs, use distinct regex keys (e.g. `"gpt-4o"` vs `".*"`) to vary the
  TTL header by model.

> **Note**: the `x-bf-cache-ttl` header name is derived from the audit research
> findings rather than verified against Bifrost's source/docs directly. Confirm
> against your Bifrost gateway version if TTL behavior does not take effect.

---

## 3. ⚠️ Streaming Caveat — Use `stream: false`

Bifrost emits the `extra_fields.cache_debug` payload on the **non-streaming** JSON
response body. When streaming (SSE) is enabled, the cache debug object is **not**
reliably reconstructable from the token deltas, so cache status will not be surfaced.

To guarantee cache observability, disable streaming for the session/CLI invocation:

```yaml
# ~/.config/aichat/config.yaml (top-level)
stream: false
```

Or per-invocation:

```bash
aichat -m bifrost:gpt-4o --no-stream "Summarize this document"
# or via env:
AICHAT_STREAM=false aichat -m bifrost:gpt-4o "..."
```

> The CLI cache-status line is wired into the **non-streaming** consumption path
> (`call_chat_completions` in `src/client/common.rs:449`). Streaming responses bypass
> this path, so no cache line is printed for streamed requests.

---

## 4. Surfacing Cache Status — `show_gateway_info`

CLI cache-status display is **off by default** and gated behind a config flag so it
does not pollute output for users not on a caching gateway.

### Enable via config.yaml

```yaml
# ~/.config/aichat/config.yaml (top-level)
show_gateway_info: true
```

### Enable via environment variable

```bash
export AICHAT_SHOW_GATEWAY_INFO=true
```

When enabled, after a non-streaming completion aichat reads
`output.extra["cache_debug"]` and prints a single status line to **stderr** (so it does
not contaminate piped stdout), e.g.:

```
⚡ Bifrost cache HIT (direct) [bf_hash_9f2c...]
⚡ Bifrost cache MISS
```

| Field        | Source (`cache_debug.*`) | Meaning                                   |
|--------------|--------------------------|-------------------------------------------|
| HIT / MISS   | `cache_hit` (bool)       | Whether the response was served from cache |
| `(hit_type)` | `hit_type`               | e.g. `direct`, `semantic` (shown on HIT)  |
| `[cache_id]` | `cache_id`               | Bifrost cache entry identifier (on HIT)   |

---

## 5. Serve-Mode Pass-Through

When aichat runs as an OpenAI-compatible proxy (`aichat --serve`), the captured
`extra` value is re-injected into the non-streaming proxy response under the
`extra_fields` key (`ret_non_stream` in `src/serve.rs:742`). This lets downstream
consumers of aichat's proxy observe the same Bifrost `cache_debug` data:

```bash
aichat --serve 0.0.0.0:8000

curl http://localhost:8000/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"bifrost:gpt-4o","stream":false,
       "messages":[{"role":"user","content":"hi"}]}' | jq .extra_fields
```

```json
{
  "cache_debug": {
    "cache_hit": true,
    "hit_type": "direct",
    "cache_id": "bf_hash_9f2c..."
  }
}
```

`extra_fields` is only present when the upstream gateway provided it (i.e. `Some`);
non-Bifrost providers and native/Converse parsers leave it absent.

---

## 6. End-to-End Checklist

1. Run a Bifrost gateway with prompt caching enabled.
2. Add the `openai-compatible` client block (Section 2) with the
   `x-bf-cache-namespace` header patch.
3. Set `stream: false` (Section 3) so the cache payload is delivered intact.
4. Set `show_gateway_info: true` or `AICHAT_SHOW_GATEWAY_INFO=true` (Section 4).
5. Run `aichat -m bifrost:gpt-4o "<prompt>"` twice — the second run should print
   `⚡ Bifrost cache HIT ...`.
6. (Optional) For proxy use, run `aichat --serve` and inspect `.extra_fields` in the
   JSON response (Section 5).

---

## 7. Implementation Cross-References

| Concern                        | Location                                  |
|--------------------------------|-------------------------------------------|
| `extra` field on output        | `src/client/common.rs:340`                |
| Header/body request patching   | `src/client/common.rs:303` (`apply_patch`)|
| OpenAI-compatible providers     | `src/client/mod.rs:38` (`OPENAI_COMPATIBLE_PROVIDERS`) |
| `extra_fields` extraction       | `src/client/openai.rs:399` (`openai_extract_chat_completions`) |
| CLI cache-status line           | `src/client/common.rs:449` (`call_chat_completions`) |
| `show_gateway_info` config flag | `src/config/mod.rs` (`Config` struct + `load_envs`) |
| Serve-mode pass-through         | `src/serve.rs:742` (`ret_non_stream`)     |
