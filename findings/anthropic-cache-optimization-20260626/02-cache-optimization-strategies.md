# Cache Optimization Strategies and Best Practices

## Sources
- Anthropic Official Docs: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
- Anthropic Cookbook: https://platform.claude.com/cookbook/misc-prompt-caching
- Cache Diagnostics (Beta): https://docs.anthropic.com/en/docs/build-with-claude/cache-diagnostics
- Tool Use with Prompt Caching: https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/tool-use-with-prompt-caching

---

## 1. Two Caching Approaches

### Automatic Caching (Recommended for Most Cases)
- Add a single `cache_control: {"type": "ephemeral"}` at the **top level** of the request body
- The system automatically places the cache breakpoint on the **last cacheable block**
- Cache point moves forward automatically as conversations grow
- Each new request caches everything up to the last cacheable block
- Previous content is read from cache automatically

```json
{
  "model": "claude-sonnet-4-6",
  "max_tokens": 1024,
  "cache_control": {"type": "ephemeral"},
  "system": "...",
  "messages": [...]
}
```

### Explicit Cache Breakpoints (For Fine-Grained Control)
- Place `cache_control: {"type": "ephemeral"}` directly on individual content blocks
- Up to **4 breakpoints** per request
- Useful when caching sections that change at different frequencies
- Can be combined with automatic caching (automatic uses one of the 4 slots)

```json
{
  "system": [
    {
      "type": "text",
      "text": "You are a helpful assistant.",
      "cache_control": {"type": "ephemeral"}
    }
  ]
}
```

### When to Use Which

| Criterion | Automatic | Explicit |
|-----------|-----------|----------|
| Multi-turn conversations | ✅ Best | Manual management needed |
| Different TTLs per section | ❌ Single TTL | ✅ Per-breakpoint |
| Fine-grained control | ❌ | ✅ Up to 4 breakpoints |
| Simplicity | ✅ One-line change | More complex |
| Combined usage | ✅ | ✅ |

---

## 2. Content Ordering Hierarchy (CRITICAL)

Cache prefixes are created and validated in this strict order:

```
tools → system → messages
```

**This ordering forms a hierarchy where each level builds upon the previous ones.**

Changes at any level invalidate that level AND all subsequent levels:
- Changing tools → invalidates tools, system, AND messages caches
- Changing system → invalidates system AND messages caches
- Changing messages (earlier blocks) → invalidates only subsequent message cache

### Optimal Ordering Strategy

Place content in this order, from most stable to most dynamic:

1. **Tool definitions** (rarely change between requests)
2. **System prompt** (static instructions, role definitions)
3. **Static context** (injected files, reference documents)
4. **Conversation history** (grows but prior turns are stable)
5. **Current user message** (changes every request — should be AFTER breakpoint)

---

## 3. Cache Breakpoint Placement (THE KEY INSIGHT)

### The Golden Rule
> Place `cache_control` on the **last block whose prefix is identical across the requests you want to share a cache**.

### Critical Mistake to Avoid
If you place `cache_control` on content that changes every request (timestamps, per-request data, the current user message), you will:
- Pay for a fresh cache **write** on every request
- **Never** get a cache read/hit
- The lookback does NOT find stable content behind your breakpoint — it only finds entries that prior requests wrote at their own breakpoints

### How Lookback Works (3 Core Principles)

1. **Cache writes happen ONLY at your breakpoint** — the system does not write entries for any earlier position
2. **Cache reads look backward** — on each request, the system computes the prefix hash at your breakpoint, then walks backward checking for matching entries from prior requests
3. **Lookback window is 20 blocks** — the system checks at most 20 positions per breakpoint

### Practical Implications for Growing Conversations

In multi-turn conversations where the breakpoint is on the last message:
- Works perfectly as long as each turn adds **fewer than 20 blocks**
- If conversation grows by 20+ blocks between turns, the lookback window misses earlier cache writes
- **Solution**: Add a second explicit breakpoint closer to the last write position

---

## 4. Multi-Turn Conversation Patterns

### Pattern: Automatic Caching (Simplest)
```
Request 1: System + User(1) ◀ auto-cache → everything written to cache
Request 2: [System + User(1)] cached + Asst(1) + User(2) ◀ auto-cache → new blocks cached
Request 3: [System through User(2)] cached + Asst(2) + User(3) ◀ auto-cache → new blocks cached
```

The cache breakpoint moves forward automatically. Prior content is read from cache.

### Pattern: Explicit with System Pinning
Cache the system prompt explicitly and use automatic caching for the conversation:
```json
{
  "cache_control": {"type": "ephemeral"},
  "system": [
    {
      "type": "text",
      "text": "Your large system prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

This ensures the system prompt cache is always hit even if the message lookback window is exceeded.

### Pattern: Append-Only History
- Treat conversation history as **append-only** — never edit earlier messages
- Echo assistant content and tool results back **verbatim** (byte-for-byte identical)
- Any modification to earlier messages invalidates subsequent cache entries

---

## 5. Pre-Warming the Cache

For latency-sensitive applications, pre-warm the cache before users arrive:

```json
{
  "model": "claude-sonnet-4-6",
  "max_tokens": 0,
  "system": [
    {
      "type": "text",
      "text": "Your system prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [{"role": "user", "content": "warmup"}]
}
```

Key points:
- `max_tokens: 0` writes the cache without generating output
- No output tokens billed
- Must use **explicit** breakpoint (automatic places it on the placeholder message)
- Breakpoint goes on the **shared** content (system prompt), NOT on the placeholder user message
- Re-warm at least every 5 minutes (or use 1-hour TTL)
- Cache entry becomes available only AFTER the first response begins

---

## 6. TTL Management Strategies

### 5-Minute TTL (Default)
- Best for: Frequent requests (more than every 5 minutes)
- Cache refreshed on each hit at no additional cost
- Cost: 1.25x base input for writes, 0.1x base input for reads

### 1-Hour TTL
- Best for: Infrequent requests (less than every 5 minutes but more than every hour)
- Cost: 2x base input for writes, 0.1x base input for reads
- Useful for agentic workflows where steps take >5 minutes

### Mixing TTLs
- Longer TTL entries must appear **before** shorter TTL entries
- Example: 1-hour cache on system prompt, 5-minute on conversation

```json
{
  "system": [
    {
      "type": "text",
      "text": "Rarely changing system prompt...",
      "cache_control": {"type": "ephemeral", "ttl": "1h"}
    }
  ],
  "messages": [
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Conversation...",
          "cache_control": {"type": "ephemeral"}
        }
      ]
    }
  ]
}
```

---

## 7. Tool Definition Caching

### Basic Pattern
Place `cache_control` on the **last tool** in the `tools` array:

```json
{
  "tools": [
    {"name": "tool_a", ...},
    {"name": "tool_b", ..., "cache_control": {"type": "ephemeral"}}
  ]
}
```

### Critical Requirements
- Tool array must be in a **fixed, deterministic order** across requests
- JSON schemas must be serialized **deterministically** (sort keys!)
- Some languages (Swift, Go) randomize key order during JSON conversion — this breaks caches
- Adding/removing/reordering tools invalidates the ENTIRE cache (tools + system + messages)

### defer_loading for Dynamic Tools
- Deferred tools are NOT included in the system-prompt prefix
- Tools discovered via tool search are appended inline as `tool_reference` blocks
- The prefix cache is preserved even as new tools are dynamically added

---

## 8. Use-Case Specific Strategies

### Coding Assistants (Highly Relevant to aichat/vim-llm-assistant)
- Cache the system prompt (role definition, instructions) — changes rarely
- Cache tool definitions — changes rarely
- Cache injected file content (role files, skills) as part of system or early messages
- The user's actual code question is the dynamic part → goes AFTER the breakpoint
- With ~10K+ token role files, caching yields significant savings

### Agentic Tool Use
- Each iteration typically requires a new API call
- Cache the system prompt + tool definitions
- Use automatic caching to handle growing tool result history
- Server tool results are cached automatically (5-min TTL)

### Large Document Processing
- Embed full documents in system prompt or early user messages
- Place breakpoint after the document, before the question
- All subsequent questions about the same document benefit from cache hits

### Conversational Agents
- Automatic caching handles the growing conversation naturally
- System prompt with context is cached from turn 1
- Each turn: read prior turns from cache, write new delta

---

## 9. Cache Diagnostics (Beta)

A new beta feature (`cache-diagnosis-2026-04-07` header) for debugging cache misses:

### How to Use
1. Pass `diagnostics: {"previous_message_id": null}` on first turn
2. Pass `diagnostics: {"previous_message_id": "<prev_response_id>"}` on subsequent turns
3. Response includes `diagnostics.cache_miss_reason` identifying the first divergence point

### Cache Miss Reason Types
| Type | Meaning |
|------|---------|
| `model_changed` | Different model between requests |
| `system_changed` | System prompt differs (timestamps, request IDs) |
| `tools_changed` | Tools added/removed/reordered or non-deterministic schema serialization |
| `messages_changed` | Earlier message was edited/reordered/removed |
| `unavailable` | Comparison couldn't be produced |

---

## 10. Monitoring Cache Performance

Track these response fields in `usage`:
- `cache_creation_input_tokens` — tokens written to cache (new entry)
- `cache_read_input_tokens` — tokens read from cache (hit!)
- `input_tokens` — tokens AFTER the last cache breakpoint (not cached)

**Formula**: `total_input = cache_read + cache_creation + input_tokens`

**Signs of healthy caching**:
- High `cache_read_input_tokens` on most requests
- Low `cache_creation_input_tokens` (only on first request or after changes)
- Low `input_tokens` (only the dynamic suffix)

**Signs of broken caching**:
- Both `cache_creation_input_tokens` and `cache_read_input_tokens` are 0 → prompt below minimum token threshold
- High `cache_creation_input_tokens` on every request → breakpoint on changing content
- No `cache_read_input_tokens` ever → content changing between requests

---

## 11. Common Pitfalls and Anti-Patterns

| Pitfall | Why It Fails | Fix |
|---------|-------------|-----|
| Timestamp in system prompt | Changes every request, never gets cache hit | Move dynamic data to user message after breakpoint |
| Non-deterministic JSON key ordering | Hash changes even though semantically identical | Sort JSON keys deterministically |
| Editing earlier messages | Invalidates all subsequent cache entries | Treat history as append-only |
| Breakpoint on user message | Changes every request | Put breakpoint on last static block |
| Toggling web search/citations | Modifies system prompt, invalidates cache | Keep toggles constant within a session |
| Changing tool_choice mid-conversation | Invalidates messages cache | Keep tool_choice stable or place breakpoint before variation |
| Prompt below minimum tokens | No error returned, just no caching | Expand cached content to reach threshold |
| Parallel first requests | Cache entry only available after first response begins | Wait for first response before sending parallel requests |

---

## 12. Summary of Key Strategies for Maximum Cache Hit Rates

1. **Use automatic caching** for multi-turn conversations — it handles breakpoint management
2. **Pin static content at the front** — tools, system prompt, reference documents
3. **Put dynamic content after the last breakpoint** — user questions, timestamps, per-request data
4. **Keep content byte-for-byte identical** — no reformatting, no reordering, no added whitespace
5. **Use fixed tool ordering** with deterministic JSON serialization
6. **Never edit earlier messages** — append only
7. **Pre-warm for latency-critical paths** — `max_tokens: 0` on application startup
8. **Monitor `usage` fields** — track cache hit rates and diagnose problems
9. **Use cache diagnostics (beta)** to identify exactly what's breaking the cache
10. **Choose appropriate TTL** — 5-min for frequent access, 1-hour for infrequent
11. **Add secondary breakpoints** for long conversations (20+ block growth between turns)
12. **Meet minimum token thresholds** — 1,024 tokens for Sonnet 4.5/4.6, 4,096 for Haiku 4.5/Opus 4.5/4.6
