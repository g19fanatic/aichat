# Prompt Caching Implementation Plan

**Date**: 2025-07-16
**Scope**: aichat (Rust) + vim-llm-assistant (VimScript)
**Based on**: Findings 01–08 (API mechanics, strategies, code analysis, opportunities)

---

## Executive Summary

Both projects are **80% of the way** to optimal prompt caching. The aichat Claude client already caches the system prompt and last conversation message with explicit breakpoints, plus enables automatic caching. vim-llm-assistant already orders content stable→dynamic for cache-friendly prefixes. The remaining 20% requires ~150 lines of Rust code and ~80 lines of VimScript to unlock **70-90% input token cost savings** per request.

### Current Savings (Already Working)
- System prompt (~10K tokens): ✅ Cached via explicit breakpoint
- Last message: ✅ Cached (enables multi-turn prefix reuse)
- Automatic mode: ✅ Enabled (API handles breakpoint movement)

### Additional Savings After Implementation
- Tool definitions (~8K tokens): 🔧 NOT cached → **8K tokens × 90% savings/request**
- Long conversations (20+ blocks): 🔧 Miss due to lookback limit → **50-100K tokens recoverable**
- Dynamic cursor/prompt in JSON: 🔧 Invalidates entire user message cache → **20-50K tokens recoverable**

### Estimated Cost Impact

| Scenario | Before (per request) | After (per request) | Savings |
|----------|---------------------|--------------------|---------| 
| Simple question (5-turn session) | ~35K full-price tokens | ~5K full + 30K cached | **77%** |
| Heavy coding (15-turn, tools) | ~80K full-price tokens | ~8K full + 72K cached | **82%** |
| Ralph loop (25+ turns, 25 tools) | ~120K full-price tokens | ~12K full + 108K cached | **87%** |

At Claude Sonnet 4 pricing ($3/MTok input):
- Before: $0.36/request (120K tokens)
- After: $0.036 (cached) + $0.036 (12K full) + cache write amortized ≈ **$0.08/request**
- **Net: 78% cost reduction for agentic workflows**

---

## Current State Assessment

### What's Already Working (Don't Break These)

| Feature | Location | Status |
|---------|----------|--------|
| System prompt caching | `claude.rs:312-318` | ✅ Explicit breakpoint |
| Last message caching | `claude.rs:319-335` | ✅ Explicit breakpoint |
| Automatic caching mode | `claude.rs:311` | ✅ Top-level `cache_control` |
| Extended TTL header | `claude.rs:63` | ✅ For Claude 4+ models |
| No beta header needed | `claude.rs:42-67` | ✅ Caching is GA |
| Input ordering | `input.rs:77` | ✅ Documents → reply → prompt |
| JSON key ordering | `llm.vim:32-70` | ✅ Stable → dynamic |
| Content comment annotations | `llm.vim`, `input.rs` | ✅ Intent documented |

### What's Missing (Implementation Targets)

| Gap | Impact | Project | Priority |
|-----|--------|---------|----------|
| Tools not cached | 8K tokens wasted/request | aichat | **P0** |
| No cache metrics display | Can't verify/debug | aichat | **P0** |
| Breakpoint budget wasted (4→3 usable) | Lost optimization slot | aichat | **P1** |
| No intermediate breakpoints | Miss in long sessions | aichat | **P1** |
| cursor_line/cursor_col in JSON | Invalidates user msg cache | vim-llm-assistant | **P1** |
| History as flat text | Can't multi-turn cache | vim-llm-assistant | **P2** |
| No cache boundary signaling | aichat can't split blocks | vim-llm-assistant | **P2** |
| No cache warming | First request always cold | vim-llm-assistant | **P3** |

---

## Phase 1: aichat Critical Fixes (PR #1 — ~40 lines, 30 minutes)

**Theme**: Maximize cache hits with minimal code changes. Highest ROI.

### Change 1.1: Add `cache_control` to Tool Definitions

**Impact**: ⭐⭐⭐ (8K tokens × 90% savings = ~$0.02/request saved)
**Effort**: 8 lines
**File**: `src/client/claude.rs`
**Location**: Inside `if should_cache` block (after line 311)

```rust
// Add cache_control to last tool for tool-level caching
// Tools are first in cache hierarchy: tools → system → messages
// Cached tools remain valid even if system/messages change
if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
    if let Some(last_tool) = tools_arr.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}
```

**Reference**: Bedrock client does this correctly at `bedrock.rs:614-619`.

**Why this matters**: The vim-llm-assistant workflow loads 20+ tools (~8K tokens) on every request. These NEVER change between requests but currently aren't cached, meaning 8K tokens are re-processed at full price every single time.

---

### Change 1.2: Extract and Display Cache Metrics

**Impact**: ⭐⭐⭐ (Essential for verification — without this you cannot know caching works)
**Effort**: 25 lines across 2 files
**Files**: `src/client/claude.rs:386-393`, `src/client/common.rs:467-490`

**In `claude_extract_chat_completions` (replace lines 386-393):**

```rust
// Extract Anthropic cache metrics from response usage
let cache_extra = {
    let cache_creation = data["usage"]["cache_creation_input_tokens"].as_u64();
    let cache_read = data["usage"]["cache_read_input_tokens"].as_u64();
    if cache_creation.is_some() || cache_read.is_some() {
        Some(json!({
            "cache_creation_input_tokens": cache_creation.unwrap_or(0),
            "cache_read_input_tokens": cache_read.unwrap_or(0),
        }))
    } else {
        None
    }
};

let output = ChatCompletionsOutput {
    text: text.to_string(),
    tool_calls,
    id: data["id"].as_str().map(|v| v.to_string()),
    input_tokens: data["usage"]["input_tokens"].as_u64(),
    output_tokens: data["usage"]["output_tokens"].as_u64(),
    extra: cache_extra,
};
```

**In `common.rs` (after existing Bifrost cache display, ~line 490):**

```rust
// Anthropic prompt cache metrics
if let Some(extra) = &extra {
    if let (Some(cache_creation), Some(cache_read)) = (
        extra.get("cache_creation_input_tokens").and_then(|v| v.as_u64()),
        extra.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
    ) {
        if cache_creation > 0 || cache_read > 0 {
            eprintln!(
                "📦 Cache: {} tokens read, {} tokens written",
                cache_read, cache_creation
            );
        }
    }
}
```

**Verification**: After implementing, make two identical requests. First should show `cache_creation > 0, cache_read = 0`. Second should show `cache_read > 0, cache_creation = 0`.

---

### Change 1.3: Deterministic Tool Ordering

**Impact**: ⭐ (Defensive — prevents future silent cache invalidation)
**Effort**: 4 lines
**File**: `src/client/claude.rs:289-303`

```rust
if let Some(functions) = functions {
    let mut tools: Vec<Value> = functions
        .iter()
        .map(|v| {
            json!({
                "name": v.name,
                "description": v.description,
                "input_schema": v.parameters,
            })
        })
        .collect();
    // Deterministic ordering for cache stability
    tools.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
    });
    body["tools"] = json!(tools);
}
```

**Why**: If tool declaration order ever varies (e.g., from HashMap iteration), the tool cache silently misses. This is a zero-risk guard.

---

### Change 1.4: Remove Top-Level `cache_control`

**Impact**: ⭐⭐ (Frees 1 of 4 breakpoint slots for explicit use)
**Effort**: 1 line deletion
**File**: `src/client/claude.rs:311`

```rust
// DELETE this line:
body["cache_control"] = json!({"type": "ephemeral"});
```

**Rationale**: With explicit breakpoints on tools, system, and last message, automatic mode provides no additional value but consumes 1 of 4 available breakpoint slots. Removing it frees a slot for intermediate conversation breakpoints (Change 2.1).

**Risk mitigation**: Test before and after. The explicit breakpoints (system + last message) already cover what automatic mode would do. If caching degrades unexpectedly, restore this line and investigate.

---

## Phase 2: aichat Advanced Caching (PR #2 — ~50 lines, 1 hour)

**Theme**: Handle long conversations and edge cases.

### Change 2.1: Intermediate Conversation Breakpoints

**Impact**: ⭐⭐ (Prevents cache misses in long sessions — 50-100K tokens recoverable)
**Effort**: 20 lines
**File**: `src/client/claude.rs` (after line 335)
**Depends on**: Phase 1 Change 1.4 (needs the freed breakpoint slot)

```rust
// Intermediate breakpoint for long conversations (prevents lookback window miss)
// The 20-block lookback window means conversations growing 20+ blocks between
// turns need a stepping-stone breakpoint to maintain cache hits
if let Some(messages_arr) = body["messages"].as_array().map(|m| m.len()) {
    if messages_arr > 20 {
        // Place breakpoint roughly in the middle on a user message
        let mid_idx = messages_arr / 2;
        let target_idx = (mid_idx..messages_arr.saturating_sub(1))
            .find(|&i| body["messages"][i]["role"] == "user")
            .unwrap_or(mid_idx);

        if let Some(content_str) = body["messages"][target_idx]["content"]
            .as_str()
            .map(|s| s.to_string())
        {
            body["messages"][target_idx]["content"] = json!([{
                "type": "text",
                "text": content_str,
                "cache_control": {"type": "ephemeral"}
            }]);
        } else if body["messages"][target_idx]["content"].is_array() {
            let content_len = body["messages"][target_idx]["content"]
                .as_array()
                .map_or(0, |a| a.len());
            if content_len > 0 {
                body["messages"][target_idx]["content"][content_len - 1]["cache_control"] =
                    json!({"type": "ephemeral"});
            }
        }
    }
}
```

**Breakpoint allocation after Phase 1+2**:
1. Tools (explicit) — Phase 1
2. System prompt (explicit) — already exists
3. Mid-conversation anchor (explicit) — this change
4. Last message (explicit) — already exists

---

### Change 2.2: Extended TTL for System Prompt

**Impact**: ⭐ (System prompt stays cached during idle periods > 5 min)
**Effort**: 6 lines
**File**: `src/client/claude.rs:312-318`

Replace the system message cache logic:

```rust
// Use 1-hour TTL for system prompt on supported models (survives idle periods)
let system_cache_control = if claude_supports_extended_cache(model) {
    json!({"type": "ephemeral", "ttl": "1h"})
} else {
    json!({"type": "ephemeral"})
};

body["system"] = json!([{
    "type": "text",
    "text": system_str,
    "cache_control": system_cache_control
}]);
```

**Cost tradeoff**: Cache writes at 2x instead of 1.25x, but system prompt writes happen rarely (only first request or after 1-hour expiry). Worth it for developers who step away for 10-30 minutes.

---

### Change 2.3: OpenAI-Compatible Tool Caching

**Impact**: ⭐⭐ (Parity for OpenRouter/other Claude providers)
**Effort**: 8 lines
**File**: `src/client/openai.rs:353-386` (inside Claude caching block)

```rust
// Mirror tool caching for Claude-via-OpenRouter
if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
    if let Some(last_tool) = tools_arr.last_mut() {
        if let Some(func) = last_tool.get_mut("function") {
            func["cache_control"] = json!({"type": "ephemeral"});
        }
    }
}
```

---

### Change 2.4: Streaming Cache Metrics (Optional)

**Impact**: ⭐ (Completeness — metrics in streaming mode)
**Effort**: 15-20 lines
**File**: `src/client/claude.rs:85-157`

Add handling for `message_delta` event in the streaming handler to capture the final usage data (which includes cache metrics). This requires extending the `SseHandler` interface.

**Recommendation**: Defer to Phase 3 or later; non-streaming metrics are sufficient for verification.

---

## Phase 3: vim-llm-assistant Quick Wins (PR #1 — ~40 lines, 30 minutes)

**Theme**: Stabilize the JSON payload to maximize aichat's existing caching.

### Change 3.1: Move cursor_line/cursor_col to Environment Variables

**Impact**: ⭐⭐⭐ (Highest single-change impact for vim-llm-assistant)
**Effort**: 15 lines
**Files**: `autoload/llm.vim:525-560`, `autoload/llm/adapters/aichat.vim:161-170`

**Current problem**: cursor_line and cursor_col are inside the JSON file. EVERY cursor movement changes the file content, invalidating the entire user message cache. These tiny values (~5 tokens) force re-processing of 20-50K tokens of stable context.

**In `llm#run()` — remove from data before JSON encode:**
```vim
" Store cursor data for env var passing, remove from JSON payload
let l:cursor_data = {
      \ 'cursor_line': l:data.cursor_line,
      \ 'cursor_col': l:data.cursor_col,
      \ }
" Remove dynamic fields from the cache-stable JSON
call remove(l:data, 'cursor_line')
call remove(l:data, 'cursor_col')
```

**In `aichat.vim` command construction — pass via env vars:**
```vim
" Pass volatile cursor data via environment (not in cached JSON)
let l:cursor_env = 'AICHAT_CURSOR_LINE=' . l:cursor_data.cursor_line
      \ . ' AICHAT_CURSOR_COL=' . l:cursor_data.cursor_col . ' '
let l:cmd = l:cursor_env . l:cmd_extra . 'aichat ...'
```

**aichat side** (minimal change in `input.rs`): Read `AICHAT_CURSOR_LINE` and `AICHAT_CURSOR_COL` from environment and append to the user message AFTER the cached document content.

**Result**: The JSON file content now changes ONLY when actual context changes (new history, different buffers, different prompt). Between cursor movements, the file is byte-for-byte identical → 100% cache hits on the user message block.

---

### Change 3.2: Reorder file_arguments Before active_buffer

**Impact**: ⭐ (Marginal — extends stable prefix slightly)
**Effort**: 2 lines
**File**: `autoload/llm.vim:37-44`

```vim
let l:ordered_keys = [
      \ 'llm_history',      " 1. Large, stable (append-only)
      \ 'buffers',          " 2. Stable within session
      \ 'file_arguments',   " 3. Stable per invocation (MOVED UP)
      \ 'active_buffer',    " 4. Semi-stable (changes with edits)
      \ 'prompt',           " 5. Dynamic
      \ ]
```

**Why**: `file_arguments` is typically stable for an entire question sequence (explicitly attached files). `active_buffer` changes whenever the user edits. Putting the more stable content first extends the cacheable prefix.

---

### Change 3.3: Add Cache Mode Signaling via cmd_extra

**Impact**: ⭐⭐ (Enables aichat to make better caching decisions)
**Effort**: 20 lines
**File**: User's vimrc or dedicated config file

```vim
function! LLMCacheExtra(json_filename, prompt, model) abort
  let l:extras = 'AICHAT_CACHE_MODE=explicit '

  " Signal continuation status for cache behavior
  if exists('g:llm_scratch_bufnr') && bufexists(g:llm_scratch_bufnr)
        \ && len(getbufline(g:llm_scratch_bufnr, 1, '$')) > 1
    let l:extras .= 'AICHAT_IS_CONTINUATION=1 '
  else
    let l:extras .= 'AICHAT_IS_CONTINUATION=0 '
  endif

  return l:extras
endfunction

let g:llm_adapter_cmd_extra = {'aichat': 'LLMCacheExtra'}
```

**aichat side**: Read `AICHAT_CACHE_MODE` to decide between automatic vs explicit-only caching, and `AICHAT_IS_CONTINUATION` to know whether intermediate breakpoints are useful.

---

## Phase 4: vim-llm-assistant Architecture (PR #2 — ~100 lines, 2 hours)

**Theme**: Enable proper multi-turn caching through structured data.

### Change 4.1: Structure History as Turn Array

**Impact**: ⭐⭐⭐ (Enables proper multi-turn API messages → compound cache savings)
**Effort**: 50 lines
**File**: `autoload/llm.vim`

```vim
function! llm#parse_history_turns() abort
  if !exists('g:llm_scratch_bufnr') || !bufexists(g:llm_scratch_bufnr)
    return []
  endif

  let l:lines = getbufline(g:llm_scratch_bufnr, 1, '$')
  let l:turns = []
  let l:current_turn = {}
  let l:collecting = ''
  let l:content = []

  for l:line in l:lines
    if l:line =~# '^==== .* ====$'
      if !empty(l:current_turn)
        if !empty(l:content)
          let l:current_turn[l:collecting] = join(l:content, "\n")
        endif
        call add(l:turns, l:current_turn)
      endif
      let l:timestamp = matchstr(l:line, '^==== \zs.*\ze ====$')
      let l:current_turn = {'timestamp': l:timestamp}
      let l:content = []
      let l:collecting = ''
    elseif l:line =~# '^Prompt: '
      let l:current_turn.user = l:line[8:]
      let l:collecting = 'assistant'
      let l:content = []
    else
      call add(l:content, l:line)
    endif
  endfor

  if !empty(l:current_turn)
    if !empty(l:content)
      let l:current_turn[l:collecting] = join(l:content, "\n")
    endif
    call add(l:turns, l:current_turn)
  endif

  return l:turns
endfunction
```

**Usage in `llm#run()`:**
```vim
let l:data.llm_history_turns = llm#parse_history_turns()
" Keep legacy llm_history field for backward compatibility
let l:data.llm_history = join(getbufline(g:llm_scratch_bufnr, 1, '$'), "\n")
```

**aichat side** (in `input.rs`): When `llm_history_turns` is present in the JSON, convert each turn to a proper user/assistant message pair in the API request. This enables Anthropic's automatic multi-turn caching where prior turns are read from cache and only new turns are written.

---

### Change 4.2: Add Cache Boundary Hints

**Impact**: ⭐⭐ (Tells aichat where to split content blocks for optimal caching)
**Effort**: 10 lines
**File**: `autoload/llm.vim` (in `llm#run()`)

```vim
" Add cache boundary hints for aichat to place breakpoints optimally
let l:data._cache_hints = {
      \ 'breakpoint_after': ['llm_history', 'buffers'],
      \ 'stable_fields': ['llm_history', 'buffers', 'file_arguments'],
      \ 'dynamic_fields': ['prompt'],
      \ }
```

**aichat side**: Parse `_cache_hints` and split the single user message text block into multiple `content` blocks, placing `cache_control` on blocks after the designated breakpoints.

---

### Change 4.3: Cache Warming Command (`:LLMWarm`)

**Impact**: ⭐⭐ (Eliminates cold-start latency on first real request)
**Effort**: 20 lines
**File**: `autoload/llm.vim`, `plugin/llm.vim`

```vim
" In plugin/llm.vim:
command! LLMWarm call llm#warm_cache()

" In autoload/llm.vim:
function! llm#warm_cache() abort
  " Build context exactly as llm#run() would, with a dummy prompt
  let l:data = s:build_context_data()
  let l:data._cache_warm = 1
  let l:data.prompt = 'warmup'
  
  let l:json_data = llm#encode(l:data)
  let l:tempfile = tempname()
  call writefile(split(l:json_data, "\n"), l:tempfile)
  
  " aichat detects _cache_warm and uses max_tokens: 0
  echom '[LLM] Warming cache...'
  " Fire async, report result
endfunction
```

**aichat side**: Detect `_cache_warm` field (or `AICHAT_CACHE_WARM=1` env var) and set `max_tokens: 0` in the API request. Return only usage data showing cache_creation_input_tokens.

---

## Phase 5: Full Multi-Turn Architecture (Future — Major)

**Theme**: Fundamental restructuring for maximum caching efficiency.

### Vision

```
Current: Single flat user message (all context as one text block)
Future:  Proper multi-turn messages (history as user/assistant pairs + context as separate blocks)
```

### Why This Matters

With proper multi-turn messages, Anthropic's caching works at maximum efficiency:
- Turn 1: Cache entire prefix (system + tools + user1)
- Turn 2: Read turn 1 from cache, write turn 2 delta only
- Turn N: Read turns 1→(N-1) from cache, write turn N delta only

Each subsequent request pays only for the NEW content's tokens. Savings compound with conversation length.

### Implementation Path

1. **Phase 5a**: vim-llm-assistant maintains parallel internal turn array (Change 4.1 enables this)
2. **Phase 5b**: aichat parses `llm_history_turns` and builds proper multi-turn messages
3. **Phase 5c**: vim-llm-assistant stops including flat `llm_history` once aichat supports turns
4. **Phase 5d**: aichat places automatic cache breakpoint at conversation boundary

### Estimated Savings at Phase 5 Completion

| Turn # | Tokens Processed (Before) | Tokens Processed (After) | Savings |
|--------|--------------------------|--------------------------|---------|
| 1 | 25K (system+tools+context) | 25K (first request, no cache) | 0% |
| 2 | 30K (all + turn 1) | 5K new + 25K cached | 75% |
| 5 | 45K (all + turns 1-4) | 5K new + 40K cached | 80% |
| 10 | 70K (all + turns 1-9) | 5K new + 65K cached | 85% |
| 20 | 120K (all + turns 1-19) | 5K new + 115K cached | 91% |

---

## Dependency Graph

```
Phase 1 (aichat quick wins)
├── 1.1 Tool caching ─────────────────── Independent
├── 1.2 Cache metrics ─────────────────── Independent
├── 1.3 Deterministic tools ────────────── Independent
└── 1.4 Remove auto cache_control ─────── Prerequisite for 2.1

Phase 2 (aichat advanced)
├── 2.1 Intermediate breakpoints ──────── Depends on 1.4
├── 2.2 Extended TTL ──────────────────── Independent
├── 2.3 OpenAI-compatible ─────────────── Independent
└── 2.4 Streaming metrics ────────────── Depends on 1.2

Phase 3 (vim quick wins)
├── 3.1 Remove cursor from JSON ───────── Independent (needs minor aichat change)
├── 3.2 Reorder keys ──────────────────── Independent
└── 3.3 Cache mode signaling ──────────── Independent (needs aichat env var reads)

Phase 4 (vim architecture)
├── 4.1 Structured history ────────────── Independent (backward compatible)
├── 4.2 Cache hints ───────────────────── Depends on aichat parsing support
└── 4.3 Cache warming ────────────────── Depends on aichat max_tokens:0 support

Phase 5 (multi-turn)
└── Full multi-turn ───────────────────── Depends on 4.1 + aichat message building
```

---

## Verification Strategy

### Immediate Verification (After Phase 1)

1. **Make request 1**: Note `cache_creation_input_tokens` in metrics output
2. **Make identical request 2** (within 5 min): Should show `cache_read_input_tokens ≈ cache_creation from request 1`
3. **Verify tool caching specifically**: Compare `cache_creation` with vs. without tools — the difference equals tool tokens

### Ongoing Monitoring

| Metric | Healthy | Broken |
|--------|---------|--------|
| `cache_read_input_tokens` | > 0 on most requests | Always 0 |
| `cache_creation_input_tokens` | > 0 only on first request or after changes | > 0 on every request |
| `input_tokens` (uncached) | Small (just dynamic suffix) | Large (entire content) |
| Hit rate formula | `read / (read + creation + input)` > 70% | < 30% |

### Cache Diagnostics (Beta)

For debugging persistent misses, use the `cache-diagnosis-2026-04-07` beta header:
```rust
// In claude.rs headers (temporarily for debugging):
headers.insert("anthropic-beta", "cache-diagnosis-2026-04-07");
// Add to request body:
body["diagnostics"] = json!({"previous_message_id": prev_id});
```

Response includes `diagnostics.cache_miss_reason` identifying the exact divergence point.

---

## Risk Matrix

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Tool count below 1024-token minimum | Low (20+ tools = ~8K tokens) | Tool cache silently ignored | Check metrics; if 0, tools are too small |
| Breakpoint budget exhaustion (>4) | Medium (if auto mode kept) | Unpredictable behavior | Phase 1.4 removes auto mode |
| JSON key order instability | Very Low (VimScript dicts are ordered) | Full cache miss | Phase 1.3 sorts tools defensively |
| cursor env vars not read by aichat | Medium (requires aichat code) | No benefit from Phase 3.1 | Implement aichat-side simultaneously |
| 5-min TTL expires during long think | Medium | Cold start next request | Phase 2.2 uses 1-hour TTL on system |
| Intermediate breakpoint on wrong msg | Low | Wasted breakpoint slot | Place on user messages only |
| Multi-turn parsing bug | Medium | Corrupted message array | Keep flat history as fallback |

---

## Implementation Timeline (Recommended)

| Week | Phase | Deliverable | Effort |
|------|-------|-------------|--------|
| 1 | Phase 1 | aichat PR #1: tool caching + metrics + deterministic + remove auto | 2 hours |
| 1 | Phase 3.1-3.2 | vim PR #1: remove cursor from JSON + reorder keys | 30 min |
| 2 | Phase 2 | aichat PR #2: intermediate breakpoints + extended TTL + OpenAI parity | 2 hours |
| 2 | Phase 3.3 | vim PR #2: cache mode signaling | 30 min |
| 3 | Phase 4.1-4.2 | vim PR #3: structured history + cache hints | 2 hours |
| 3 | aichat support | aichat PR #3: parse cache_hints, read env vars, split content blocks | 3 hours |
| 4+ | Phase 5 | Full multi-turn architecture (both projects) | 8+ hours |

**Total for 90% of the benefit (Phases 1-3)**: ~5 hours across both projects.

---

## Specific File:Line Reference Index

### aichat Changes

| File | Lines | Change | Phase |
|------|-------|--------|-------|
| `src/client/claude.rs:289-303` | Tool array construction | Add sort_by for deterministic order | 1.3 |
| `src/client/claude.rs:306-335` | Cache logic block | Add tool cache_control, remove auto | 1.1, 1.4 |
| `src/client/claude.rs:312-318` | System message caching | Add extended TTL conditional | 2.2 |
| `src/client/claude.rs:335+` | After last-message logic | Add intermediate breakpoint | 2.1 |
| `src/client/claude.rs:386-393` | Response extraction | Add cache metrics to extra | 1.2 |
| `src/client/common.rs:467-490` | Gateway info display | Add cache metrics display | 1.2 |
| `src/client/openai.rs:353-386` | Claude-via-OpenAI caching | Add tool cache_control | 2.3 |
| `src/config/input.rs:77+` | Document assembly | Read AICHAT_CURSOR_* env vars | 3.1 (aichat side) |

### vim-llm-assistant Changes

| File | Lines | Change | Phase |
|------|-------|--------|-------|
| `autoload/llm.vim:37-44` | Key ordering array | Move file_arguments before active_buffer | 3.2 |
| `autoload/llm.vim:525-560` | Context assembly | Remove cursor_line/col from data dict | 3.1 |
| `autoload/llm.vim:525-560` | Context assembly | Add _cache_hints field | 4.2 |
| `autoload/llm.vim` (new func) | History parsing | New llm#parse_history_turns() | 4.1 |
| `autoload/llm.vim` (new func) | Cache warming | New llm#warm_cache() | 4.3 |
| `autoload/llm/adapters/aichat.vim:161-170` | Command construction | Pass cursor via env vars | 3.1 |
| `plugin/llm.vim` | Command definitions | Add :LLMWarm command | 4.3 |

---

## Success Criteria

After implementing Phases 1-3, the following should be observable:

1. **Cache metrics appear in stderr**: `📦 Cache: N tokens read, M tokens written`
2. **Second identical request shows high cache_read**: Most tokens served from cache
3. **Tool cache hit independent of message changes**: Changing user message doesn't invalidate tools
4. **Cursor movement doesn't invalidate**: Two requests differing only in cursor position hit cache
5. **Cost reduction visible in API dashboard**: 70%+ reduction in input token charges

After implementing Phase 4-5:

6. **Multi-turn compound savings**: Each conversation turn adds only ~5K new tokens
7. **Cache warming works**: `:LLMWarm` shows cache_creation without output tokens
8. **Long sessions stay cached**: 20+ turn conversations maintain >80% hit rate

---

## Appendix: Quick Reference Card

### The 4 Breakpoint Budget (After Phase 1+2)

```
Breakpoint 1: Last tool in tools[] array          → caches ALL tools
Breakpoint 2: System message text block           → caches system prompt + tools prefix
Breakpoint 3: Mid-conversation user message       → stepping stone for long convos
Breakpoint 4: Last message before current         → caches conversation prefix
```

### Cache Hierarchy (Invalidation Rules)

```
Change tools     → invalidates: tools ✘, system ✘, messages ✘
Change system    → invalidates: tools ✓, system ✘, messages ✘
Change messages  → invalidates: tools ✓, system ✓, messages ✘ (only after change point)
Change last msg  → invalidates: tools ✓, system ✓, messages ✓ (prefix still cached)
```

### Token Minimum Requirements

| Model Family | Minimum | Typical aichat Content | Meets? |
|-------------|---------|----------------------|--------|
| Claude Sonnet 4/4.5/4.6 | 1,024 | System: ~10K, Tools: ~8K | ✅✅ |
| Claude Haiku 3.5 | 2,048 | System: ~10K, Tools: ~8K | ✅✅ |
| Claude Haiku 4.5, Opus 4.5/4.6 | 4,096 | System: ~10K, Tools: ~8K | ✅ |

### Pricing Quick Math (Claude Sonnet 4, $3/MTok)

| Scenario | No Caching | With Caching | Monthly Savings (100 req/day) |
|----------|-----------|-------------|-------------------------------|
| Light (25K tokens) | $0.075/req | $0.015/req | $180/month |
| Medium (50K tokens) | $0.150/req | $0.025/req | $375/month |
| Heavy (100K tokens) | $0.300/req | $0.045/req | $765/month |
