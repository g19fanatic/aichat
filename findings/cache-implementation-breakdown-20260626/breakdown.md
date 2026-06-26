# Breakdown: Anthropic Prompt Cache Implementation Plan

**Source**: `findings/anthropic-cache-optimization-20260626/10-implementation-plan.md`
**Date**: 2026-06-26
**Projects**: aichat (Rust), vim-llm-assistant (VimScript)

---

## Epic 1: aichat Critical Cache Fixes (Phase 1)

**Goal**: Maximize cache hits with minimal code changes. Highest ROI.  
**Estimated effort**: 2 hours total  
**Dependencies**: None (all changes are independent)

### Milestone 1.1: Tool Definition Caching
> Cache the ~8K tokens of tool definitions that currently re-process at full price every request.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **1** | Add `cache_control` to last tool in tools array | `src/client/claude.rs:306-335` | — |
| **2** | Sort tools alphabetically for deterministic ordering | `src/client/claude.rs:289-303` | — |

### Milestone 1.2: Cache Observability
> Without metrics display, you cannot verify caching works.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **3** | Extract cache metrics from Anthropic response `usage` field into `ChatCompletionsOutput.extra` | `src/client/claude.rs:345-395` | — |
| **4** | Display cache metrics to stderr (eprintln) | `src/client/common.rs:480-495` | 3 |

### Milestone 1.3: Breakpoint Budget Optimization
> Free a breakpoint slot by removing automatic mode (explicit breakpoints already cover it).

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **5** | Remove top-level `body["cache_control"]` (line 311 area) — frees 1 of 4 slots | `src/client/claude.rs:311` | — |

---

## Epic 2: aichat Advanced Caching (Phase 2)

**Goal**: Handle long conversations and edge cases.  
**Estimated effort**: 2 hours total  
**Dependencies**: Task 5 (for breakpoint slot)

### Milestone 2.1: Long Conversation Support
> Conversations > 20 messages lose cache hits without intermediate breakpoints.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **6** | Add intermediate breakpoint at mid-conversation user message when messages > 20 | `src/client/claude.rs` (after line 335) | 5 |

### Milestone 2.2: Extended TTL + OpenAI Parity
> System prompt survives idle periods; tool caching works via OpenRouter too.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **7** | Add 1-hour TTL conditional to system prompt cache_control for Claude 4+ models | `src/client/claude.rs:312-318` | — |
| **8** | Mirror tool caching for Claude-via-OpenRouter (OpenAI-compatible client) | `src/client/openai.rs:353-386` | — |

---

## Epic 3: vim-llm-assistant Quick Wins (Phase 3)

**Goal**: Stabilize the JSON payload to maximize aichat's existing caching.  
**Estimated effort**: 1 hour total  
**Dependencies**: None (independent of aichat changes, but benefits compound)

### Milestone 3.1: Eliminate Cursor-Induced Cache Misses
> cursor_line/cursor_col in JSON forces re-processing of 20-50K tokens on EVERY cursor movement.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **9** | Remove cursor_line/cursor_col from JSON data dict; pass via env vars in cmd construction | `autoload/llm.vim:37-44,648-651`, `autoload/llm/adapters/aichat.vim:161-170` | — |
| **10** | aichat side: read AICHAT_CURSOR_LINE/AICHAT_CURSOR_COL env vars and append to user message after cached content | `src/config/input.rs` | 9 |

### Milestone 3.2: Content Ordering Optimization
> Extend stable prefix by moving stable content earlier.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **11** | Reorder `l:ordered_keys`: move file_arguments before active_buffer | `autoload/llm.vim:37-44` | — |

---

## Epic 4: vim-llm-assistant Architecture (Phase 4)

**Goal**: Enable proper multi-turn caching through structured data.  
**Estimated effort**: 3 hours total  
**Dependencies**: Phases 1-3 should be done first for validation

### Milestone 4.1: Structured History
> Convert flat text history into a structured turn array that aichat can parse into proper messages.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **12** | Implement `llm#parse_history_turns()` — parse scratch buffer `====` delimiters into turn array | `autoload/llm.vim` (new function) | — |
| **13** | Add `llm_history_turns` field to data dict in `llm#run()` alongside legacy `llm_history` | `autoload/llm.vim:617-620` | 12 |

### Milestone 4.2: Cache Hints & Warming
> Tell aichat where to place breakpoints; eliminate cold-start latency.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **14** | Add `_cache_hints` field to data dict with breakpoint_after/stable_fields/dynamic_fields | `autoload/llm.vim` (in llm#run) | — |
| **15** | Implement `:LLMWarm` command — builds context with `_cache_warm=1`, fires async request | `autoload/llm.vim`, `plugin/llm.vim` | — |

---

## Epic 5: Full Multi-Turn Architecture (Phase 5)

**Goal**: Fundamental restructuring — proper multi-turn messages for maximum caching.  
**Estimated effort**: 8+ hours total  
**Dependencies**: Tasks 12-13 (Phase 4.1 structured history)

### Why Phase 5 Matters

```
Current:  All context crammed into a single user message text block
          → Anthropic caches the prefix, but ANY edit to the block invalidates everything after it
          → Each request re-transmits the ENTIRE conversation history

Future:   History as proper user/assistant message pairs + context as separate blocks  
          → Turn 1 cached permanently (until TTL)
          → Turn 2 reads turn 1 from cache, writes only its delta
          → Turn N reads 1→(N-1) from cache, writes only turn N
          → COMPOUNDING savings: 91% cost reduction by turn 20
```

### Milestone 5.1: aichat Multi-Turn Message Builder
> Parse structured turns from vim-llm-assistant and build proper API message pairs.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **16** | Add `parse_vim_history_turns()` function in input.rs — detect `llm_history_turns` field in JSON, extract user/assistant pairs | `src/config/input.rs` | 13 |
| **17** | Modify message building: when turns present, prepend as user/assistant message pairs BEFORE the current user message (context/prompt) | `src/config/input.rs` (in `build_messages()` or equivalent) | 16 |
| **18** | Handle edge cases: empty turns, turns with only user or only assistant, turns with tool calls | `src/config/input.rs` | 17 |

### Milestone 5.2: Cache Breakpoints at Turn Boundaries
> Place explicit cache_control at the last history turn (right before current request).

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **19** | In `claude_build_chat_completions_body()`: detect when multi-turn history messages are present, place cache_control on the last history assistant message | `src/client/claude.rs` | 17 |
| **20** | Adjust intermediate breakpoint logic (task 6) to account for multi-turn messages — move stepping stone into the history messages when applicable | `src/client/claude.rs` | 6, 19 |

### Milestone 5.3: Context Block Separation
> Split the current single user message into semantic blocks (context, buffers, prompt) for finer-grained caching.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **21** | aichat: Parse `_cache_hints` from JSON input; when present, split single user message text into multiple content blocks at designated boundaries | `src/config/input.rs` | 14 |
| **22** | aichat: Place cache_control on the content blocks designated by `_cache_hints.breakpoint_after` | `src/client/claude.rs` | 21 |

### Milestone 5.4: vim-llm-assistant Multi-Turn Mode
> Signal multi-turn support and eventually drop the legacy flat history.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **23** | vim: Add env var `AICHAT_MULTITURN_READY=1` to adapter command when `llm_history_turns` is populated | `autoload/llm/adapters/aichat.vim` | 13 |
| **24** | vim: Behind `g:llm_multiturn_mode` flag (default 0), stop including flat `llm_history` when turns are available — reduces payload by ~50% | `autoload/llm.vim` (in llm#run) | 23 |

### Milestone 5.5: max_tokens:0 Support for Cache Warming
> Allow `:LLMWarm` to work by supporting zero-output requests.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **25** | aichat: Detect `_cache_warm` field or `AICHAT_CACHE_WARM=1` env var; set `max_tokens: 1` (minimum), return only usage data, suppress output display | `src/config/input.rs`, `src/client/claude.rs` | 15 |

### Milestone 5.6: Verification & Integration Testing
> Prove the full multi-turn pipeline works end-to-end.

| Task | Description | Files | Depends |
|------|-------------|-------|---------|
| **26** | Create manual test script: start vim session → make 3 requests → verify cache_read grows on each turn via stderr metrics | `tests/cache_integration_test.sh` or docs | 4, 17, 19 |
| **27** | Document the full multi-turn architecture in `project_info/` or README section | `project_info/cache-architecture.md` | All |

---

## Dependency Graph (Visual)

```
Phase 1 (Independent — do first)
├── T1  Tool cache_control          ──── standalone
├── T2  Deterministic tool sort     ──── standalone
├── T3  Extract cache metrics       ──── standalone
├── T4  Display cache metrics       ──── depends T3
└── T5  Remove auto cache_control   ──── standalone (prerequisite for T6)

Phase 2 (After T5)
├── T6  Intermediate breakpoints    ──── depends T5
├── T7  Extended TTL                ──── standalone
└── T8  OpenAI-compat tools         ──── standalone

Phase 3 (Independent — parallel with Phase 1-2)
├── T9  Remove cursor from JSON     ──── standalone
├── T10 aichat: read cursor env     ──── depends T9
└── T11 Reorder keys                ──── standalone

Phase 4 (After Phases 1-3 verified)
├── T12 Parse history turns         ──── standalone
├── T13 Add turns to data dict      ──── depends T12
├── T14 Add _cache_hints            ──── standalone
└── T15 :LLMWarm command            ──── standalone

Phase 5 (After Phase 4)
├── T16 Parse turns in aichat       ──── depends T13
├── T17 Build multi-turn messages   ──── depends T16
├── T18 Edge cases                  ──── depends T17
├── T19 Turn boundary breakpoints   ──── depends T17
├── T20 Adjust intermediate logic   ──── depends T6, T19
├── T21 Parse _cache_hints          ──── depends T14
├── T22 Hint-driven breakpoints     ──── depends T21
├── T23 MULTITURN_READY env         ──── depends T13
├── T24 Drop flat history (flag)    ──── depends T23
├── T25 Cache warming support       ──── depends T15
├── T26 Integration test            ──── depends T4, T17, T19
└── T27 Documentation               ──── depends All
```

---

## Critical Path

The minimum path to maximum benefit:

```
T1 + T2 + T3 → T4 + T5 → T6
              ↓
T9 → T10 + T11
              ↓
T12 → T13 → T16 → T17 → T19 → T26
```

**Tasks 1-5, 9-11** = Phase 1+3 = **90% of benefit** in **~3 hours**.  
Everything after is compounding optimization.

---

## Phase 5 Detailed Design Notes

### How Multi-Turn Messages Work with Anthropic Caching

**Current single-message approach:**
```json
{
  "messages": [
    {"role": "user", "content": "<ENTIRE 80K token context blob>"}
  ]
}
```
Cache behavior: Prefix of the single message is cached. Any edit to any part of the message invalidates everything after the edit point.

**Multi-turn approach:**
```json
{
  "messages": [
    {"role": "user", "content": "Turn 1 user message"},
    {"role": "assistant", "content": "Turn 1 assistant response"},
    {"role": "user", "content": "Turn 2 user message"},
    {"role": "assistant", "content": "Turn 2 assistant response", "cache_control": {"type": "ephemeral"}},
    {"role": "user", "content": "<current context + prompt>"}
  ]
}
```
Cache behavior: ALL previous turns are byte-identical between requests → cached at 90% discount. Only the new current message is full-price.

### Turn Parsing Contract (vim → aichat)

The `llm_history_turns` field in the JSON will have this shape:
```json
{
  "llm_history_turns": [
    {
      "timestamp": "Thu 26 Jun 2026 09:12:41 AM EDT",
      "user": "the user's prompt text",
      "assistant": "the full assistant response text"
    },
    ...
  ]
}
```

**Parsing rules for vim-llm-assistant:**
- `==== <timestamp> ====` marks turn start
- `Prompt: <text>` is the user message
- Everything after `Prompt:` until the next `====` is assistant response
- Turns missing either user or assistant are still included (with empty string for missing part)

**Building rules for aichat:**
- Each turn with both user + assistant → two messages (user role, assistant role)
- Turn with only user (the current/last turn) → becomes part of the current user message
- Tool calls in assistant responses: detected by `**~~ Call` pattern and converted to proper tool_use blocks (FUTURE — initially keep as text)

### Why Not Just Use aichat Sessions?

aichat has built-in session support (`--session`) that maintains conversation history. However:

1. **vim-llm-assistant owns the history** — it's in the scratch buffer, user-visible, editable
2. **Context is injected per-request** — buffers, cursor, active file change between requests
3. **The JSON file IS the message** — aichat's session would be redundant/conflicting
4. **We need fine-grained control** — cache breakpoints at specific turn boundaries

The right approach is: vim manages state, passes structured turns, aichat builds optimal API messages.
