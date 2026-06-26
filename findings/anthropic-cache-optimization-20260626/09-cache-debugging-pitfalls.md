# Cache Debugging and Common Pitfalls

**Task**: Research what can go wrong with prompt caching — cache misses, debugging strategies, and monitoring
**Date**: 2025-07-16
**Sources**: 
- Anthropic Prompt Caching Docs: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
- Cache Diagnostics (Beta): https://docs.anthropic.com/en/docs/build-with-claude/cache-diagnostics
- Tool Use with Prompt Caching: https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/tool-use-with-prompt-caching
- aichat source code: /home/pdibiase/sources/aichat/src/client/claude.rs

---

## Executive Summary

Prompt caching failures are **silent by default** — no error is returned when caching fails. The only signals are in the `usage` response fields. Understanding why cache misses occur requires both knowledge of the API mechanics and careful instrumentation. This document covers the complete taxonomy of cache miss causes, debugging strategies, and specific issues relevant to the aichat/vim-llm-assistant stack.

---

## 1. Taxonomy of Cache Miss Causes

### 1.1 Prefix Hash Sensitivity (Character-Level Exactness)

**The fundamental rule**: Cache hits require the prompt prefix to be **byte-for-byte identical** to a prior request's cached prefix. This is not "semantically similar" — it's cryptographic hash matching.

| Change Type | Cache Impact | Example |
|-------------|-------------|---------|
| Single character difference | Full miss | Adding/removing a space |
| Whitespace variation | Full miss | `\n` vs `\r\n`, trailing spaces |
| JSON key reordering | Full miss | `{"a":1,"b":2}` vs `{"b":2,"a":1}` |
| Unicode normalization | Full miss | Different UTF-8 encodings of same character |
| Number formatting | Full miss | `1.0` vs `1` in JSON |
| Null handling | Full miss | `{"key": null}` vs omitting key |

**Critical insight for aichat**: Rust's `serde_json` with `json!()` macro preserves insertion order, which is deterministic. However, the `input_schema` field in tool definitions comes from parsed JSON files — if those files are ever regenerated or reordered, cache invalidation occurs.

### 1.2 Model Switching

Caches are **per-model**. Switching between:
- `claude-sonnet-4-20250514` and `claude-sonnet-4.5-20241022`
- Any model alias that resolves differently over time
- A/B testing or load balancer routing to different model versions

**All produce cache misses.** There is no cross-model cache sharing.

**aichat relevance**: If the user switches between model configurations (e.g., `claude-sonnet-4` vs `claude-sonnet-4.5` in config), all accumulated caches are lost. Even minor version bumps (if Anthropic updates what an alias resolves to) can silently invalidate caches.

### 1.3 Cache Hierarchy Cascade

Changes cascade downward through the `tools → system → messages` hierarchy:

```
tools change    → tools ✘, system ✘, messages ✘  (everything lost)
system change   → tools ✓, system ✘, messages ✘  (tools survive)
messages change → tools ✓, system ✓, messages ✘  (tools+system survive)
```

**What counts as a "tools change"**:
- Adding/removing a tool from the array
- Reordering tools in the array
- Changing any tool's name, description, or input_schema
- Non-deterministic JSON serialization of input_schema

**What counts as a "system change"**:
- Any text modification to system prompt
- Toggling web search or citations features
- Toggling `speed: "fast"` mode

**What counts as a "messages change"**:
- Editing an earlier message (not just appending)
- Removing or reordering messages
- Re-serializing tool_result blocks differently
- Changing `tool_choice` parameter
- Adding/removing images
- Changing thinking parameters (enable/disable, budget)

### 1.4 Token Minimum Not Met

| Model | Minimum Tokens | Behavior When Below |
|-------|---------------|---------------------|
| Claude Sonnet 4 / 4.5 / 4.6, Opus 4.8, Opus 4.1 | 1,024 | Silent no-op |
| Claude Opus 4.7, Haiku 3.5 | 2,048 | Silent no-op |
| Claude Opus 4.5, 4.6, Haiku 4.5 | 4,096 | Silent no-op |
| Claude Fable 5, Mythos 5 | 512 | Silent no-op |

**No error is returned** when below the minimum. The request processes normally, but no cache entry is created or read. The **only** indication is that both `cache_creation_input_tokens` and `cache_read_input_tokens` are 0 in the response.

**aichat relevance**: The default-vim-role.md is ~39KB (~10K tokens), and tools are ~8K tokens. These easily exceed minimums. But for short system prompts or single-tool configurations, caching may silently fail to activate.

### 1.5 TTL Expiration

- **Default**: 5 minutes from last access (refreshed on each hit)
- **Extended**: 1 hour (requires `extended-cache-ttl-2025-04-11` beta header)

If a user takes more than 5 minutes between requests (e.g., reading code, thinking), the cache expires silently. The next request pays for a full cache write.

**aichat relevance**: Development workflows often have gaps > 5 minutes. The extended-cache-ttl header is already sent for Claude 4+ models (`claude.rs:63`), but the actual `cache_control` markers still use `{"type": "ephemeral"}` without `"ttl": "1h"`, which means 5-minute default TTL.

### 1.6 Concurrent Request Race Condition

A cache entry is only available **after the first response begins**. If multiple parallel requests are sent simultaneously:
- First request creates cache entry → available once response starts streaming
- Parallel requests sent before first response starts → all miss cache → all create separate entries

**aichat relevance**: Ralph loops with parallel workers can hit this. The first worker's cache entry won't be available for others unless there's a brief delay.

### 1.7 Workspace Isolation (Since Feb 2026)

Caches are isolated per workspace within an organization. Different API keys from different workspaces produce separate caches even with identical prompts.

### 1.8 Lookback Window Exhaustion (20-Block Limit)

The system checks at most **20 positions backward** from each cache breakpoint. In conversations that grow by 20+ blocks per turn (common in tool-heavy agentic workflows), the lookback window can't reach the previous cache write.

**Example failure scenario**:
- Turn 1: 10 blocks, cache written at block 10
- Turn 2: Agent does 12 tool calls → adds ~24 blocks → total 34 blocks, breakpoint at 34
- Lookback checks blocks 34→15 (20 positions) — block 10's cache entry is OUTSIDE the window
- Result: Full cache miss despite all content being identical up to block 10

**Fix**: Add a secondary explicit breakpoint closer to where prior writes occurred.

---

## 2. Issues Specific to aichat/vim-llm-assistant Stack

### 2.1 Dynamic Content in System Prompt

The aichat system prompt comes from role files. If any part of the role system injects dynamic content (timestamps, session IDs, environment variables that change), the system cache is invalidated every request.

**Check**: Grep role files for any `date`, `time`, `hostname`, or `$VARIABLE` interpolation that might produce different text each invocation.

### 2.2 Tool Definition Instability

aichat loads tools from JSON function declarations (`config/mod.rs:1654-1700`). Potential instability:
- If the functions JSON file is regenerated with different formatting
- If tools are loaded from multiple sources that may have inconsistent JSON key ordering
- If the tool selection set changes between requests (e.g., role changes tool list)

**Current code** (`claude.rs:289-303`) iterates tools in declaration order with `json!()` macro — this is deterministic within a single process invocation. However:
- Different aichat invocations (separate CLI calls from vim) rebuild the tool list fresh each time
- If the function declarations file is modified between calls, tools change

### 2.3 The "Breakpoint on Last Message" Problem

Current aichat code (`claude.rs:321-336`) places a `cache_control` breakpoint on the **last message** in the conversation. This is the message that changes every request (the user's new input). 

**Why this still works with automatic caching**: The top-level `cache_control` at line 311 enables automatic caching mode, which uses the 20-block lookback to find the prior turn's cache write. For growing conversations with <20 blocks of growth per turn, the lookback finds the prior entry.

**When this breaks**: 
- If the conversation grows by 20+ blocks between turns (heavy tool use)
- The explicit breakpoint on the last message is technically a wasted write (changes every time)

### 2.4 vim-llm-assistant's Content Assembly

The vim plugin assembles context as a single large JSON blob. If any part of this blob is non-deterministic between calls:
- `cursor_line` / `cursor_col` (changes constantly as user moves cursor)
- Buffer contents (change as user edits)
- File modification timestamps

Since these are embedded in the **messages** content (after the system prompt), they only invalidate the messages cache — tools and system remain cached. But this means the most expensive part (the full context) gets cache-missed frequently.

### 2.5 Missing cache_control on Tools

aichat currently does NOT add `cache_control` to the last tool definition. This means:
- Tool definitions (~8K tokens) are included in the automatic caching (via top-level `cache_control`)
- But they don't have their own independent cache breakpoint
- If tools change, there's no way to preserve a partial cache

This is documented in finding-07 as the #1 implementation opportunity.

---

## 3. Monitoring Cache Hit Rates

### 3.1 Response Usage Fields

Every API response includes these fields in the `usage` object:

```json
{
  "usage": {
    "input_tokens": 50,              // Tokens AFTER last cache breakpoint (not cached)
    "cache_creation_input_tokens": 0, // Tokens written to cache this request
    "cache_read_input_tokens": 15000, // Tokens read from cache (HIT)
    "output_tokens": 503,
    "cache_creation": {               // Breakdown by TTL (if multiple)
      "ephemeral_5m_input_tokens": 0,
      "ephemeral_1h_input_tokens": 0
    }
  }
}
```

**Key formula**: `total_input = cache_read + cache_creation + input_tokens`

### 3.2 Interpreting the Numbers

| cache_creation | cache_read | input_tokens | Interpretation |
|---------------|------------|--------------|----------------|
| 0 | 0 | high | ⚠️ Caching NOT active — below minimum or no cache_control |
| high | 0 | low | First request or cache expired — writing new entry |
| 0 | high | low | ✅ Cache HIT — ideal state |
| high | 0 | high | 🐛 Breakpoint on changing content — writing every time |
| low | high | low | ✅ Partial hit — some new content cached, most read |
| high | high | low | Rare — multiple breakpoints, some hit some miss |

### 3.3 Health Metrics

Calculate these across requests:

```
hit_rate = cache_read_input_tokens / (cache_read + cache_creation + input_tokens)
cache_savings = cache_read_input_tokens * 0.9  # 90% savings on these tokens
write_overhead = cache_creation_input_tokens * 0.25  # 25% extra cost on writes
net_savings = cache_savings - write_overhead
```

**Target**: hit_rate > 80% after the first few requests of a session.

### 3.4 Current aichat Limitation

**aichat currently ignores these fields** (`claude.rs:386-393`). The `extra` field on `ChatCompletionsOutput` is set to `None`, discarding cache metrics. There is existing infrastructure for displaying cache info (`common.rs:480-493` — the Bifrost cache display), but no Anthropic prompt cache equivalent.

**What's needed**: Extract `cache_creation_input_tokens` and `cache_read_input_tokens` from the response and display them when `show_gateway_info` is enabled.

---

## 4. Cache Diagnostics (Beta)

### 4.1 Overview

Anthropic provides a beta feature (`cache-diagnosis-2026-04-07` header) that compares consecutive requests and reports exactly where the prompt prefix diverged.

### 4.2 How to Use

```json
// First request: opt in
{
  "diagnostics": {"previous_message_id": null},
  // ... normal request ...
}

// Subsequent requests: reference prior response
{
  "diagnostics": {"previous_message_id": "msg_01Xyz..."},
  // ... normal request ...
}
```

### 4.3 Response Format

```json
{
  "diagnostics": {
    "cache_miss_reason": {
      "type": "tools_changed",
      "cache_missed_input_tokens": 41850
    }
  }
}
```

### 4.4 Miss Reason Types

| Type | Meaning | Fix |
|------|---------|-----|
| `model_changed` | Different model between requests | Hold model constant in conversation |
| `system_changed` | System prompt differs (timestamps, request IDs) | Move dynamic data to user message after breakpoint |
| `tools_changed` | Tools added/removed/reordered, or non-deterministic schema | Fixed tool order, deterministic JSON serialization |
| `messages_changed` | Earlier message edited/reordered/removed | Treat history as append-only, echo content verbatim |
| `previous_message_not_found` | Fingerprint expired or missing | Send beta header on every turn, keep turns close |
| `unavailable` | Other parameter change or very long conversation | Check tool_choice, thinking, context_management params |

### 4.5 Combining Diagnostics with Usage

| Diagnostics | Cache Read Tokens | Interpretation |
|-------------|-------------------|----------------|
| null (no divergence) | high | ✅ Working perfectly |
| null (no divergence) | 0 | Cache expired (TTL) — shorten gaps or use 1h TTL |
| `*_changed` type | 0 | 🐛 Your bug — fix cause indicated by type |
| `*_changed` type | high | Low-impact — change is late in prompt, earlier breakpoint still hits |

### 4.6 Limitations

- Beta only — field names may change
- Claude API only (not Bedrock/GCloud)
- Fingerprints expire after short period
- Same workspace required
- Very long conversations may get `unavailable` instead of precise location
- Cannot be combined with `max_tokens: 0` pre-warming

### 4.7 Implementation for aichat

To add cache diagnostics support, aichat would need:
1. Store the response `id` from each request
2. Pass it as `diagnostics.previous_message_id` on the next request
3. Add the `cache-diagnosis-2026-04-07` beta header
4. Parse and display `diagnostics.cache_miss_reason` from responses

This requires state between requests (conversation session tracking), which aichat partially has via its session system.

---

## 5. Debugging Strategies

### 5.1 Quick Diagnosis Checklist

When cache reads are unexpectedly zero:

1. **Check token minimums**: Is total prefix ≥ 1024 tokens (for Sonnet)?
2. **Check TTL**: Was last request > 5 minutes ago?
3. **Check model consistency**: Same model string both requests?
4. **Check system prompt**: Any dynamic content (timestamps, env vars)?
5. **Check tools**: Same tool set, same order, same schemas?
6. **Check message history**: Append-only? Any edits/removals?
7. **Check parameters**: Same `tool_choice`, `thinking` settings, image presence?
8. **Check breakpoint placement**: Is `cache_control` on content that changes?

### 5.2 Instrumentation Strategy for aichat

**Level 1: Basic Metrics (Minimum Viable)**
```rust
// In claude_extract_chat_completions:
let cache_read = data["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
let cache_write = data["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);
let input = data["usage"]["input_tokens"].as_u64().unwrap_or(0);
let total = cache_read + cache_write + input;
if total > 0 {
    let hit_pct = cache_read * 100 / total;
    eprintln!("📦 Cache: {cache_read} read, {cache_write} written, {input} uncached ({hit_pct}% hit)");
}
```

**Level 2: Request Logging (For Investigation)**
```rust
// When debugging, log the request body hash to detect changes:
let body_str = serde_json::to_string(&body)?;
let hash = sha256::digest(&body_str);
eprintln!("🔍 Request hash: {}", &hash[..16]);
```

**Level 3: Differential Debugging**
Compare two consecutive requests by logging:
- System prompt length and first/last 50 chars
- Tool count and names
- Message count and roles
- Any field that could differ

### 5.3 Common Debugging Scenarios

#### Scenario: "cache_creation on every request, never cache_read"

**Diagnosis**: Something in the cached prefix changes every request.

**Steps**:
1. Log the system prompt — look for timestamps, process IDs, session tokens
2. Log tool definitions — look for reordering or schema changes
3. Log first message — look for cursor position, buffer content changes
4. If using automatic caching (top-level `cache_control`), the breakpoint is on the last block — which changes every turn! This is actually expected behavior; the lookback should find the prior write.
5. If lookback also fails: conversation grew 20+ blocks since last request.

#### Scenario: "cache works for 2-3 requests then stops"

**Diagnosis**: Likely TTL expiration or tool/system change.

**Steps**:
1. Check time gap between requests — if >5 min, TTL expired
2. Check if role or tool configuration changed mid-session
3. Check if model alias resolved to a different version (Anthropic model updates)

#### Scenario: "cache works in testing but not in production"

**Diagnosis**: Environment-specific differences.

**Steps**:
1. Different workspaces → separate cache pools
2. Different API keys from different workspaces
3. Load balancer routing to different model deployments
4. Environment variables injected differently in production

#### Scenario: "partial cache hit — cache_read is lower than expected"

**Diagnosis**: Breakpoint placement or hierarchy.

**Steps**:
1. Tools cache might be hitting while system/messages miss
2. System might be hitting while messages miss
3. Check what specific layer changed using cache diagnostics

---

## 6. Language/Framework-Specific Pitfalls

### 6.1 JSON Serialization (THE #1 Cause of Silent Misses)

**Languages with non-deterministic JSON key ordering**:
- **Go**: `map[string]interface{}` iterates in random order → tools change every request
- **Swift**: `Dictionary` is unordered → same problem
- **Python**: `dict` is insertion-ordered since 3.7, but some JSON libraries don't preserve it
- **JavaScript**: Object keys are generally insertion-ordered, but `JSON.stringify` may vary

**Rust (serde_json)**: Deterministic — struct fields serialize in definition order, `json!()` macro preserves insertion order. **aichat is safe here.**

**The trap**: Even if YOUR code serializes deterministically, if `input_schema` comes from a file that was generated by a non-deterministic tool, the schema string changes between generations.

### 6.2 Floating Point Precision

```json
// Request 1: tool schema from one source
{"temperature": {"type": "number", "minimum": 0.0}}

// Request 2: same schema from different source
{"temperature": {"type": "number", "minimum": 0}}
```

These are semantically identical but produce different JSON bytes → cache miss.

### 6.3 Unicode Normalization

```
"café" (NFC: é as single codepoint U+00E9)
"café" (NFD: e + combining acute U+0065 U+0301)
```

Visually identical, different bytes → cache miss.

### 6.4 Line Ending Differences

```
"Hello\nWorld"   (Unix LF)
"Hello\r\nWorld" (Windows CRLF)
```

Different bytes → cache miss. This is particularly relevant for file content injected from different OS environments.

---

## 7. Anti-Patterns and Their Fixes

### 7.1 Timestamp in System Prompt

**Anti-pattern**:
```json
{"system": "You are a helpful assistant. Current time: 2025-07-16T14:30:00Z"}
```

**Fix**: Move timestamps to the user message (after the cache breakpoint):
```json
{"system": [..., "cache_control": ...],
 "messages": [{"role": "user", "content": "Current time: 2025-07-16T14:30:00Z\n\nWhat is..."}]}
```

### 7.2 Request IDs or Session IDs in Prompt

**Anti-pattern**:
```json
{"system": "Session: abc123. You are a helpful assistant."}
```

**Fix**: Remove from cached content entirely, or pass via a non-cached mechanism.

### 7.3 Non-Deterministic Tool Schema Generation

**Anti-pattern**: Generating tool schemas from runtime type reflection that may produce different key ordering.

**Fix**: 
- Load schemas from static JSON files
- Sort JSON keys at serialization time
- Validate schema stability with hash comparison

### 7.4 Editing Earlier Messages

**Anti-pattern**: Summarizing or modifying earlier conversation turns to save context.

**Fix**: 
- Treat history as append-only
- If summarization is needed, add it as a new message rather than replacing old ones
- Use context compaction features that maintain cache-friendly structure

### 7.5 Placing Breakpoint on the Varying Suffix

**Anti-pattern**: 
```json
{"system": ["...", "cache_control": ...],
 "messages": [..., {"role": "user", "content": "UNIQUE_QUESTION", "cache_control": ...}]}
```

The breakpoint is on content that changes every request — you pay for a cache write but never get a read.

**Fix**: Place the breakpoint on the last STABLE block. Rely on automatic caching or lookback to pick up the prior entry:
```json
{"system": [{"text": "...", "cache_control": ...}],
 "messages": [...stable history..., {"role": "user", "content": "new question"}]}
```

### 7.6 Toggling Features Mid-Conversation

**Anti-pattern**: Enabling web search on request 3 when requests 1-2 didn't have it.

**Fix**: Keep feature toggles constant for the lifetime of a conversation session. If a feature needs to be toggled, accept the one-time cache miss.

---

## 8. Monitoring and Alerting Recommendations

### 8.1 Metrics to Track

| Metric | Formula | Healthy Threshold |
|--------|---------|-------------------|
| Hit Rate | `cache_read / (cache_read + cache_creation + input)` | > 80% (after warmup) |
| Miss Rate | `1 - hit_rate` | < 20% |
| Write Frequency | count of requests where `cache_creation > 0` | Low after first few |
| Average Write Size | `avg(cache_creation_input_tokens)` | Stable (±5%) |
| Cost Savings | `cache_read * 0.9 * price_per_token` | Positive |
| TTL Expiration Rate | requests where both cache_read and cache_creation are 0 | < 10% |

### 8.2 Alerting Conditions

- **Sudden drop in hit rate**: Indicates code change broke cache stability
- **Persistent zero cache_read**: Indicates caching not working at all
- **Increasing cache_creation without cache_read**: Breakpoint on changing content
- **Both fields zero**: Below token minimum or no cache_control in request

### 8.3 Logging for Debugging

When investigating cache issues, log (at debug level):
1. Model name used
2. System prompt hash (first 8 chars of SHA-256)
3. Tool count and sorted tool name list
4. Message count and role sequence
5. Breakpoint positions (which indices have cache_control)
6. Response usage fields (cache_read, cache_creation, input_tokens)

---

## 9. aichat-Specific Debugging Flow

### 9.1 Current State Assessment

**What works now**:
- System prompt gets `cache_control` breakpoint ✅ (claude.rs:313-318)
- Last message gets `cache_control` breakpoint ✅ (claude.rs:321-336)
- Top-level `cache_control` enables automatic caching ✅ (claude.rs:311)
- Extended TTL header sent for Claude 4+ ✅ (claude.rs:63)
- Rust's serde_json is deterministic ✅

**What's missing**:
- Tool definitions have no `cache_control` ❌ (tools cache independently in hierarchy)
- Response cache metrics are discarded ❌ (claude.rs:386-393, `extra: None`)
- No way to verify caching is working ❌ (no logging/display)
- No cache diagnostics integration ❌
- Extended TTL not used on actual breakpoints ❌ (only header is set)
- No intermediate breakpoints for long conversations ❌

### 9.2 Debugging Without Code Changes

Even without modifying aichat, you can debug caching issues:

1. **Enable debug logging** (`AICHAT_LOG_LEVEL=debug`):
   - Shows full request and response JSON
   - Can grep for `cache_read_input_tokens` in logs

2. **Compare request bodies manually**:
   - Export two consecutive request bodies
   - Diff them to find what changed

3. **Test with curl**:
   - Send the same request twice manually
   - Check if second request gets cache_read > 0

### 9.3 Quick Validation Test

```bash
# Send same request twice, check for cache hit on second:
BODY='{"model":"claude-sonnet-4-20250514","max_tokens":100,"cache_control":{"type":"ephemeral"},"system":"You are a test assistant with a long system prompt. [repeat text to reach 1024+ tokens]","messages":[{"role":"user","content":"Hello"}]}'

# First request (cache write):
curl -s https://api.anthropic.com/v1/messages \
  -H "anthropic-version: 2023-06-01" \
  -H "x-api-key: $ANTHROPIC_API_KEY" \
  -H "content-type: application/json" \
  -d "$BODY" | jq '.usage'

# Second request (should be cache hit):
curl -s https://api.anthropic.com/v1/messages \
  -H "anthropic-version: 2023-06-01" \
  -H "x-api-key: $ANTHROPIC_API_KEY" \
  -H "content-type: application/json" \
  -d "$BODY" | jq '.usage'
```

Expected: First shows `cache_creation_input_tokens > 0`. Second shows `cache_read_input_tokens > 0`.

---

## 10. Comprehensive Debugging Checklist

### Before Deployment

- [ ] System prompt is static (no timestamps, session IDs, env vars)
- [ ] Tool definitions are loaded from stable source (deterministic JSON)
- [ ] Tool ordering is consistent between process invocations
- [ ] Model name is hardcoded (not a dynamic alias)
- [ ] Total cached prefix exceeds minimum token threshold for target model
- [ ] `cache_control` breakpoint is on the last STABLE content, not the dynamic suffix
- [ ] Feature toggles (web search, citations, thinking) are constant per session
- [ ] Message history is append-only (no edits, no removals)
- [ ] Cache metrics are being logged/displayed for verification

### When Cache Misses Occur

- [ ] Check `usage` fields — is caching active at all?
- [ ] Check time gap — is it < 5 min (or < 1h with extended TTL)?
- [ ] Check model — same string both requests?
- [ ] Check system prompt — byte-for-byte identical?
- [ ] Check tools — same set, same order?
- [ ] Check messages — append-only growth?
- [ ] Enable cache diagnostics (beta) for automatic detection
- [ ] Compare request body hashes between consecutive requests
- [ ] Check for lookback window exhaustion (20+ block growth)

### Performance Optimization

- [ ] Tools have their own `cache_control` breakpoint (highest stability level)
- [ ] System prompt has `cache_control` breakpoint
- [ ] Automatic caching handles conversation growth
- [ ] Consider 1-hour TTL for system prompt (especially for dev workflows with gaps)
- [ ] Consider pre-warming for latency-critical first requests
- [ ] Add intermediate breakpoints for long conversations (20+ messages)
- [ ] Monitor hit rate and set up alerting for drops

---

## 11. The Lookback Mechanism in Detail

### How It Actually Works

When a request arrives with cache_control breakpoints:

1. **For each breakpoint** (up to 4), the system computes a prefix hash covering all content from the start through that block.
2. **Hash comparison**: If the hash matches a stored entry, cache HIT → read tokens cheaply.
3. **If no match**: Walk backward one block at a time, re-computing hashes.
4. **Window limit**: Walk back at most 20 positions from the breakpoint.
5. **Only finds prior writes**: The lookback finds entries that PRIOR REQUESTS wrote at THEIR breakpoints. It does NOT discover "stable content" — it discovers previously-written cache entries.

### Visual Example

```
Request 1:  [A][B][C][D][E*]     → cache written at position E (hash of A+B+C+D+E)
Request 2:  [A][B][C][D][E][F][G*]  → hash at G doesn't match; walk back...
            G→F→E → found match at E! Cache hit for A+B+C+D+E; process F+G fresh.
Request 3:  [A][B]...[Z][AA][BB*]   → 20+ positions past E
            BB→AA→...→(can check 20 positions)... if E is >20 away, MISS!
```

### Implications for aichat

The vim-llm-assistant workflow typically adds 2-4 blocks per turn (user message + assistant response, maybe tool calls). With 20-block lookback, this means the system can handle up to 10 turns of conversation without intermediate breakpoints. After that, consider adding a mid-conversation breakpoint.

For **agentic tool use** (ralph loops), each tool call cycle adds 2 blocks (assistant tool_use + user tool_result). A single iteration with 12 tool calls adds 24 blocks — exceeding the lookback window!

---

## 12. Summary of Key Takeaways

1. **Caching failures are silent** — you MUST instrument response usage fields to know if it's working
2. **Character-level sensitivity** — the prefix hash is byte-exact; any difference is a full miss
3. **Cache hierarchy** — tools → system → messages; changes cascade DOWN
4. **Model switching kills cache** — different model = different cache space
5. **TTL is short** (5 min default) — development workflows need 1-hour TTL
6. **Lookback window is limited** (20 blocks) — long conversations need intermediate breakpoints
7. **Cache diagnostics (beta)** — new feature that tells you exactly what changed; requires header and previous response ID tracking
8. **aichat's biggest gap** — no visibility into whether caching works (metrics discarded)
9. **Tool caching is independent** — even when system/messages miss, tools can still hit
10. **Test early, test often** — validate caching works in your specific flow before assuming savings
