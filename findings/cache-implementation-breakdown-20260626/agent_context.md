# AGENT.md — Cache Implementation
## Project
- Source directories:
  - aichat: /home/pdibiase/sources/aichat (Rust)
  - vim-llm-assistant: /home/pdibiase/sources/vim-llm-assistant (VimScript)
- Language/stack: Rust (aichat), VimScript (vim-llm-assistant)

## Build Commands
```bash
# aichat
cd /home/pdibiase/sources/aichat && cargo build --release

# vim-llm-assistant (no build step — interpreted VimScript)
# Validate syntax:
vim -u NONE -c "source autoload/llm.vim" -c "qa" 2>&1
```

## Test Commands
```bash
# aichat
cd /home/pdibiase/sources/aichat && cargo test

# Manual cache verification (after implementing metrics):
cd /home/pdibiase/sources/aichat && echo "hello" | cargo run -- -m bifrost:bedrock/claude-sonnet-4 2>&1 | grep "📦 Cache"
```

## Passing Criteria
- Code tasks (aichat): `cargo build --release` passes with no errors
- Code tasks (vim): File sources without VimScript errors
- Metrics task: Two identical requests show cache_read > 0 on second request
- Each task: specific code change applied, compiles, doesn't break existing behavior

## Key Files

### aichat — Cache Logic
- src/client/claude.rs — Claude API request building + cache breakpoint logic (lines 280-340)
- src/client/claude.rs:345-395 — Response extraction (where metrics come from)
- src/client/common.rs:460-500 — Gateway info display (where to add cache metrics display)
- src/client/openai.rs:353-386 — OpenAI-compatible Claude caching block
- src/config/input.rs — Input/message building from JSON files and CLI args

### vim-llm-assistant — Context Assembly
- autoload/llm.vim:37-44 — llm#encode() key ordering
- autoload/llm.vim:481-560 — llm#run() entry point and context building
- autoload/llm.vim:617-650 — Data dict assembly (cursor, buffers, history)
- autoload/llm/adapters/aichat.vim:161-170 — Command construction
- plugin/llm.vim — Command definitions

### Research Reference
- findings/anthropic-cache-optimization-20260626/ — All 10 research findings (224KB)
- findings/anthropic-cache-optimization-20260626/10-implementation-plan.md — The source plan

## Known Learnings
(empty — first iteration)
