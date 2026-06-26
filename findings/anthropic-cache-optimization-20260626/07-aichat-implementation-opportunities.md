# Implementation Opportunities in aichat's Rust Code

**Task**: Identify specific code changes needed in aichat to add cache_control markers to appropriate message blocks
**Date**: 2025-07-16
**Based on**: Findings 01 (API mechanics), 02 (strategies), 03 (current client analysis), 05 (token patterns)

---

## Executive Summary

aichat's Claude client (`src/client/claude.rs`) already implements basic prompt caching with system message and last-message breakpoints, plus top-level automatic caching. However, several high-impact improvements are available:

1. **Add tool caching** (highest ROI — tools are large, static, and first in cache hierarchy)
2. **Extract and display cache metrics** (essential for debugging/verification)
3. **Fix the invalid top-level `cache_control` placement** (harmless but incorrect)
4. **Add intermediate conversation breakpoints** (prevents misses in long sessions)
5. **Mirror improvements to OpenAI-compatible client** (Claude-via-OpenRouter)

Total estimated code change: ~80-120 lines across 3 files.

---

## Opportunity 1: Add cache_control to Tool Definitions

### Priority: **CRITICAL** (Highest ROI)

### Problem

Tools are serialized into the request body at `claude.rs:289-303` but never receive a `cache_control` marker. Tool definitions can be 5-20K tokens (especially with 20+ tools as in the vim-llm-assistant use case). Since tools are **first in the cache hierarchy** (tools → system → messages), a cached tools prefix remains valid even when system prompt or messages change.

### Current Code (`claude.rs:289-303`)

```rust
if let Some(functions) = functions {
    body["tools"] = functions
        .iter()
        .map(|v| {
            json!({
                "name": v.name,
                "description": v.description,
                "input_schema": v.parameters,
            })
        })
        .collect();
}
```

### Proposed Change

```rust
if let Some(functions) = functions {
    let tools: Vec<Value> = functions
        .iter()
        .map(|v| {
            json!({
                "name": v.name,
                "description": v.description,
                "input_schema": v.parameters,
            })
        })
        .collect();
    body["tools"] = json!(tools);
}
```

Then, inside the `if should_cache` block (after line 311), add:

```rust
// Add cache_control to last tool for tool-level caching
if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
    if let Some(last_tool) = tools_arr.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}
```

### Reference Implementation

The Bedrock client already does this correctly at `bedrock.rs:614-619`:
```rust
// Add cachePoint after tools
if let Some(tools_arr) = body.get_mut("toolConfig")
    .and_then(|tc| tc.get_mut("tools"))
    .and_then(|t| t.as_array_mut())
{
    tools_arr.push(cache_point);
}
```

### Impact

- **Savings**: 5-20K tokens cached per request at 90% discount
- **Stability**: Tools rarely change during a session — cache hit rate should be near 100%
- **Independence**: Even if system prompt or messages change, tools cache persists
- **Effort**: ~8 lines of code

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 306-335 (inside `if should_cache` block) | Add 5-8 lines for tool caching |

---

## Opportunity 2: Extract and Display Cache Metrics from Response

### Priority: **HIGH** (Essential for debugging)

### Problem

The response extraction function `claude_extract_chat_completions` (`claude.rs:341-395`) captures `input_tokens` and `output_tokens` but ignores the cache-specific fields that Anthropic returns:

```json
{
  "usage": {
    "input_tokens": 50,
    "cache_creation_input_tokens": 5120,
    "cache_read_input_tokens": 0,
    "output_tokens": 503
  }
}
```

Without these fields, there's **no way to verify caching is working**.

### Current Code (`claude.rs:386-393`)

```rust
let output = ChatCompletionsOutput {
    text: text.to_string(),
    tool_calls,
    id: data["id"].as_str().map(|v| v.to_string()),
    input_tokens: data["usage"]["input_tokens"].as_u64(),
    output_tokens: data["usage"]["output_tokens"].as_u64(),
    extra: None,
};
```

### Proposed Change

```rust
// Build cache metrics extra data
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

### Downstream Display (`common.rs:467-490`)

The existing `show_gateway_info` infrastructure already displays cache info from the `extra` field. Extend it to also show Anthropic prompt cache metrics:

```rust
// After the existing Bifrost cache display block (common.rs:~490):
if let Some(extra) = &extra {
    if show_gateway_info {
        // Existing Bifrost cache display...
        
        // NEW: Anthropic prompt cache metrics
        if let (Some(cache_creation), Some(cache_read)) = (
            extra.get("cache_creation_input_tokens").and_then(|v| v.as_u64()),
            extra.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
        ) {
            if cache_creation > 0 || cache_read > 0 {
                eprintln!(
                    "📦 Cache: {} read, {} written",
                    cache_read, cache_creation
                );
            }
        }
    }
}
```

### Impact

- **Observability**: Users can verify caching is working and debug misses
- **No behavioral change**: Only adds logging when `show_gateway_info` is enabled
- **Effort**: ~25 lines across 2 files

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 386-393 | Build cache metrics into `extra` field |
| `src/client/common.rs` | 467-490 | Display cache metrics alongside existing Bifrost info |

---

## Opportunity 3: Fix Invalid Top-Level `cache_control`

### Priority: **MEDIUM** (Correctness fix)

### Problem

At `claude.rs:311`:
```rust
body["cache_control"] = json!({"type": "ephemeral"});
```

This sets a **top-level** `cache_control` field on the request body. According to Anthropic's API documentation, this is how you enable **automatic caching mode** — the API automatically selects optimal cache breakpoints.

However, setting BOTH automatic mode AND explicit per-block markers means:
- Automatic mode uses 1 of the 4 available breakpoint slots
- The behavior becomes less predictable (API adds breakpoints beyond your explicit ones)
- You lose fine-grained control over what gets cached

### Proposed Change: Remove or Make Conditional

**Option A (Recommended)**: Remove top-level `cache_control` entirely, rely on explicit breakpoints only:

```rust
if should_cache {
    // Removed: body["cache_control"] = json!({"type": "ephemeral"});
    // Use explicit breakpoints only for predictable cache behavior
    
    // System message cache breakpoint
    if let Some(system_str) = body.get("system").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        // ... existing code ...
    }
    // ... rest of caching logic ...
}
```

**Option B**: Keep it but only for simple cases (single-message, no explicit markers):

```rust
if should_cache {
    // Only use automatic caching when we don't have enough content for explicit breakpoints
    let has_system = body.get("system").is_some();
    let has_tools = body.get("tools").is_some();
    if !has_system && !has_tools {
        // Automatic mode as fallback when explicit breakpoints aren't viable
        body["cache_control"] = json!({"type": "ephemeral"});
    }
    // ... explicit breakpoints for system, tools, messages ...
}
```

### Impact

- **Predictability**: Explicit-only caching gives deterministic breakpoint placement
- **Breakpoint budget**: Frees up 1 of 4 slots for user-controlled placement
- **Risk**: Low — the explicit breakpoints already cover the same ground

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 311 | Remove or conditionalize top-level `cache_control` |

---

## Opportunity 4: Add Intermediate Conversation Breakpoints

### Priority: **MEDIUM** (Important for long sessions)

### Problem

Only the **last message** gets a cache breakpoint (`claude.rs:315-335`). In long conversations (20+ message blocks), if the conversation grows by more than 20 blocks between turns (common with tool-use where each tool call generates assistant+user message pairs), the 20-block lookback window won't find the previous breakpoint, causing a full cache miss.

### Proposed Change

Add a "stepping stone" breakpoint approximately every 15-18 messages to ensure the lookback window always finds a prior write:

```rust
// After the last-message cache breakpoint logic (claude.rs:~335), add:

// Add intermediate stepping-stone breakpoints for long conversations
if let Some(messages_arr) = body["messages"].as_array().map(|m| m.len()) {
    if messages_arr > 20 {
        // Place a breakpoint roughly in the middle of the conversation
        // to ensure lookback window always finds a prior cache entry
        let mid_idx = messages_arr / 2;
        // Only place on user messages (cache_control on assistant messages is less useful)
        let target_idx = (mid_idx..messages_arr - 1)
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

### Breakpoint Budget Consideration

With this approach, the 4 breakpoints would be allocated:
1. Tools (if present)
2. System prompt
3. Mid-conversation anchor (if conversation > 20 blocks)
4. Last message (automatic or explicit)

If top-level `cache_control` is removed (Opportunity 3), all 4 slots are available for explicit placement.

### Impact

- **Prevents cache misses** in long agentic sessions (ralph loops, multi-tool conversations)
- **Estimated savings**: 50-100K tokens per request in long sessions that would otherwise miss
- **Effort**: ~20 lines of code

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | After line 335 | Add intermediate breakpoint logic |

---

## Opportunity 5: Mirror Tool Caching to OpenAI-Compatible Client

### Priority: **MEDIUM** (Parity with direct Claude client)

### Problem

The OpenAI-compatible client (`openai.rs:346-386`) applies caching for Claude models served via providers like OpenRouter. It caches system messages and the last message but **does not cache tools** — same gap as the direct Claude client.

### Current Code (`openai.rs:353-356`)

The caching block for OpenAI-compatible Claude only handles system and last message.

### Proposed Change

After the system message caching block (`openai.rs:~366`), add:

```rust
// Add cache_control to last tool for tool-level caching
if has_tools {
    if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
        if let Some(last_tool) = tools_arr.last_mut() {
            if let Some(func) = last_tool.get_mut("function") {
                // OpenAI format wraps tools in {"type": "function", "function": {...}}
                func["cache_control"] = json!({"type": "ephemeral"});
            }
        }
    }
}
```

### Impact

- **Parity**: Users on OpenRouter get the same caching benefits
- **Effort**: ~8 lines of code

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/openai.rs` | 353-386 (inside Claude caching block) | Add tool cache_control |

---

## Opportunity 6: Deterministic Tool Ordering

### Priority: **LOW-MEDIUM** (Insurance against future issues)

### Problem

The tool selection logic (`config/mod.rs:1654-1700`) uses `HashSet<String>` for tool name filtering, then filters the declarations list in declaration order. Currently, declarations come from a JSON file that's read once and is presumably stable. However, if tool ordering ever became non-deterministic (e.g., from agent functions being merged), cache invalidation would occur silently.

### Current Flow

```
config/mod.rs:1654 select_functions()
  → Filters declarations by role's use_tools list
  → Returns Vec<FunctionDeclaration> in declaration file order
    → claude.rs:289 iterates to build tools JSON
```

### Proposed Change (Defensive)

After building the tools array in `claude.rs`, sort it by name:

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
    // Sort tools deterministically for cache stability
    tools.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or(""))
    });
    body["tools"] = json!(tools);
}
```

### Impact

- **Defensive**: Prevents silent cache invalidation if tool ordering changes
- **No functional change**: Tool order doesn't affect LLM behavior
- **Risk**: None — purely an ordering guarantee
- **Effort**: ~4 lines

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 289-303 | Add `.sort_by()` after building tools vec |

---

## Opportunity 7: Extended TTL for System Prompt on Claude 4+

### Priority: **LOW** (Marginal improvement)

### Problem

The extended-cache-ttl beta header is already sent for Claude 4+ models (`claude.rs:63`), but the explicit `cache_control` markers on system and messages all use `{"type": "ephemeral"}` which gives the default 5-minute TTL. For system prompts that are highly stable (role definitions, skills), a 1-hour TTL would be more appropriate.

### Proposed Change

When the model supports extended cache, use 1-hour TTL on the system prompt:

```rust
let cache_control = if claude_supports_extended_cache(model) {
    json!({"type": "ephemeral", "ttl": "1h"})
} else {
    json!({"type": "ephemeral"})
};

// Use this for system message:
body["system"] = json!([{
    "type": "text",
    "text": system_str,
    "cache_control": cache_control
}]);
```

Keep the standard 5-minute TTL for last-message and mid-conversation breakpoints (since those change more frequently).

### TTL Ordering Requirement

Anthropic requires **longer TTL before shorter TTL** in the prefix. Since the cache hierarchy is tools → system → messages, using 1h on system and 5m on messages satisfies this constraint naturally. If tools also get 1h TTL, the ordering is: tools(1h) → system(1h) → messages(5m) ✓.

### Impact

- **Benefit**: System prompt stays cached for 1 hour even without hits (useful for intermittent usage patterns — e.g., developer stepping away for 10-30 min)
- **Cost**: Cache writes are 2x base price instead of 1.25x (but reads are the same 0.1x)
- **Effort**: ~6 lines

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 312-318 | Use conditional TTL for system prompt |

---

## Opportunity 8: Streaming Response Cache Metrics

### Priority: **LOW** (Completeness)

### Problem

The streaming handler (`claude_chat_completions_streaming`, `claude.rs:85-157`) processes SSE events but doesn't capture the final `message_delta` event's usage data which contains cache metrics. The usage data in the `message_stop` event includes the same cache fields.

### Proposed Change

In the streaming handler's match on `typ`, add handling for `message_delta`:

```rust
"message_delta" => {
    // Capture usage data from stream end (includes cache metrics)
    if let Some(usage) = data.get("usage") {
        // This data is available in the final event
        // but currently not captured in streaming mode
        // TODO: Propagate through SseHandler for display
    }
}
```

### Impact

- **Completeness**: Cache metrics visible in streaming mode too
- **Complexity**: Higher than non-streaming (requires SseHandler modifications)
- **Effort**: ~15-20 lines + handler struct changes

### File Locations

| File | Lines | Change |
|------|-------|--------|
| `src/client/claude.rs` | 85-157 | Add `message_delta` handling |
| `src/client/common.rs` | SseHandler | Add cache metrics field |

---

## Implementation Order (Prioritized by Impact/Effort)

| # | Opportunity | Impact | Effort | Dependencies |
|---|-------------|--------|--------|--------------|
| 1 | Tool caching | ⭐⭐⭐ | 8 lines | None |
| 2 | Cache metrics extraction + display | ⭐⭐⭐ | 25 lines | None |
| 3 | Remove top-level cache_control | ⭐⭐ | 1 line delete | Should test after #1 |
| 4 | Intermediate breakpoints | ⭐⭐ | 20 lines | #3 (frees breakpoint slot) |
| 5 | OpenAI-compatible tool caching | ⭐⭐ | 8 lines | None (independent) |
| 6 | Deterministic tool ordering | ⭐ | 4 lines | None |
| 7 | Extended TTL on system prompt | ⭐ | 6 lines | None |
| 8 | Streaming cache metrics | ⭐ | 20+ lines | #2 |

**Recommended first PR**: Opportunities 1 + 2 together (~33 lines, highest combined value).

---

## Complete Patch Preview (Opportunity 1 + 2 Combined)

The minimal high-impact change modifies only `src/client/claude.rs`:

### In `claude_build_chat_completions_body` (after line 311, inside `if should_cache`):

```rust
// Add cache_control to last tool for tool-level caching (tools are first in hierarchy)
if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
    if let Some(last_tool) = tools_arr.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}
```

### In `claude_extract_chat_completions` (replace lines 386-393):

```rust
// Extract cache metrics from Anthropic response
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

### In `call_chat_completions` (`common.rs`, after Bifrost cache display):

```rust
// Anthropic prompt cache metrics display
if let (Some(cache_creation), Some(cache_read)) = (
    extra.get("cache_creation_input_tokens").and_then(|v| v.as_u64()),
    extra.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
) {
    if cache_creation > 0 || cache_read > 0 {
        let total_cached = cache_creation + cache_read;
        let pct = if let Some(input) = ret.input_tokens {
            let grand_total = input + total_cached;
            if grand_total > 0 { cache_read * 100 / grand_total } else { 0 }
        } else { 0 };
        eprintln!("📦 Cache: {} read, {} written ({}% hit rate)", cache_read, cache_creation, pct);
    }
}
```

---

## Risks and Considerations

### Risk 1: Breakpoint Budget Exhaustion

With 4 explicit breakpoints max, the current allocation would be:
- Automatic (top-level): 1 slot (if kept)
- System prompt: 1 slot
- Tools: 1 slot (new)
- Last message: 1 slot

If top-level automatic is kept AND all explicit markers are used, that's 4 slots fully consumed, leaving none for intermediate breakpoints. **Resolution**: Remove top-level `cache_control` (Opportunity 3) to free a slot.

### Risk 2: Token Minimum Not Met

If tools are fewer than 3-4 definitions (< ~1024 tokens for Sonnet), the cache_control marker will be silently ignored. No error occurs.

**Resolution**: This is acceptable — the marker costs nothing and is simply ignored when below threshold.

### Risk 3: JSON Serialization Stability

Rust's `serde_json` serializes struct fields in definition order, and `json!()` macro preserves insertion order. This is deterministic. The `input_schema` field (from FunctionDeclaration) comes from a parsed JSON file that's read once per process — also stable.

**Resolution**: No action needed; Rust's serialization is inherently deterministic.

### Risk 4: Extended TTL Cost

Using 1-hour TTL (Opportunity 7) costs 2x base input price for cache writes vs 1.25x for 5-minute writes. For a 15K token system prompt on Claude Sonnet 4 ($3/MTok):
- 5-min write: $0.056 per write
- 1-hour write: $0.090 per write
- Each cache read: $0.0045

Break-even for 1h vs 5m is after ~1 cache read in cases where the 5m TTL would have expired (intermittent usage). For continuous usage, 5m is cheaper.

---

## Existing Infrastructure to Leverage

| Component | Location | How to Use |
|-----------|----------|------------|
| `extra` field on `ChatCompletionsOutput` | `common.rs:340` | Store cache metrics as JSON |
| `show_gateway_info` config | `config/mod.rs:148` | Gate cache metric display |
| Bifrost cache display pattern | `common.rs:479-490` | Follow same pattern for Anthropic metrics |
| `claude_supports_extended_cache()` | `claude.rs:397-407` | Determine TTL for breakpoints |
| Bedrock tool caching | `bedrock.rs:614-619` | Reference implementation for tool BP |
| `RequestPatch` system | `common.rs:267-310` | Users can override cache behavior |

---

## Summary

The aichat Claude client is **80% of the way there** on prompt caching. The existing implementation correctly:
- ✅ Omits the legacy beta header (caching is GA)
- ✅ Caches system prompts with explicit breakpoints
- ✅ Caches the last conversation message
- ✅ Enables automatic caching as a fallback
- ✅ Uses extended TTL for Claude 4+ models
- ✅ Orders input content for cache-friendliness (documents → reply → prompt)

The remaining 20% — tool caching, cache metrics, and long-conversation handling — requires approximately **80-120 lines** of straightforward code changes, concentrated in `claude.rs` and `common.rs`. The tool caching fix alone (8 lines) likely represents the single highest-value change, saving 5-20K tokens × 90% per request for tool-heavy workflows like vim-llm-assistant.
