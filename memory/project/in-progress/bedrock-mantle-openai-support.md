---
summary: "Adding Bedrock Mantle (OpenAI-compatible) endpoint support to aichat's bedrock client — enables openai.gpt-5.5 on AWS Bedrock"
created: 2026-06-02
updated: 2026-06-02
type: session
scope: project
project: aichat
importance: critical
status: in-progress
task_ref:
  - id: "1-8"
    label: "Bedrock Mantle support implementation (Phase 1 + Phase 2)"
tags: [bedrock, openai, aws, bedrock-mantle, chat-completions]
sources:
  - type: file
    path: src/client/bedrock.rs
    note: "Primary file to modify (~120 LOC change)"
  - type: file
    path: project_info/bedrock-mantle-implementation.md
    note: "Full session documentation with plan"
  - type: soft
    key: conversation/2026-06-02/bedrock-mantle-troubleshooting
    note: "Three-part investigation: why model fails, why codex works, implementation plan"
---

# Bedrock Mantle OpenAI Support — In-Progress

## Current State
- **Stage**: PLAN COMPLETE → Ready for REVIEW → APPLY
- **Workspace**: `/tmp/ralph-gHeJSz/` (12 findings files including 38KB implementation plan)
- **Documentation**: `project_info/bedrock-mantle-implementation.md`
- **Todos**: `todos.md` (8 tasks, all pending)

## What's Been Done
1. ✅ Root cause identified: aichat uses `/model/{id}/converse` (doesn't support OpenAI models)
2. ✅ Codex comparison: Codex uses `bedrock-mantle.{region}.api.aws` (different service entirely)
3. ✅ Source code fully analyzed (bedrock.rs, vertexai.rs pattern, openai.rs functions, sigv4 signing, streaming)
4. ✅ Exhaustive 38KB implementation plan generated with file-by-file changes
5. ✅ Key insight: `bedrock-runtime` ALSO has `/v1/chat/completions` — same host/auth, just different path

## Key Architecture Decision
**Use `bedrock-runtime` `/v1/chat/completions` path** (NOT bedrock-mantle):
- Same host as current (`bedrock-runtime.{region}.amazonaws.com`)
- Same SigV4 service name (`"bedrock"`)
- Same credentials
- Only URI + body/response format changes
- Optional bedrock-mantle override for power users (Phase 2)

## Implementation: ~120 LOC in `src/client/bedrock.rs`
1. Add `BedrockModelCategory` enum (`Converse` | `OpenAI`)
2. Detect `openai.*` prefix via `FromStr` impl
3. Branch `chat_completions_builder()` on category (different URI + body builder)
4. Branch response parser (Converse vs OpenAI format)
5. Branch streaming handler (binary event-stream vs SSE)
6. Reuse existing `pub` functions from `openai.rs` (already available)

## Next Session Actions
1. Open `src/client/bedrock.rs` and validate line numbers from plan
2. Verify `openai_build_chat_completions_body`, `openai_chat_completions`, `openai_chat_completions_streaming` are `pub`
3. Implement the 5-step change
4. `cargo build` + `cargo test`
5. Manual test: `aichat -m openai.gpt-5.5 "Hello"`

## Secondary Bug to Fix
`profile:` field in bedrock config YAML is silently ignored by serde (field doesn't exist in struct).
Credentials work because `BEDROCK_AWS_PROFILE=anduril-armory` is set in `.bashrc`.
Fix: Add `pub profile: Option<String>` to `BedrockConfig` struct.
