# Error Path Analysis: "Invalid Responses API response data"

## Summary

The error `"Invalid Responses API response data"` is produced at **`src/client/bedrock.rs:981`** when the Responses API returns a `"completed"` status with an empty `output: []` array. The error is NOT caused by a deserialization failure or unexpected field — it's caused by the **absence of extractable content** in an otherwise valid HTTP 200 response. The error message is misleading.

## Complete Call Chain

### 1. Entry Point: `common.rs:72-77`

```rust
async fn chat_completions(&self, input: Input) -> Result<ChatCompletionsOutput> {
    // ...
    self.chat_completions_inner(&client, data)
        .await
        .with_context(|| "Failed to call chat-completions api")  // ← outer error wrapping
}
```

This adds the "Failed to call chat-completions api" context to ANY error from the inner handler.

### 2. Dispatch: `bedrock.rs:248-255`

```rust
async fn chat_completions_inner(&self, client: &ReqwestClient, data: ChatCompletionsData) -> Result<ChatCompletionsOutput> {
    let (builder, category) = self.chat_completions_builder(client, data)?;
    match category {
        BedrockModelCategory::Converse => chat_completions(builder).await,
        BedrockModelCategory::OpenAI => {
            responses_api_chat_completions(builder, &self.model).await  // ← GPT-5.5 goes here
        }
    }
}
```

### 3. Model Category Routing: `bedrock.rs:39-45`

```rust
impl BedrockModelCategory {
    fn from_model_name(model_name: &str) -> Self {
        if model_name.starts_with("openai.") {   // ← "openai.gpt-5.5" matches
            BedrockModelCategory::OpenAI
        } else {
            BedrockModelCategory::Converse
        }
    }
}
```

Any model prefixed `"openai."` routes to the Responses API path.

### 4. Request Construction: `bedrock.rs:82-95`

```rust
BedrockModelCategory::OpenAI => {
    let uri = "/openai/v1/responses".to_string();          // ← Responses API endpoint
    let body = build_responses_api_body(data, &self.model); // ← builds request body
    (uri, body)
}
```

Host: `bedrock-mantle.{region}.api.aws`
Header added: `x-amzn-mantle-client-agent: codex`

### 5. Body Builder: `bedrock.rs:818-928` (`build_responses_api_body`)

```rust
fn build_responses_api_body(data: ChatCompletionsData, model: &Model) -> Value {
    // Converts messages → "input" array
    // System messages → "instructions" field
    // Tool results → function_call + function_call_output items
    
    let mut body = json!({
        "model": model.real_name(),
        "input": input,
        "stream": stream,
    });
    
    // Tools: ONLY type, name, description, parameters — NO "strict" field
    if let Some(functions) = functions {
        body["tools"] = functions.iter().map(|v| {
            json!({
                "type": "function",
                "name": v.name,
                "description": v.description,
                "parameters": v.parameters,       // ← raw JsonSchema, no additionalProperties field
            })
        }).collect();
    }
    body
}
```

**Critical observation**: aichat NEVER sets `"strict": true` on tool schemas. The Bedrock Mantle API server (AWS infrastructure) adds `strict: true` to all tools before forwarding to OpenAI. This means aichat cannot control whether strict mode is applied.

### 6. Response Handler (THE ERROR): `bedrock.rs:932-992`

```rust
async fn responses_api_chat_completions(
    builder: RequestBuilder,
    _model: &Model,
) -> Result<ChatCompletionsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = res.json().await?;        // ← deserialize as untyped Value (NO struct!)
    
    if !status.is_success() {
        catch_error(&data, status.as_u16())?;   // ← HTTP error handling (NOT triggered - 200 OK)
    }

    debug!("responses-api-data: {data}");

    // Extract text from Responses API format
    let mut text = String::new();
    let mut tool_calls = vec![];
    if let Some(output_arr) = data["output"].as_array() {
        for item in output_arr {
            // Look for message content with text
            if let Some(content_arr) = item["content"].as_array() {
                for content_item in content_arr {
                    if let Some(t) = content_item["text"].as_str() {
                        text.push_str(t);
                    }
                }
            }
            // Look for function_call items
            if item["type"].as_str() == Some("function_call") {
                if let (Some(name), Some(arguments_str)) = (
                    item["name"].as_str(),
                    item["arguments"].as_str(),
                ) {
                    let call_id = item["call_id"].as_str().map(|s| s.to_string());
                    let arguments: Value = arguments_str.parse()?;
                    tool_calls.push(ToolCall::new(name.to_string(), arguments, call_id));
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // ███ THIS IS THE ERROR LINE (bedrock.rs:981) ███
    // ═══════════════════════════════════════════════════════════════
    if text.is_empty() && tool_calls.is_empty() {
        bail!("Invalid Responses API response data: {data}");  // ← THE ERROR
    }

    let output = ChatCompletionsOutput {
        text,
        tool_calls,
        id: data["id"].as_str().map(|s| s.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
    };
    Ok(output)
}
```

## What Triggers the Error

The error fires when BOTH conditions are true:
1. `text` is empty (no `output[].content[].text` fields found)
2. `tool_calls` is empty (no `output[].type == "function_call"` items found)

In our case, the response has `"output": []` (literally empty array), so the for loop body never executes, and both remain empty.

## Why Output is Empty

The API response shows:
- `"status": "completed"` — API considers the request successful
- `"output": []` — model generated ZERO output items
- `"usage": {"output_tokens": 0, "reasoning_tokens": 0}` — model consumed 0 output tokens

This happens because:
1. Bedrock Mantle adds `"strict": true` to all tool schemas
2. Strict mode requires ALL properties in `required` array + `additionalProperties: false`
3. The tool schemas violate both requirements (12/22 tools have non-required properties; 0/22 have additionalProperties)
4. GPT-5.5 is hyper-compliant: when it cannot produce valid output conforming to strict schemas, it produces nothing rather than violating the schema
5. The "completed" status with empty output is an undocumented edge case

## Deserialization Approach

**There is NO typed deserialization.** The response is parsed as `serde_json::Value` (dynamic JSON) and traversed manually with indexing:
- `data["output"].as_array()` — get output array
- `item["content"].as_array()` — get content from message items  
- `content_item["text"].as_str()` — extract text
- `item["type"].as_str()` — check item type
- `item["name"].as_str()` / `item["arguments"].as_str()` — extract function call data

The `JsonSchema` struct (function.rs:127-149) used for tool definitions has NO `additional_properties` field:
```rust
pub struct JsonSchema {
    pub type_value: Option<String>,
    pub description: Option<String>,
    pub properties: Option<IndexMap<String, JsonSchema>>,
    pub items: Option<Box<JsonSchema>>,
    pub any_of: Option<Vec<JsonSchema>>,
    pub enum_value: Option<Vec<String>>,
    pub default: Option<Value>,
    pub required: Option<Vec<String>>,
    // ← NO additionalProperties field!
}
```

## Critical Comparison: Chat Completions vs Responses API Handler

### openai.rs:428-439 (GRACEFUL — Chat Completions)
```rust
if text.is_empty() && tool_calls.is_empty() {
    // Gemini and GPT reasoning models intermittently return null content
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

### bedrock.rs:981 (CRASH — Responses API)
```rust
if text.is_empty() && tool_calls.is_empty() {
    bail!("Invalid Responses API response data: {data}");  // ← CRASHES
}
```

The Chat Completions handler was fixed by PR #1340 to gracefully handle this case. The Responses API handler was never fixed because:
1. It was added later (commit b92c83d, Jun 3, 2026)
2. Upstream aichat explicitly rejected Responses API support (PR #1318 closed)
3. This is a local custom patch with no upstream review

## Fields Ignored by the Parser

The response contains 30+ fields. The parser only uses 6:
- `output[].content[].text` — message text extraction
- `output[].type` — to identify function_call items
- `output[].name` — function call name
- `output[].arguments` — function call arguments
- `output[].call_id` — function call ID
- `id` — response ID
- `usage.input_tokens` / `usage.output_tokens` — token counts

**Completely ignored fields** (all present in the response):
- `status` ("completed") — NOT checked for success!
- `error` — NOT checked!
- `incomplete_details` — NOT checked!
- `output_text` — NOT used!
- `billing` — ignored
- `prompt_cache_key` — ignored
- `moderation` — ignored
- `safety_identifier` — ignored
- `parallel_tool_calls` — ignored
- All metadata fields

## The Bug: Two-Part Failure

### Part 1: Missing Graceful Handling (Immediate Bug)
The `bail!()` at bedrock.rs:981 should be a `warn!()` + return empty, matching openai.rs:428-439. This is a simple code fix.

### Part 2: Schema Non-Compliance (Root Cause)
The tool schemas sent to the API violate strict mode requirements because:
1. `JsonSchema` struct has no `additionalProperties` field → cannot emit `additionalProperties: false`
2. `required` arrays don't include optional parameters → violates strict mode requirement
3. aichat cannot set `strict: false` because it doesn't control the field (Bedrock Mantle adds it)

### Fix Path
1. **Immediate**: Change `bail!()` to `warn!()` + return empty (prevents crash)
2. **Proper**: Either:
   - Add `additionalProperties: false` to JsonSchema struct + populate `required` with all properties
   - OR find a way to set `strict: false` explicitly in the tool schema to override Mantle's addition
   - OR reduce tool count below GPT-5.5's soft limit (~20)

## Error Message as Seen by User

```
Error: Failed to call chat-completions api              ← common.rs:77 with_context
Caused by: Invalid Responses API response data: {...}   ← bedrock.rs:981 bail!
```

The full JSON response is dumped in the error message, which is why it's so verbose.
