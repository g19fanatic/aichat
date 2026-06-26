# 11: Responses API vs Chat Completions API — Tool Schema Handling Differences

## Summary

The OpenAI Responses API and Chat Completions API differ fundamentally in how they handle tool schemas, particularly around strict mode. The Responses API **auto-normalizes all schemas to strict mode by default** when the `strict` field is omitted, while Chat Completions **remains non-strict (best-effort) by default**. This difference is the root cause of the aichat GPT-5.5 bug: aichat never sets `strict` on tool schemas, which works fine for Chat Completions but triggers strict auto-normalization in the Responses API.

## Source Documentation

- OpenAI Function Calling Guide: https://platform.openai.com/docs/guides/function-calling
- OpenAI Responses API Reference: https://platform.openai.com/docs/api-reference/responses/create
- aichat source: `src/client/openai.rs` (Chat Completions) and `src/client/bedrock.rs` (Responses API)

---

## Key Difference: Default Strict Mode Behavior

| Aspect | Responses API (`/v1/responses`) | Chat Completions API (`/v1/chat/completions`) |
|--------|-------------------------------|----------------------------------------------|
| **Default when `strict` omitted** | Auto-normalizes to strict mode | Non-strict (best-effort) |
| **Auto-normalization** | Sets `additionalProperties: false`, marks ALL fields as required | No normalization performed |
| **Schema rejection** | Only if `strict: true` explicitly set AND schema invalid | Only if `strict: true` explicitly set AND schema invalid |
| **Opt-out mechanism** | Must explicitly set `strict: false` | Already non-strict by default |
| **Impact of invalid schemas** | Model constrained by auto-normalized schema → may produce 0 tokens | Model uses best-effort matching → produces output anyway |

### Direct Quote from OpenAI Docs (Function Calling Guide, Strict Mode section):

> "If you omit `strict`, the default depends on the API: **Responses requests will normalize your schema into strict mode** (for example, by setting `additionalProperties: false` and marking all fields as required), which can **make previously optional fields mandatory**, while Chat Completions requests remain non-strict by default. To opt out of strict mode in Responses and keep non-strict, best-effort function calling, explicitly set `strict: false`."

---

## Tool Schema Format Differences

### Chat Completions API (openai.rs)

**Request format** — tools are nested under a `"function"` key:
```json
{
  "type": "function",
  "function": {
    "name": "get_weather",
    "description": "...",
    "parameters": {...},
    "strict": true  // optional, defaults to non-strict
  }
}
```

**aichat implementation** (`src/client/openai.rs:334-338`):
```rust
body["tools"] = functions
    .iter()
    .map(|v| {
        json!({"type": "function", "function": v})
    })
    .collect();
```

### Responses API (bedrock.rs)

**Request format** — properties are at the top level (flat structure):
```json
{
  "type": "function",
  "name": "get_weather",
  "description": "...",
  "parameters": {...},
  "strict": true  // defaults to true via auto-normalization!
}
```

**aichat implementation** (`src/client/bedrock.rs:913-924`):
```rust
body["tools"] = functions
    .iter()
    .map(|v| {
        json!({
            "type": "function",
            "name": v.name,
            "description": v.description,
            "parameters": v.parameters,
        })
    })
    .collect();
```

**Critical**: No `"strict"` field is set in either path. For Chat Completions this is fine (defaults non-strict). For Responses API, this triggers auto-normalization to strict mode.

---

## Response Format Differences

### Chat Completions Response
```json
{
  "choices": [{
    "message": {
      "content": "text here",
      "tool_calls": [{"id": "...", "function": {"name": "...", "arguments": "..."}}]
    }
  }],
  "usage": {"prompt_tokens": N, "completion_tokens": N}
}
```

### Responses API Response
```json
{
  "id": "resp_...",
  "status": "completed",
  "output": [
    {"type": "message", "content": [{"type": "output_text", "text": "..."}]},
    {"type": "function_call", "name": "...", "arguments": "...", "call_id": "..."}
  ],
  "usage": {"input_tokens": N, "output_tokens": N}
}
```

---

## Empty Output Handling Differences in aichat

### Chat Completions (openai.rs:427-440) — GRACEFUL
```rust
if text.is_empty() && tool_calls.is_empty() {
    warn!("Received empty content and no tool calls from model...");
    return Ok(ChatCompletionsOutput {
        text: String::new(),
        tool_calls: vec![],
        id: data["id"].as_str().map(|v| v.to_string()),
        input_tokens: data["usage"]["prompt_tokens"].as_u64(),
        output_tokens: data["usage"]["completion_tokens"].as_u64(),
    });
}
```

### Responses API (bedrock.rs:981-982) — CRASHES
```rust
if text.is_empty() && tool_calls.is_empty() {
    bail!("Invalid Responses API response data: {data}");
}
```

---

## How This Causes the Bug

The causal chain:

1. **aichat sends tool schemas** to Bedrock's `/openai/v1/responses` endpoint **without** the `strict` field
2. **Responses API auto-normalizes** all schemas to strict mode:
   - Sets `additionalProperties: false` on all objects
   - Marks ALL properties as `required` (even optional ones)
   - Does NOT convert optional params to nullable type (`["type", "null"]`)
3. **GPT-5.5 receives strict-mode schemas** where:
   - 12/22 tools have properties NOT originally in their `required` array
   - Auto-normalization makes those fields "required" but without nullable types
   - Model cannot produce valid output (it literally cannot generate a valid tool call that includes values for all "required" fields when it doesn't know what to put)
4. **GPT-5.5 returns** `"status": "completed"` with `"output": []` and 0 output tokens
5. **aichat's Responses API parser** (bedrock.rs:981) encounters empty output, finds no text and no tool_calls, and crashes with `bail!("Invalid Responses API response data: ...")`
6. **Error propagates** as `"Failed to call chat-completions api"` → `"Invalid Responses API response data: {json}"`

---

## Why "1 Simple Tool Call Works"

The user reports that "1 simple tool call works just fine." This is consistent because:
- A single tool with only `required` properties (no optional ones) **already complies** with strict mode
- Fewer tools = less schema complexity for the model to parse
- The issue only manifests when tools have properties NOT in `required` (i.e., optional params that auto-normalization makes mandatory without nullable types)

---

## The Fix: Two Options

### Option A: Set `strict: false` in Responses API path (Recommended)
```rust
// In bedrock.rs build_responses_api_body()
body["tools"] = functions
    .iter()
    .map(|v| {
        json!({
            "type": "function",
            "name": v.name,
            "description": v.description,
            "parameters": v.parameters,
            "strict": false,  // Opt out of auto-normalization
        })
    })
    .collect();
```

This preserves the non-strict (best-effort) behavior that Chat Completions uses, avoiding schema incompatibilities.

### Option B: Make schemas compliant with strict mode
- Add `additionalProperties: false` to all parameter objects
- Put ALL properties in `required` array
- Use `"type": ["string", "null"]` for optional parameters

This requires changes to the `JsonSchema` struct in `src/function.rs`.

### Option C: Fix the error handling (defense-in-depth)
```rust
// In bedrock.rs responses_api_chat_completions()
if text.is_empty() && tool_calls.is_empty() {
    warn!("Responses API returned empty output: {data}");
    return Ok(ChatCompletionsOutput {
        text: String::new(),
        tool_calls: vec![],
        id: data["id"].as_str().map(|s| s.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
    });
}
```

---

## Responses API `strict` Field Documentation

From the API reference (Function tool definition in Responses API):
```
strict: boolean
Whether to enforce strict parameter validation. Default true.
```

This confirms: **In the Responses API, `strict` defaults to `true`** (not just auto-normalization, the documented default IS `true`).

In contrast, Chat Completions API tool definition:
```
strict: boolean (optional)
Whether to enable strict schema adherence when generating the output.
```
No default value stated → defaults to `false` (non-strict).

---

## Key Insight for the Bug Investigation

The fundamental problem is an **API behavior mismatch**: aichat was built for Chat Completions semantics where omitting `strict` means "best-effort." When the same schemas are used through the Responses API (for Bedrock's OpenAI models), the semantics flip to "strict enforcement" — breaking tools with optional parameters.

The fix is trivial: add `"strict": false` to the Responses API tool definitions in `bedrock.rs:913-924`, or equivalently, make all schemas strict-compliant.
