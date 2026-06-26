# Aichat Claude Client Implementation Analysis

## Overview

The aichat Claude client (`src/client/claude.rs`) already implements prompt caching with both
top-level "automatic" mode and explicit per-block `cache_control` markers. This analysis
documents the current implementation in detail to identify optimization opportunities.

---

## 1. Request Structure (`prepare_chat_completions`)

**File**: `src/client/claude.rs:42-67`

```rust
fn prepare_chat_completions(self_: &ClaudeClient, data: ChatCompletionsData) -> Result<RequestData> {
    // URL: {api_base}/messages
    // Headers:
    //   anthropic-version: 2023-06-01
    //   x-api-key: {key}
    //   anthropic-beta: extended-cache-ttl-2025-04-11  (conditional, Claude 4+ only)
}
```

### Headers
| Header | Value | Condition |
|--------|-------|-----------|
| `anthropic-version` | `2023-06-01` | Always |
| `x-api-key` | API key | Always |
| `anthropic-beta` | `extended-cache-ttl-2025-04-11` | Only for Claude 4+ models |

**Key insight**: No `anthropic-beta: prompt-caching-2024-07-31` header is set. This is correct
because prompt caching is now GA and doesn't require the beta header. The only beta header is
for the 1-hour extended TTL feature for newer Claude 4 models.

### Model Detection for Extended Cache (`claude_supports_extended_cache`)

**File**: `src/client/claude.rs:397-407`

Matches models containing: `4-5`, `4-6`, `opus-4`, `sonnet-4`, `haiku-4`
These get the 1-hour cache TTL instead of the default 5-minute TTL.

---

## 2. Body Construction (`claude_build_chat_completions_body`)

**File**: `src/client/claude.rs:153-336`

### 2.1 Message Processing

1. **System message extraction**: `extract_system_message()` removes the first message if
   it has `role: System` and returns its text content.
   - File: `src/client/message.rs:229-235`

2. **Messages array construction**: Iterates over remaining messages, converting each to
   Anthropic's format:
   - `Text` messages → `{"role": role, "content": text}`
   - `Array` messages → `{"role": role, "content": [{type: "text"/"image", ...}]}`
   - `ToolCalls` messages → Split into assistant (tool_use) + user (tool_result) message pair
   - Assistant messages (non-last) have think tags stripped

3. **Network images rejected**: Anthropic doesn't support URL-based images

### 2.2 Body Assembly Order

```json
{
  "model": "model_name",
  "messages": [...],
  "system": "system prompt text",      // if present
  "max_tokens": N,                      // if model specifies
  "temperature": N,                     // if specified
  "top_p": N,                           // if specified
  "stream": true,                       // if streaming
  "tools": [...]                        // if functions provided
}
```

### 2.3 Cache Implementation (Lines 306-335)

```rust
let should_cache = body.get("system").is_some()
    || body["messages"].as_array().map_or(false, |m| m.len() > 1)
    || body.get("tools").is_some();
```

**Condition**: Cache is enabled when ANY of:
- System message exists
- More than 1 message (multi-turn)
- Tools/functions are defined

**When enabled, three things happen:**

#### A. Top-level automatic caching
```rust
body["cache_control"] = json!({"type": "ephemeral"});
```
This enables Anthropic's "automatic" caching mode where the API automatically selects
optimal cache breakpoints.

#### B. System message explicit cache breakpoint
```rust
body["system"] = json!([{
    "type": "text",
    "text": system_str,
    "cache_control": {"type": "ephemeral"}
}]);
```
Converts the system message from a plain string to an array-of-blocks format with an
explicit `cache_control` marker. This ensures the system prompt is always a cache boundary.

#### C. Last message explicit cache breakpoint
```rust
// For string content:
body["messages"][last_idx]["content"] = json!([{
    "type": "text",
    "text": content_str,
    "cache_control": {"type": "ephemeral"}
}]);

// For array content:
body["messages"][last_idx]["content"][content_len - 1]["cache_control"] = 
    json!({"type": "ephemeral"});
```
Adds a cache breakpoint on the last message in the conversation. This enables the full
conversation prefix (up to and including this message) to be cached for multi-turn reuse.

---

## 3. What Is NOT Cached (Gaps)

### 3.1 Tools/Functions Have No Cache Marker

Tools are added to the body but never get `cache_control`:
```rust
body["tools"] = functions.iter().map(|v| json!({
    "name": v.name,
    "description": v.description,
    "input_schema": v.parameters,
})).collect();
```

**Impact**: For heavy tool-use scenarios (many function declarations), the tools definition
is a large static block that SHOULD be cached. In Anthropic's cache hierarchy, tools are
evaluated before system and messages, so a cache breakpoint on tools would allow caching
of the entire tools block independently.

**Note**: The Bedrock implementation (`src/client/bedrock.rs:614-619`) DOES add a cachePoint
after tools. This is inconsistent.

### 3.2 No Intermediate Conversation Breakpoints

Only the LAST message gets a cache breakpoint. For long conversations (20+ messages), if
the conversation grows by more than 20 blocks between turns, the lookback window won't
reach the previous breakpoint, causing cache misses.

### 3.3 No Cache Usage Tracking

The response extraction (`claude_extract_chat_completions`, line 338-395) only captures:
```rust
input_tokens: data["usage"]["input_tokens"].as_u64(),
output_tokens: data["usage"]["output_tokens"].as_u64(),
```

**Missing fields from Anthropic's response**:
- `cache_creation_input_tokens` — tokens written to cache this request
- `cache_read_input_tokens` — tokens read from cache this request

Without these, there's no visibility into whether caching is actually working.

### 3.4 Dual Caching Mode (Potential Conflict)

Both top-level `cache_control` (automatic mode) AND explicit per-block markers are set.
According to Anthropic docs:
- **Automatic mode** (top-level `cache_control`): API chooses breakpoints automatically
- **Explicit mode** (per-block `cache_control`): User specifies exact breakpoints

Setting BOTH is valid per Anthropic's docs — explicit markers are respected and automatic
may add additional breakpoints. However, this means the behavior is somewhat unpredictable
since automatic mode may or may not add breakpoints beyond the explicit ones.

---

## 4. Message Flow (How Messages Reach claude.rs)

### 4.1 Input Assembly (`src/config/input.rs`)

The `Input::from_files()` method assembles content in this order:
```
1. Documents (static content — cache-friendly prefix)  [line 77]
2. Last reply (semi-static — stable per session turn)  [line 89]
3. Prompt text (dynamic — changes every request)       [line 106]
```

This ordering is **intentionally designed for cache-friendliness** (comment in code confirms this).

### 4.2 Role Message Building (`src/config/role.rs:225`)

```
System message (from role prompt) →
Few-shot examples (if any) →
User content (input text + documents)
```

### 4.3 Session Message Building (`src/config/session.rs:525`)

```
Stored conversation history (all prior messages) →
New user message (current input)
```

### 4.4 Preparation Pipeline (`src/config/input.rs:237`)

```rust
pub fn prepare_completion_data(&self, model: &Model, stream: bool) -> Result<ChatCompletionsData> {
    let mut messages = self.build_messages()?;     // Role or Session builds messages
    patch_messages(&mut messages, model);           // Add system_prompt_prefix if model needs it
    model.guard_max_input_tokens(&messages)?;       // Token limit check
    let functions = self.config.read().select_functions(self.role());  // Get tool definitions
    Ok(ChatCompletionsData { messages, temperature, top_p, functions, stream })
}
```

---

## 5. Cross-Client Comparison

| Feature | Claude Direct | Bedrock | OpenAI-Compatible |
|---------|--------------|---------|-------------------|
| Top-level `cache_control` | ✅ | N/A (uses cachePoint) | ❌ |
| System cache marker | ✅ | ✅ (cachePoint after system) | ✅ |
| Tools cache marker | ❌ | ✅ (cachePoint after tools) | ❌ |
| Last message cache marker | ✅ | ✅ (cachePoint in content) | ✅ |
| Extended TTL | ✅ (1h beta header) | ✅ (1h TTL field) | ❌ |
| Cache usage tracking | ❌ | ❌ | ❌ |

**Key inconsistency**: Bedrock caches tools but Claude direct does not.

---

## 6. The `RequestPatch` System

**File**: `src/client/common.rs:267-310`

The `patch_request_data()` method allows per-model patches via config or environment variables.
This is relevant because:
- Users could manually add/override `anthropic-beta` headers
- Body patches can merge additional fields
- The comment in `prepare_chat_completions` says "user patches can override this header to
  combine multiple beta features"

---

## 7. Summary of Current State

### What's Working
1. ✅ System message gets cached (explicit breakpoint)
2. ✅ Last conversation message gets cached (explicit breakpoint)
3. ✅ Top-level automatic caching is enabled
4. ✅ Extended 1-hour TTL for Claude 4+ models
5. ✅ Input content is ordered for cache-friendliness (documents → last reply → prompt)
6. ✅ No beta header needed (uses GA caching)

### What's Missing/Suboptimal
1. ❌ Tools/functions don't get cache_control markers (can be very large)
2. ❌ No cache usage tracking (can't tell if caching is working)
3. ❌ No intermediate breakpoints for long conversations
4. ❌ Dual automatic+explicit mode may cause unpredictable behavior
5. ❌ No cache breakpoint on injected file content (which is often very large and static)

### Opportunities
1. Add `cache_control` to the last tool in the tools array
2. Parse and log `cache_creation_input_tokens` and `cache_read_input_tokens`
3. Consider removing top-level `cache_control` in favor of purely explicit markers
4. Add intermediate breakpoints every N messages for long conversations
5. Consider cache breakpoint placement on large static content blocks within messages

---

## 8. Relevant File Locations

| File | Lines | Purpose |
|------|-------|---------|
| `src/client/claude.rs:42-67` | Request preparation, headers |
| `src/client/claude.rs:153-336` | Body construction + caching |
| `src/client/claude.rs:338-395` | Response extraction (missing cache metrics) |
| `src/client/claude.rs:397-407` | Extended cache model detection |
| `src/client/common.rs:267-310` | Request patching system |
| `src/client/common.rs:330-340` | ChatCompletionsOutput struct (missing cache fields) |
| `src/client/bedrock.rs:585-622` | Bedrock caching (reference for tools caching) |
| `src/client/openai.rs:346-386` | OpenAI-compatible caching |
| `src/config/input.rs:67-107` | Input ordering (cache-friendly) |
| `src/config/input.rs:237-252` | Data preparation pipeline |
| `src/config/role.rs:225-258` | Role message building |
| `src/config/session.rs:525-557` | Session message building |
| `src/client/message.rs:229-235` | System message extraction |
