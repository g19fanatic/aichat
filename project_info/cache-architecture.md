# Multi-Turn Prompt Cache Architecture

**Date**: 2025-07-16  
**Scope**: aichat (Rust) + vim-llm-assistant (VimScript)  
**Status**: Fully implemented (Tasks 1–26)

---

## Overview

This document describes the Anthropic prompt caching architecture spanning two projects:

- **aichat** (`src/client/claude.rs`, `src/config/input.rs`) — Rust backend that constructs Claude API requests with strategic `cache_control` breakpoints
- **vim-llm-assistant** (`autoload/llm.vim`, `autoload/llm/adapters/aichat.vim`) — VimScript frontend that structures context data for optimal caching

The architecture achieves **70–90% input token cost savings** by:
1. Allocating 4 explicit cache breakpoints across the request structure
2. Building proper multi-turn messages from structured conversation history
3. Splitting user content into independently-cacheable blocks via a hint protocol
4. Removing volatile data (cursor position) from the cached JSON payload

---

## 1. Breakpoint Allocation Strategy

Anthropic's Claude API supports a maximum of **4 explicit `cache_control` breakpoints** per request. Our allocation maximizes cache hit rates across varying conversation lengths:

### The 4 Breakpoints

| # | Location | What It Caches | TTL | File Reference |
|---|----------|---------------|-----|----------------|
| 1 | Last tool in `tools[]` array | All tool definitions (~8K tokens) | 5 min | `claude.rs:314-319` |
| 2 | System message text block | System prompt (~10K tokens) | 1 hour* | `claude.rs:321-328` |
| 3 | Last assistant msg before current turn | Entire conversation history prefix | 5 min | `claude.rs:360-380` |
| 4 | Last message content block | Current user context | 5 min | `claude.rs:330-345` |

*\*1-hour TTL applies to Claude 4+ models via `extended-cache-ttl-2025-04-11` beta header; falls back to 5-min for older models.*

### Stepping-Stone Breakpoint (Conditional 5th)

For very long conversations (>10 history messages before the last assistant boundary), a **stepping-stone breakpoint** is placed at the midpoint of the history region. This prevents lookback window misses in sessions with 20+ messages:

```
claude.rs:383-418 — Intermediate breakpoint logic
- Only activates when history_end > 10 messages
- Placed on a user message near the midpoint of history
- Ensures the API's 20-block lookback always finds a prior cache entry
```

### Cache Invalidation Hierarchy

```
Change tools     → invalidates: tools ✘, system ✘, messages ✘ (everything)
Change system    → invalidates: tools ✓, system ✘, messages ✘
Change history   → invalidates: tools ✓, system ✓, history prefix ✓, new content ✘
Change last msg  → invalidates: tools ✓, system ✓, history ✓, last msg ✘
```

This means: changing the user's prompt (last message) preserves cache hits for tools, system prompt, AND all conversation history — only the new prompt content is processed at full price.

### Deterministic Tool Ordering

Tools are sorted alphabetically by name (`claude.rs:293-308`) before being assigned to the request body. This prevents non-deterministic iteration order from silently invalidating the tools cache:

```rust
tools.sort_by(|a, b| {
    a["name"].as_str().unwrap_or("").cmp(&b["name"].as_str().unwrap_or(""))
});
```

### OpenRouter Parity

The OpenAI-compatible client (`openai.rs:353-390`) mirrors the same caching strategy for Claude models served via OpenRouter and other compatible providers. It applies `cache_control` to:
- Last tool in the tools array
- System message (converted to content block array)
- Last user message

---

## 2. Multi-Turn Message Building

### Architecture

Instead of passing conversation history as a flat text blob in a single user message, the system constructs **proper multi-turn API messages** where each conversation exchange becomes a user/assistant message pair:

```
[vim-llm-assistant]                    [aichat]                         [Claude API]
                                                                        
[LLM-Scratch] buffer    →  JSON with        →  Proper message     →  messages: [
  ==== timestamp ====       llm_history_turns    pairs in API           {role: user, ...}
  Prompt: question          [{user, asst}]       request                {role: assistant}
  response text                                                         {role: user, ...}
                                                                        {role: assistant}
                                                                        {role: user, content}
                                                                       ]
```

### Message Construction (`input.rs:293-317`)

When `vim_history_turns` is non-empty, `build_messages()` splices history turn pairs **before** the last (current) user message:

```rust
// Find insertion point: before the last user message (current context)
if let Some(insert_pos) = messages.iter().rposition(|m| m.role == MessageRole::User) {
    let mut history_messages = Vec::new();
    for turn in &self.vim_history_turns {
        if !turn.user.is_empty() {
            history_messages.push(Message::new(MessageRole::User, ...));
        }
        if !turn.assistant.is_empty() {
            history_messages.push(Message::new(MessageRole::Assistant, ...));
        }
    }
    messages.splice(insert_pos..insert_pos, history_messages);
}
```

### Cache Breakpoint at History Boundary (`claude.rs:360-380`)

After building multi-turn messages, a `cache_control` breakpoint is placed on the **last assistant message before the current user message**. This ensures:

- Turn 1: Full content written to cache
- Turn 2: Turns 1's messages read from cache; only turn 2 is new
- Turn N: Turns 1→(N-1) read from cache; only turn N is new

Each subsequent request pays only for **new content's tokens**.

### Feature Flag

Multi-turn mode is gated by the `AICHAT_MULTITURN_READY=1` environment variable, set by the vim adapter when `llm_history_turns` is present in the JSON payload. This enables backward-compatible rollout.

On the vim side, `g:llm_multiturn_mode` (default 0) controls whether flat `llm_history` text is included alongside structured turns. When set to 1, the redundant flat history is omitted to save tokens.

---

## 3. Turn Parsing Contract

### JSON Schema (vim → aichat)

The `llm_history_turns` field in the JSON payload follows this contract:

```json
{
  "llm_history_turns": [
    {
      "timestamp": "Mon Jul 15 10:30:00 2025",
      "user": "User's question text",
      "assistant": "Assistant's response text"
    },
    {
      "timestamp": "Mon Jul 15 10:35:00 2025",
      "user": "Follow-up question",
      "assistant": "Follow-up response"
    }
  ]
}
```

### Field Semantics

| Field | Type | Required | Notes |
|-------|------|----------|-------|
| `timestamp` | string | No | Human-readable timestamp from [LLM-Scratch] buffer headers |
| `user` | string | Yes | User's prompt text (non-empty, non-whitespace) |
| `assistant` | string | No | Assistant's response (empty string if turn in progress) |

### Parsing Rules (aichat side — `input.rs:508-554`)

1. **Empty/whitespace user**: Turn is skipped entirely (no meaningful prompt)
2. **Missing assistant**: Treated as empty string (turn may be in progress)
3. **Content > 16,000 chars**: Truncated with `\n(…truncated)` marker
4. **Non-string values**: Turn is skipped (type safety)
5. **Non-JSON input**: Returns empty Vec (graceful fallback)

### Parsing Rules (vim side — `llm.vim:94-143`)

The `llm#parse_history_turns()` function parses the `[LLM-Scratch]` buffer format:

```
==== <timestamp> ====
Prompt: <user message>
<assistant response lines...>
<blank line>
```

- Lines matching `^==== .* ====$` start a new turn
- Lines matching `^Prompt: ` (only immediately after a timestamp, before any assistant content) extract the user field
- All subsequent lines until the next timestamp are the assistant response
- Leading/trailing blank lines in assistant content are trimmed

### Ordering Guarantee

The `llm#encode()` function (`llm.vim:42-83`) enforces deterministic key ordering in the JSON output. The `llm_history_turns` key is placed second in the ordered list (after `llm_history`), ensuring it appears in the stable prefix region of the payload for cache stability.

---

## 4. Cache Hint System

### Purpose

The `_cache_hints` system allows vim-llm-assistant to communicate content boundaries to aichat, enabling the backend to split a single user message into multiple independently-cacheable content blocks.

### JSON Schema

```json
{
  "_cache_hints": {
    "breakpoint_after": ["llm_history", "buffers"],
    "stable_fields": ["llm_history", "buffers", "file_arguments"],
    "dynamic_fields": ["prompt"]
  }
}
```

| Field | Purpose |
|-------|---------|
| `breakpoint_after` | Field names after which a `cache_control` breakpoint should be placed |
| `stable_fields` | Fields that rarely change between requests (informational) |
| `dynamic_fields` | Fields that change every request (informational) |

### Content Block Splitting (`input.rs:561-640`)

The `split_json_content_blocks()` function processes the JSON input:

1. Parses the JSON object
2. Extracts `_cache_hints.breakpoint_after` array
3. Iterates over remaining fields (excluding `_`-prefixed metadata and `llm_history_turns`)
4. For each field, creates a `CacheContentBlock` with:
   - `field_name`: The JSON key
   - `text`: The field's string value (or serialized JSON for objects/arrays)
   - `is_breakpoint`: Whether this field appears in `breakpoint_after`

### API Request Construction (`claude.rs:330-345`)

When `cache_content_blocks` is non-empty, the last user message's content is replaced with an array of text blocks:

```json
{
  "role": "user",
  "content": [
    {"type": "text", "text": "<llm_history content>", "cache_control": {"type": "ephemeral"}},
    {"type": "text", "text": "<buffers content>", "cache_control": {"type": "ephemeral"}},
    {"type": "text", "text": "<active_buffer content>"},
    {"type": "text", "text": "<prompt content>"}
  ]
}
```

Blocks marked as breakpoints get `cache_control: {"type": "ephemeral"}`. This means:
- **History + buffers** are cached independently of the active buffer and prompt
- Changing only the prompt (most common case) preserves cache hits for all stable fields
- Changing the active buffer preserves cache hits for history and buffers

### Cursor Position Isolation

Cursor position (`cursor_line`, `cursor_col`) is removed from the JSON payload entirely and passed via environment variables (`AICHAT_CURSOR_LINE`, `AICHAT_CURSOR_COL`). This prevents the most volatile data from invalidating the entire user message cache. The aichat backend reads these env vars and appends cursor context to the user message **after** the cache breakpoint.

### Content Ordering

vim-llm-assistant orders JSON keys from most-stable to most-dynamic (`llm.vim:42-53`):

```
llm_history → llm_history_turns → buffers → file_arguments → active_buffer → prompt → _cache_hints
```

This ensures the maximum stable prefix before the first dynamic content change.

---

## 5. Verification Strategy

### Runtime Cache Metrics

Every Claude API response includes usage metrics that aichat extracts and displays:

```
📦 Cache: <read_tokens> read, <creation_tokens> written
```

**Extraction** (`claude.rs:430-440`): The `claude_extract_chat_completions()` function reads `cache_creation_input_tokens` and `cache_read_input_tokens` from the response's `usage` object and stores them in the `extra` field.

**Display** (`common.rs:496-501`): Shown on stderr when `show_gateway_info` is enabled.

### Health Indicators

| Metric | Healthy | Unhealthy |
|--------|---------|-----------|
| `cache_read` on 2nd+ request | > 0 (growing) | Always 0 |
| `cache_creation` on 2nd+ request | Small (only new content) | Same as 1st request |
| Ratio: `read / (read + creation + input)` | > 70% | < 30% |

### Cache Warming (`:LLMWarm`)

The `:LLMWarm` vim command (`llm.vim:818-897`, `plugin/llm.vim:70`) sends a cache-priming request:

1. Builds context identically to `llm#run()` (same buffers, history, hints)
2. Sets `_cache_warm: 1` in the JSON payload
3. aichat detects this field (`input.rs:651-679`) and sets `max_tokens: 1`
4. The API writes all breakpointed content to cache without generating a real response
5. Subsequent real requests benefit from immediate cache hits (no cold start)

### Integration Test (`scripts/test-multiturn-cache.sh`)

A comprehensive bash script validates the multi-turn cache strategy end-to-end:

**Test Design**:
1. Makes 3 sequential requests with growing `llm_history_turns` (1, 2, 3 turns)
2. All requests share identical `buffers` content (~8K tokens of Rust code)
3. Sleeps between requests to allow cache registration

**Assertions**:
- Turn 2 `cache_read` > Turn 1 `cache_read` (history turn 1 is now cached)
- Turn 3 `cache_read` >= Turn 2 `cache_read` (history turns 1-2 are cached)
- Turn 1 `cache_written` > Turn 2 `cache_written` (less new content each turn)
- Turn 1 `cache_written` > 0 (cache was populated on first request)

**Running**:
```bash
./scripts/test-multiturn-cache.sh [model_name]
# or
MODEL=claude-sonnet-4-20250514 ./scripts/test-multiturn-cache.sh
```

### Manual Verification Workflow

1. **First request** (cold cache):
   ```
   📦 Cache: 0 read, 35000 written
   ```
   
2. **Identical second request** (within 5 min):
   ```
   📦 Cache: 35000 read, 0 written
   ```

3. **Changed prompt only** (stable prefix cached):
   ```
   📦 Cache: 30000 read, 500 written
   ```

4. **After `:LLMWarm`** (pre-populated cache):
   ```
   First real request: 📦 Cache: 35000 read, 0 written
   ```

---

## Appendix A: Data Flow Diagram

```
┌─────────────────────────────────────────────────────────────────────┐
│ vim-llm-assistant                                                     │
│                                                                       │
│  [LLM-Scratch] ──→ llm#parse_history_turns() ──→ llm_history_turns  │
│                                                                       │
│  buffers, active_buffer, file_arguments ──→ l:data dict              │
│                                                                       │
│  _cache_hints = {breakpoint_after: [...]}                            │
│                                                                       │
│  llm#encode(l:data) ──→ JSON file (deterministic key order)          │
│                                                                       │
│  Cursor pos ──→ AICHAT_CURSOR_LINE/COL env vars                      │
│  AICHAT_MULTITURN_READY=1 env var (when turns present)               │
└───────────────────────────────────┬─────────────────────────────────┘
                                    │
                              JSON file + env vars
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────┐
│ aichat (input.rs)                                                    │
│                                                                       │
│  parse_vim_history_turns(content) ──→ Vec<VimHistoryTurn>            │
│  split_json_content_blocks(content) ──→ Vec<CacheContentBlock>       │
│  detect_cache_warm_field(content) ──→ bool                           │
│                                                                       │
│  build_messages():                                                    │
│    role.build_messages() + splice(history_turns) + tool_calls        │
│                                                                       │
│  prepare_completion_data():                                           │
│    ChatCompletionsData { messages, cache_content_blocks, cache_warm } │
└───────────────────────────────────┬─────────────────────────────────┘
                                    │
                          ChatCompletionsData
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────┐
│ aichat (claude.rs) — claude_build_chat_completions_body()            │
│                                                                       │
│  1. Sort tools alphabetically, assign to body["tools"]               │
│  2. Build messages array from ChatCompletionsData.messages            │
│  3. if cache_warm: set max_tokens = 1                                │
│  4. if should_cache:                                                  │
│     a. Breakpoint on last tool                                        │
│     b. Breakpoint on system message (1h TTL on Claude 4+)            │
│     c. Split user msg into content blocks (from cache_content_blocks)│
│     d. Breakpoint on last message                                     │
│     e. Breakpoint at history turn boundary (last assistant msg)       │
│     f. Stepping-stone breakpoint (if history > 10 messages)          │
└───────────────────────────────────┬─────────────────────────────────┘
                                    │
                              API Request
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────┐
│ Claude API Response                                                   │
│                                                                       │
│  usage: {                                                             │
│    input_tokens: N,                                                   │
│    cache_creation_input_tokens: M,                                    │
│    cache_read_input_tokens: K                                         │
│  }                                                                    │
│                                                                       │
│  Extracted by claude_extract_chat_completions() → extra field         │
│  Displayed by common.rs: "📦 Cache: K read, M written"               │
└─────────────────────────────────────────────────────────────────────┘
```

---

## Appendix B: File Reference Index

### aichat (Rust)

| File | Lines | Component |
|------|-------|-----------|
| `src/client/claude.rs:63-66` | Extended cache TTL header | Beta header for 1h TTL |
| `src/client/claude.rs:170-285` | Message construction | Flat messages → content arrays |
| `src/client/claude.rs:286` | Cache warm override | `max_tokens = 1` |
| `src/client/claude.rs:293-308` | Tool sorting + assignment | Deterministic alphabetical order |
| `src/client/claude.rs:311-420` | Cache breakpoint logic | All 4+ breakpoints allocated here |
| `src/client/claude.rs:430-440` | Cache metrics extraction | From response `usage` object |
| `src/client/claude.rs:455-460` | Extended cache helper | `claude_supports_extended_cache()` |
| `src/client/openai.rs:350-390` | OpenRouter caching | Mirror strategy for Claude-via-OpenAI |
| `src/client/common.rs:496-501` | Cache metrics display | `📦 Cache:` stderr output |
| `src/config/input.rs:22-57` | Type definitions | VimHistoryTurn, CacheHints, CacheContentBlock |
| `src/config/input.rs:293-317` | Multi-turn building | Splice history turns into messages |
| `src/config/input.rs:508-554` | Turn parser | `parse_vim_history_turns()` |
| `src/config/input.rs:561-640` | Block splitter | `split_json_content_blocks()` |
| `src/config/input.rs:651-679` | Cache warm detector | `detect_cache_warm_field()` |

### vim-llm-assistant (VimScript)

| File | Lines | Component |
|------|-------|-----------|
| `autoload/llm.vim:42-53` | Key ordering | Deterministic stable→dynamic order |
| `autoload/llm.vim:94-143` | Turn parser | `llm#parse_history_turns()` |
| `autoload/llm.vim:145-149` | Trim helper | `s:trim_blank_lines()` |
| `autoload/llm.vim:701-707` | Turn integration | Adds to data dict in `llm#run()` |
| `autoload/llm.vim:709-712` | Multiturn flag | `g:llm_multiturn_mode` check |
| `autoload/llm.vim:731-736` | Cache hints | `_cache_hints` field in data dict |
| `autoload/llm.vim:818-897` | Cache warming | `llm#warm_cache()` function |
| `autoload/llm/adapters/aichat.vim:178,299-300` | Env signaling | `AICHAT_MULTITURN_READY=1` |
| `plugin/llm.vim:70` | Warm command | `:LLMWarm` definition |

### Integration Tests

| File | Purpose |
|------|---------|
| `scripts/test-multiturn-cache.sh` | End-to-end multi-turn cache validation |

---

## Appendix C: Cost Impact Estimates

Based on Claude Sonnet 4 pricing ($3/MTok input, $0.30/MTok cached read, $3.75/MTok cache write):

| Scenario | Without Caching | With Caching | Savings |
|----------|----------------|-------------|---------|
| Simple question (5-turn session) | ~35K full = $0.105/req | ~5K full + 30K cached = $0.024/req | **77%** |
| Heavy coding (15-turn, tools) | ~80K full = $0.240/req | ~8K full + 72K cached = $0.046/req | **81%** |
| Ralph loop (25+ turns, 25 tools) | ~120K full = $0.360/req | ~12K full + 108K cached = $0.069/req | **81%** |

Cache warming adds one-time write cost (3.75×) amortized across all subsequent reads (0.1×).

---

## Appendix D: Configuration Reference

### Environment Variables

| Variable | Set By | Read By | Purpose |
|----------|--------|---------|---------|
| `AICHAT_CURSOR_LINE` | vim adapter | aichat input.rs | Cursor line (excluded from JSON) |
| `AICHAT_CURSOR_COL` | vim adapter | aichat input.rs | Cursor column (excluded from JSON) |
| `AICHAT_MULTITURN_READY` | vim adapter | (informational) | Signals history turns are in JSON |
| `AICHAT_CACHE_WARM` | vim adapter | aichat input.rs | Alternative cache warm signal |

### Vim Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `g:llm_multiturn_mode` | 0 | When 1, omits flat `llm_history` from JSON when turns are available |

### Token Minimums

Cache breakpoints are silently ignored if content below the threshold:

| Model Family | Minimum Tokens | Typical Content | Meets Threshold? |
|-------------|---------------|-----------------|------------------|
| Claude Sonnet 4/4.5/4.6 | 1,024 | System: ~10K, Tools: ~8K | ✅ |
| Claude Haiku 3.5 | 2,048 | System: ~10K, Tools: ~8K | ✅ |
| Claude Haiku 4.5, Opus 4.5/4.6 | 4,096 | System: ~10K, Tools: ~8K | ✅ |
