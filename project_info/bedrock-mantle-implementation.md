# Bedrock Mantle (OpenAI-Compatible) Support — Implementation Project

## Status: PLAN STAGE COMPLETE — Ready for REVIEW → APPLY

**Date**: 2026-06-02  
**Project**: `~/sources/aichat` (custom fork)  
**Workspace**: `/tmp/ralph-gHeJSz/` (findings preserved — see Appendix B for key content)

---

## Quick Resume Instructions

To pick up this work in a new session:
1. Open this file + the implementation plan at `/tmp/ralph-gHeJSz/findings/21-implementation-plan.md` (if temp still exists)
2. If temp is gone, the full plan is reproduced in Section 5 below
3. The implementation is **~120 LOC in ONE file**: `src/client/bedrock.rs`
4. Start at REVIEW stage → validate plan → move to APPLY

---

## 1. Problem Statement

**Error**: `Error: Failed to call chat-completions api / The provided model identifier is invalid.`

**Config that fails** (`~/.config/aichat/config.yaml`):
```yaml
- type: bedrock
  name: aws-openai
  region: us-east-2
  profile: anduril-armory  # ← silently ignored (bug)
  supports_function_calling: true
  models:
  - name: openai.gpt-5.5
    max_tokens: 1000000
    supports_vision: true
```

**But Codex works** with the same model/region/profile (`~/.codex/config.toml`):
```toml
model = 'openai.gpt-5.5'
model_provider = 'amazon-bedrock'
[model_providers.amazon-bedrock.aws]
profile = 'anduril-armory'
region = 'us-east-2'
```

---

## 2. Root Cause (CONFIRMED)

| | **Codex CLI** | **aichat** |
|--|---|---|
| **Endpoint domain** | `bedrock-mantle.us-east-2.api.aws` | `bedrock-runtime.us-east-2.amazonaws.com` |
| **API path** | `/openai/v1/responses` | `/model/openai.gpt-5.5/converse` |
| **Wire protocol** | OpenAI Responses API | AWS Converse API |
| **SigV4 service name** | `bedrock-mantle` | `bedrock` |
| **Model ID location** | In request body JSON | In URL path |

**Codex uses a completely different AWS service (Bedrock Mantle)** — an OpenAI-compatible gateway. aichat uses the older Bedrock Runtime Converse API which **does not support OpenAI models**.

### Secondary Bug: `profile` field silently ignored
`BedrockConfig` struct has no `profile` field. Serde drops it silently. Credentials come from `BEDROCK_AWS_PROFILE` env var (set in `.bashrc`).

---

## 3. Solution Architecture

**Pattern**: Follow VertexAI's `ModelCategory` enum for multi-vendor routing within a single client.

**Key Insight**: `bedrock-runtime` ALSO supports `/v1/chat/completions` (same host, same SigV4 service `"bedrock"`) — so we only need to change the **URL path** and **body/response format**. Zero auth changes.

```
openai.* models → /v1/chat/completions (OpenAI body format, SSE streaming)
everything else → /model/{id}/converse  (Converse body format, binary event-stream)
```

---

## 4. Implementation Summary (~120 LOC, single file)

### File: `src/client/bedrock.rs`

1. **Add import**: `use super::openai::{openai_build_chat_completions_body, openai_chat_completions, openai_chat_completions_streaming};`

2. **Add enum** (after BedrockConfig struct, ~line 26):
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BedrockModelCategory {
    Converse,  // Default: anthropic.*, meta.*, amazon.*, etc.
    OpenAI,    // For openai.* prefix models
}

impl std::str::FromStr for BedrockModelCategory {
    type Err = anyhow::Error;
    fn from_str(model_name: &str) -> std::result::Result<Self, Self::Err> {
        if model_name.starts_with("openai.") {
            Ok(BedrockModelCategory::OpenAI)
        } else {
            Ok(BedrockModelCategory::Converse)
        }
    }
}
```

3. **Modify `chat_completions_builder()`** (~line 41-86):
   - Return `(RequestBuilder, BedrockModelCategory)` tuple
   - Branch URI: OpenAI → `/v1/chat/completions`; Converse → `/model/{id}/converse[-stream]`
   - Branch body builder: OpenAI → `openai_build_chat_completions_body()`; Converse → existing `build_chat_completions_body()`

4. **Modify `chat_completions_inner()`** (~line 200):
   - Match on category: OpenAI → `openai_chat_completions()`; Converse → existing `chat_completions()`

5. **Modify `chat_completions_streaming_inner()`** (~line 208):
   - Match on category: OpenAI → `openai_chat_completions_streaming()` (SSE); Converse → existing binary event-stream

### Zero changes needed in:
- `src/client/openai.rs` (functions already `pub`)
- `src/client/common.rs` (error handler already supports both formats)
- `src/client/mod.rs` (no new client type)
- `Cargo.toml` (no new deps, `reqwest_eventsource` already present)

---

## 5. Full Implementation Plan

The exhaustive 38KB implementation plan was generated at:
`/tmp/ralph-gHeJSz/findings/21-implementation-plan.md`

Key sections:
- Architecture Decision (bedrock-runtime `/v1/chat/completions` — same host/auth!)
- File-by-File Changes (only bedrock.rs)
- New Types (just `BedrockModelCategory` enum)
- Config Schema (no required changes; optional `profile` field fix)
- Endpoint Routing (prefix detection on `openai.*`)
- Request Body Format Switching (reuse existing `openai_build_chat_completions_body()`)
- Response Parsing (reuse existing `openai_chat_completions()`)
- SigV4 Signing (UNCHANGED — same service name "bedrock")
- Streaming (SSE via `reqwest_eventsource` instead of binary event-stream)
- Error Handling (UNCHANGED — `catch_error()` handles both formats)
- Testing Strategy (unit tests + manual integration tests)
- Migration (zero breaking changes)
- Alternative Approaches (5 rejected with rationale)
- Phase 1 = single PR ~120 LOC; Phase 2 = profile field fix + models.yaml

---

## 6. Key Source File References

| File | Lines | What |
|------|-------|------|
| `src/client/bedrock.rs` | 41-86 | `chat_completions_builder()` — **primary modification target** |
| `src/client/bedrock.rs` | 200-215 | `chat_completions_inner()` + streaming — **modify** |
| `src/client/bedrock.rs` | 17-26 | `BedrockConfig` struct (optional: add `profile` field) |
| `src/client/bedrock.rs` | 380-500 | `build_chat_completions_body()` (Converse format, keep as-is) |
| `src/client/bedrock.rs` | 638-750 | `aws_fetch()` + SigV4 signing (unchanged) |
| `src/client/openai.rs` | 84-98 | `openai_chat_completions()` — reuse for response parsing |
| `src/client/openai.rs` | 100-197 | `openai_chat_completions_streaming()` — reuse for SSE |
| `src/client/openai.rs` | 226-395 | `openai_build_chat_completions_body()` — reuse for body |
| `src/client/vertexai.rs` | 37-77 | `ModelCategory` pattern — architectural reference |
| `src/client/common.rs` | 493-533 | `catch_error()` — already handles both error formats |

---

## 7. Environment Context

```bash
# AWS profile for bedrock (set in ~/.bashrc)
export BEDROCK_AWS_PROFILE="anduril-armory"
export SUBAGENT_AWS_PROFILE="anduril-armory"

# SSO login alias
alias aws_codex_login="aws sso login --profile anduril-armory"

# Debug logging
export AICHAT_LOG_LEVEL=debug
```

---

## 8. Verification Checklist (for APPLY stage)

- [ ] `cargo build` passes
- [ ] `cargo test` passes (existing tests unchanged)
- [ ] New unit tests for `BedrockModelCategory` detection pass
- [ ] Manual: `aichat -m anthropic.claude-sonnet-4-6 "Hello"` still works (regression)
- [ ] Manual: `aichat -m openai.gpt-5.5 "Hello"` returns valid response
- [ ] Manual: `aichat -m openai.gpt-5.5 --stream "Hello"` streams correctly
- [ ] Debug log shows `/v1/chat/completions` URL for openai models
- [ ] Tool calling works with openai models

---

## Appendix A: Research Findings Index

All detailed findings at `/tmp/ralph-gHeJSz/findings/`:

| File | Content |
|------|---------|
| `01-aichat-endpoint-construction.md` | How bedrock.rs builds URLs (Converse only) |
| `02-bedrock-chat-completions-api.md` | AWS Chat Completions API docs + Bedrock Mantle |
| `03-codex-bedrock-integration.md` | Codex source analysis (bedrock-mantle endpoint) |
| `04-aichat-chat-completions-support.md` | Why error says "chat-completions" (generic wrapper) |
| `06-vertexai-model-category-pattern.md` | VertexAI routing pattern to replicate |
| `09-response-parsing.md` | Converse vs OpenAI response format comparison |
| `12-request-body-format.md` | Converse vs OpenAI request body comparison |
| `16-sigv4-signing.md` | SigV4 implementation (already parameterized) |
| `18-streaming-implementation.md` | Binary event-stream vs SSE comparison |
| `19-config-struct-analysis.md` | BedrockConfig fields + what to add |
| `20-openai-compatible-reference.md` | OpenAI-compatible client reference |
| `21-implementation-plan.md` | **EXHAUSTIVE 38KB implementation plan** |

---

## Appendix B: Config References

### aichat config (`~/.config/aichat/config.yaml`) — aws-openai section:
```yaml
- type: bedrock
  name: aws-openai
  region: us-east-2
  profile: anduril-armory
  supports_function_calling: true
  models:
  - name: openai.gpt-5.5
    max_tokens: 1000000
    supports_vision: true
```

### Codex config (`~/.codex/config.toml`):
```toml
model = 'openai.gpt-5.5'
model_provider = 'amazon-bedrock'
model_reasoning_effort = 'medium'
[model_providers.amazon-bedrock.aws]
profile = 'anduril-armory'
region = 'us-east-2'
```

---

## Appendix C: Session Timeline

1. **10:26 PM** — First attempt to use `aws-openai` model fails with "model identifier is invalid"
2. **10:35 PM** — Started troubleshooting with agent-loop (10 steps). Found:
   - `openai.gpt-5.5` doesn't exist in Bedrock `list-foundation-models`
   - `profile:` field is silently ignored
   - aichat uses `/model/{id}/converse` endpoint
3. **10:48 PM** — Question: "Why does Codex work?" Started deep investigation (20 steps):
   - Codex uses `bedrock-mantle.{region}.api.aws` (completely different service!)
   - SigV4 service is `"bedrock-mantle"` not `"bedrock"`
   - OpenAI models only accessible via Mantle or Chat Completions API
4. **11:10 PM** — Requested implementation plan. Resumed agent-loop:
   - Completed all source code inspection tasks (vertexai pattern, sigv4, streaming, config, openai_compatible)
   - Generated 38KB exhaustive implementation plan (task 21)
5. **Current** — PLAN stage complete. Ready for REVIEW → APPLY.
