# Anthropic Prompt Caching API Mechanics

**Source**: Official Anthropic documentation (https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching)
**Date researched**: 2025-07-16
**Status**: GA (Generally Available) — no beta header required

## Executive Summary

Anthropic's prompt caching allows reusing computed prefixes of prompts across API calls. Cache reads cost **90% less** than regular input tokens. The system caches content from the beginning of a request up to designated breakpoints, following a strict hierarchy: `tools` → `system` → `messages`.

---

## 1. Two Caching Modes

### Automatic Caching (Simplest)
Add a single `cache_control` field at the **top level** of the request body:

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "cache_control": {"type": "ephemeral"},
  "system": "You are a helpful assistant...",
  "messages": [...]
}
```

The system automatically places the cache breakpoint on the **last cacheable block**. In multi-turn conversations, the breakpoint moves forward automatically as the conversation grows.

### Explicit Cache Breakpoints (Fine-Grained)
Place `cache_control` directly on individual content blocks:

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "system": [
    {
      "type": "text",
      "text": "You are a helpful assistant with extensive knowledge...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

**Up to 4 explicit breakpoints** are allowed per request.

---

## 2. Cache Hierarchy and Prefix Structure

Cache prefixes follow this strict order:
1. **`tools`** (tool definitions)
2. **`system`** (system prompt blocks)
3. **`messages`** (conversation messages)

**Critical rule**: Changes at any level invalidate that level AND all subsequent levels.

| Change | Tools Cache | System Cache | Messages Cache |
|--------|-------------|--------------|----------------|
| Modify tool definitions | ✘ | ✘ | ✘ |
| Toggle web search/citations | ✓ | ✘ | ✘ |
| Change tool_choice | ✓ | ✓ | ✘ |
| Change thinking parameters | ✓ | ✓ | ✘ |
| Add/remove images | ✓ | ✓ | ✘ |

---

## 3. Token Minimums (Must Meet to Cache)

| Model | Minimum Cacheable Tokens |
|-------|--------------------------|
| Claude Fable 5, Mythos 5 | 512 |
| Claude Opus 4.8, Sonnet 4.6, Sonnet 4.5, Sonnet 4 | **1,024** |
| Claude Opus 4.7, Haiku 3.5 | 2,048 |
| Claude Opus 4.6, Opus 4.5, Haiku 4.5 | 4,096 |

**Important**: If the prompt falls below the minimum, no caching occurs and **no error is returned**. Check response `usage` fields to verify caching happened.

---

## 4. TTL (Time-To-Live)

### Default: 5 minutes
- Cache entry expires 5 minutes after last use
- **Refreshed on every hit** (no additional cost for refresh)
- Suitable for frequent, regular usage patterns

### Extended: 1 hour
- Costs 2x base input token price (vs 1.25x for 5-min writes)
- Syntax: `"cache_control": {"type": "ephemeral", "ttl": "1h"}`
- Best for: agentic workflows taking >5 min, infrequent-but-regular usage

### TTL Ordering Constraint
When mixing TTLs, **longer TTL must come before shorter TTL** in the prefix.

---

## 5. Pricing Structure

| Token Type | Cost Multiplier vs Base Input |
|------------|-------------------------------|
| Cache write (5-min) | 1.25x |
| Cache write (1-hour) | 2.0x |
| **Cache read/hit** | **0.10x** (90% savings) |
| Uncached input | 1.0x |

### Concrete Examples (Claude Sonnet 4.5 at $3/MTok base):
- Cache write (5-min): $3.75/MTok
- Cache write (1-hour): $6/MTok
- **Cache read**: $0.30/MTok
- Regular input: $3/MTok

**Net effect**: After the initial cache write, subsequent requests using the same prefix cost only 10% for the cached portion. Break-even is at ~2 cache reads for a 5-min write.

---

## 6. Exact API Request Structure

### Raw HTTP Request (Tool + System Caching)
```json
POST https://api.anthropic.com/v1/messages
Content-Type: application/json
x-api-key: <key>
anthropic-version: 2023-06-01

{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 4096,
  "tools": [
    {
      "name": "tool_1",
      "description": "...",
      "input_schema": {...}
    },
    {
      "name": "tool_2",
      "description": "...",
      "input_schema": {...},
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "system": [
    {
      "type": "text",
      "text": "Large system prompt content...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [
    {"role": "user", "content": "Hello"}
  ]
}
```

### Key Points:
- **No beta header needed** — prompt caching is GA as of current API
- `cache_control` goes on the **last item** in each section you want to cache
- System prompt MUST be array format (not plain string) to attach `cache_control`
- Tool definitions: put `cache_control` on the **last tool** in the array

---

## 7. Response Usage Fields

```json
{
  "usage": {
    "input_tokens": 50,
    "cache_creation_input_tokens": 5120,
    "cache_read_input_tokens": 0,
    "output_tokens": 503,
    "cache_creation": {
      "ephemeral_5m_input_tokens": 5120,
      "ephemeral_1h_input_tokens": 0
    }
  }
}
```

**Token breakdown**:
- `cache_read_input_tokens`: Tokens successfully read from cache (cheap)
- `cache_creation_input_tokens`: Tokens written to cache this request (slightly expensive)
- `input_tokens`: Tokens **after** the last cache breakpoint (not cached)
- **Total**: `cache_read + cache_creation + input_tokens`

**How to verify caching is working**: If both `cache_creation_input_tokens` and `cache_read_input_tokens` are 0, caching did NOT occur (likely below token minimum).

---

## 8. Lookback Mechanism (20-Block Window)

When checking for cache hits, the system:
1. Computes prefix hash at your breakpoint
2. If no match, walks backward **one block at a time**
3. Checks up to **20 positions** for a matching cache entry
4. Only finds entries that **prior requests explicitly wrote** (via their breakpoints)

**Implications for growing conversations**:
- If conversation grows by <20 blocks per turn, the lookback finds the prior write ✓
- If conversation grows by ≥20 blocks per turn, add a second explicit breakpoint closer to the prior write position

---

## 9. Pre-Warming the Cache

Use `max_tokens: 0` to write cache without generating output:

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 0,
  "system": [
    {
      "type": "text",
      "text": "Your large system prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [{"role": "user", "content": "warmup"}]
}
```

Returns empty `content` array, `stop_reason: "max_tokens"`, and populated `usage`. No output tokens billed.

**Limitations**: Cannot combine with streaming, extended thinking, structured outputs, or forced tool use.

---

## 10. What Can and Cannot Be Cached

### Cacheable:
- Tool definitions (`tools` array)
- System messages (content blocks in `system` array)
- Text messages (in `messages.content`, both user and assistant)
- Images & Documents (in user turns)
- Tool use and tool results

### NOT Cacheable:
- Thinking blocks (directly with `cache_control`) — but they ARE cached alongside other content in subsequent turns
- Sub-content blocks (e.g., citations) — cache the parent block instead
- Empty text blocks

---

## 11. Cache Invalidation Causes

- **Any modification** to content before or at a breakpoint changes the prefix hash
- **Character-level sensitivity**: Even whitespace changes invalidate
- **JSON key ordering**: Unstable key order in tool definitions (e.g., Go, Swift map serialization) breaks cache
- **Tool toggle changes**: Enabling/disabling web search, citations
- **Parameter changes**: tool_choice, thinking parameters, image presence

---

## 12. Critical Insight: Beta Header No Longer Required

**IMPORTANT**: The original `anthropic-beta: prompt-caching-2024-07-31` header is **NO LONGER NEEDED**. Prompt caching is now Generally Available on the standard `anthropic-version: 2023-06-01` API. 

If previous implementation attempts used the beta header or old beta-specific syntax, they should be updated to the current GA format shown above.

---

## 13. Automatic Caching in Multi-Turn Conversations

| Request | Content | Cache Behavior |
|---------|---------|----------------|
| Request 1 | System + User(1) + Asst(1) + **User(2)** ◀ cache | Everything written to cache |
| Request 2 | System + User(1) + Asst(1) + User(2) + Asst(2) + **User(3)** ◀ cache | System through User(2) read from cache; Asst(2) + User(3) written |
| Request 3 | (continues growing) | Previous content read from cache; only new content written |

The cache breakpoint **automatically moves forward** — no manual marker updates needed.

---

## 14. Combining Automatic + Explicit

You can use both together. For example, cache system prompt explicitly while automatic caching handles conversation growth:

```json
{
  "model": "claude-sonnet-4-20250514",
  "max_tokens": 1024,
  "cache_control": {"type": "ephemeral"},
  "system": [
    {
      "type": "text",
      "text": "Static system prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

The automatic breakpoint uses one of the 4 available slots.

---

## 15. Tool Definitions and Caching

Place `cache_control` on the **last tool** in the `tools` array:

```json
{
  "tools": [
    {"name": "tool_1", "description": "...", "input_schema": {...}},
    {"name": "tool_2", "description": "...", "input_schema": {...}, "cache_control": {"type": "ephemeral"}}
  ]
}
```

This caches ALL tool definitions (the entire tools prefix). Tools are at the top of the hierarchy, so cached tools remain valid even if system/messages change.

### Deferred Tools (defer_loading)
Tools loaded via `tool_search` appear as `tool_reference` blocks in conversation history — they do NOT break the prefix cache. This is ideal for large tool sets where only a few are needed per turn.

---

## Summary of Key Numbers

| Parameter | Value |
|-----------|-------|
| Max breakpoints | 4 per request |
| Default TTL | 5 minutes (refreshed on hit) |
| Extended TTL | 1 hour |
| Cost savings on hit | 90% vs base input |
| Write cost premium (5-min) | 25% above base input |
| Write cost premium (1-hour) | 100% above base input |
| Min tokens (Sonnet 4.5/4.6) | 1,024 |
| Min tokens (Haiku 4.5) | 4,096 |
| Lookback window | 20 blocks |
| Cache isolation | Per workspace (since Feb 2026) |
| Concurrent requests | Cache available only after first response begins |
