# Response JSON Structure Analysis

## Overview

This document analyzes the actual OpenAI Responses API response received when using `openai.gpt-5.5` through AWS Bedrock, cross-references each field with aichat's parser implementation, and identifies which fields are unexpected or unused.

## Key Architecture Discovery

The `openai.gpt-5.5` model is routed through **AWS Bedrock** (not the direct OpenAI client), which uses Bedrock's OpenAI-compatible endpoint at `/openai/v1/responses`. This is critical:

- **File**: `src/client/bedrock.rs`
- **Routing logic** (line ~45): `BedrockModelCategory::from_model_name()` — if model name starts with `"openai."`, it uses `BedrockModelCategory::OpenAI`
- **Endpoint** (line 93): `/openai/v1/responses` (Responses API, NOT Chat Completions)
- **Parser** (line 932-992): `responses_api_chat_completions()` function

## Error Chain

```
common.rs:77   → "Failed to call chat-completions api" (outer wrapper)
bedrock.rs:982 → "Invalid Responses API response data: {data}" (inner cause)
```

The error occurs at `bedrock.rs:982` when `text.is_empty() && tool_calls.is_empty()` evaluates to `true` because `output: []` is empty.

## Complete Response Field Inventory

### Fields Present in Actual Response

| Field | Value in Response | Used by Parser? | Notes |
|-------|-------------------|-----------------|-------|
| `background` | `false` | ❌ NO | New Responses API field |
| `billing` | `{"payer": "developer"}` | ❌ NO | Billing metadata |
| `completed_at` | `1780516603` | ❌ NO | Unix timestamp of completion |
| `created_at` | `1780516558` | ❌ NO | Unix timestamp of creation |
| `error` | `null` | ❌ NO | **CRITICAL**: Could indicate why output is empty |
| `frequency_penalty` | `0.0` | ❌ NO | Echo of request parameter |
| `id` | `"resp_n7iolnydus..."` | ✅ YES | Extracted as response ID |
| `incomplete_details` | `null` | ❌ NO | **CRITICAL**: Could explain empty output |
| `instructions` | (full system prompt) | ❌ NO | Echo of request field |
| `max_output_tokens` | `null` | ❌ NO | Echo of request parameter |
| `max_tool_calls` | `null` | ❌ NO | New field — tool call limit |
| `metadata` | `{}` | ❌ NO | Custom metadata |
| `model` | `"openai.gpt-5.5"` | ❌ NO | Model name echo |
| `object` | `"response"` | ❌ NO | Object type identifier |
| `output` | `[]` (EMPTY) | ✅ YES | **THE PROBLEM**: Array is empty |
| `parallel_tool_calls` | `true` | ❌ NO | Request parameter echo |
| `presence_penalty` | `0.0` | ❌ NO | Request parameter echo |
| `previous_response_id` | `null` | ❌ NO | Conversation chaining |
| `prompt_cache_key` | `null` | ❌ NO | Prompt caching identifier |
| `prompt_cache_retention` | `"in_memory"` | ❌ NO | Cache retention policy |
| `reasoning` | `{"effort":"medium","summary":null,"context":"current_turn"}` | ❌ NO | Reasoning configuration |
| `safety_identifier` | `null` | ❌ NO | Safety system identifier |
| `service_tier` | `"default"` | ❌ NO | API service tier |
| `status` | `"completed"` | ❌ NO | **CRITICAL**: Response status |
| `store` | `true` | ❌ NO | Whether response is stored |
| `temperature` | `1.0` | ❌ NO | Request parameter echo |
| `text` | `{"format":{"type":"text"},"verbosity":"medium"}` | ❌ NO | Text format config |
| `tool_choice` | `"auto"` | ❌ NO | Tool selection strategy |
| `tools` | `[...22 tools...]` | ❌ NO | Echo of tools with `strict:true` added |
| `top_logprobs` | `0` | ❌ NO | Request parameter echo |
| `top_p` | `0.98` | ❌ NO | Request parameter echo |
| `truncation` | `"disabled"` | ❌ NO | Context truncation setting |
| `usage` | See below | ✅ PARTIAL | Only `input_tokens` and `output_tokens` |
| `user` | `null` | ❌ NO | User identifier |
| `moderation` | `null` | ❌ NO | Content moderation results |

### Usage Field Details

| Sub-field | Value | Used by Parser? |
|-----------|-------|-----------------|
| `usage.input_tokens` | `14074` | ✅ YES |
| `usage.input_tokens_details.cached_tokens` | `0` | ❌ NO |
| `usage.output_tokens` | `0` | ✅ YES (but value is 0!) |
| `usage.output_tokens_details.reasoning_tokens` | `0` | ❌ NO |
| `usage.total_tokens` | `14074` | ❌ NO |

## What aichat's Parser Actually Extracts

From `responses_api_chat_completions()` at `bedrock.rs:932-992`:

```rust
// 1. Iterates over data["output"] array items
// 2. For each item with content array, extracts text from content[*]["text"]
// 3. For items where type == "function_call", extracts name, arguments, call_id
// 4. Extracts data["id"] as response ID
// 5. Extracts data["usage"]["input_tokens"]
// 6. Extracts data["usage"]["output_tokens"]
```

**Total fields used**: 6 out of 30+ fields present in the response.

## Critical Fields That SHOULD Be Checked But Aren't

### 1. `status` — Response Completion Status
- **Values**: `"completed"`, `"incomplete"`, `"failed"`, `"in_progress"`, `"cancelled"`
- **Why important**: A "completed" status with empty output is anomalous. An "incomplete" or "failed" status would explain empty output.
- **Current behavior**: Completely ignored — parser goes straight to extracting output content.

### 2. `error` — Error Information
- **Values**: `null` or `{"type": "...", "message": "..."}`
- **Why important**: If the API returned an error object, this would explain the empty output.
- **Current behavior**: Never checked. The parser only checks HTTP status code, not the response-level error field.

### 3. `incomplete_details` — Incompletion Reason
- **Values**: `null` or `{"reason": "max_output_tokens" | "content_filter" | ...}`
- **Why important**: If output was truncated or blocked, this explains empty output.
- **Current behavior**: Never checked.

### 4. `output_tokens: 0` — Token Usage Indicator
- **Why important**: Zero output tokens confirms the model generated NOTHING. Could be used as a diagnostic signal.
- **Current behavior**: Extracted but only for reporting, not for decision-making.

### 5. `reasoning.effort: "medium"` — Reasoning Budget
- **Why important**: If reasoning consumed the budget without producing output, this is diagnostic info.
- **Current behavior**: Completely ignored.

## Fields That Are NOT Causing Parse Failures

The following fields are present but do **NOT** cause parser failures because:
- aichat uses `serde_json::Value` (flexible JSON), not strict struct deserialization
- Unknown fields are simply ignored when accessing specific paths
- No schema validation is performed on the response

Therefore: `billing`, `prompt_cache_key`, `prompt_cache_retention`, `safety_identifier`, `moderation`, `service_tier`, etc. are **harmless** — they're just ignored.

## The `strict: true` Auto-Application

Key finding: **aichat does NOT set `strict: true`** anywhere in its source code. The `build_responses_api_body` function builds tool schemas as:

```rust
json!({
    "type": "function",
    "name": v.name,
    "description": v.description,
    "parameters": v.parameters,
})
```

The `"strict": true` visible in the response's `tools` array is **auto-applied by the Responses API** (confirmed by OpenAI documentation: the Responses API auto-normalizes schemas to strict mode when `strict` is omitted).

This means the API is applying strict validation rules to schemas that were NOT designed for strict mode — specifically:
- `additionalProperties: false` is NOT set in aichat's `JsonSchema` struct (`src/function.rs:126-150`)
- Many tool schemas have properties NOT listed in their `required` array

## Root Cause Connection

The response field analysis reveals the error is a **two-factor failure**:

1. **Model-side**: GPT-5.5 receives schemas auto-normalized to strict mode but with violations (missing required properties, no additionalProperties:false). It generates 0 output tokens — silently refuses rather than producing content.

2. **Parser-side**: `responses_api_chat_completions()` doesn't check `status`, `error`, or `incomplete_details` before deciding the response is invalid. When output is empty, it bails with a generic error instead of:
   - Logging a warning and returning empty output (like `openai_extract_chat_completions` does)
   - Checking the `status` field for diagnostic information
   - Checking `error` field for API-level errors

## Comparison: OpenAI Client vs Bedrock Client Empty Output Handling

### Direct OpenAI Client (`openai.rs:420-440`):
```rust
if text.is_empty() && tool_calls.is_empty() {
    warn!("Received empty content and no tool calls...");
    return Ok(ChatCompletionsOutput { text: String::new(), ... });
}
```
**Graceful**: Logs warning, returns empty output.

### Bedrock Responses API Client (`bedrock.rs:980-982`):
```rust
if text.is_empty() && tool_calls.is_empty() {
    bail!("Invalid Responses API response data: {data}");
}
```
**Crashes**: Bails with error, dumps entire response JSON.

## Recommendations

1. **Immediate fix**: Change `bedrock.rs:982` to match the OpenAI client's graceful handling — warn and return empty output instead of bailing.
2. **Better diagnostics**: Before bailing/warning, check `data["status"]`, `data["error"]`, and `data["incomplete_details"]` to provide informative error messages.
3. **Schema compliance**: Either remove `strict: true` echo reliance or ensure schemas include `additionalProperties: false` and all properties in `required`.

## Source File References

- `src/client/bedrock.rs:45` — `BedrockModelCategory::from_model_name()` routing
- `src/client/bedrock.rs:93` — `/openai/v1/responses` endpoint selection
- `src/client/bedrock.rs:818` — `build_responses_api_body()` tool schema construction
- `src/client/bedrock.rs:932-992` — `responses_api_chat_completions()` response parser
- `src/client/bedrock.rs:982` — The exact bail point causing the error
- `src/client/common.rs:77` — "Failed to call chat-completions api" wrapper
- `src/client/openai.rs:420-440` — Graceful empty output handling (for comparison)
- `src/function.rs:118-150` — `FunctionDeclaration` and `JsonSchema` structs (no strict/additionalProperties)
