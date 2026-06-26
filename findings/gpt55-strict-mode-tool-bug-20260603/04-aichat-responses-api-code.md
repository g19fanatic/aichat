# Finding 04: aichat Source Code - Responses API Parsing & Tool Schema Generation

## Summary

The aichat Responses API handling lives entirely in `src/client/bedrock.rs`. OpenAI models accessed via AWS Bedrock with names prefixed `openai.` are routed through the Responses API (`/openai/v1/responses`). The source code does NOT set `strict: true` on tool schemas — the Responses API auto-normalizes them to strict mode. When GPT-5.5 returns an empty output array, the parser throws a hard error rather than handling it gracefully (unlike the Chat Completions parser which warns and continues).

## Key Code Locations

### 1. Model Routing Decision
**File**: `src/client/bedrock.rs:38-44`

```rust
impl BedrockModelCategory {
    fn from_model_name(model_name: &str) -> Self {
        if model_name.starts_with("openai.") {
            BedrockModelCategory::OpenAI
        } else {
            BedrockModelCategory::Converse
        }
    }
}
```

Any model name starting with `openai.` (like `openai.gpt-5.5`) routes through the Responses API path instead of the AWS Converse API.

### 2. Responses API Request Construction
**File**: `src/client/bedrock.rs:91-95`

```rust
BedrockModelCategory::OpenAI => {
    let uri = "/openai/v1/responses".to_string();
    let body = build_responses_api_body(data, &self.model);
    (uri, body)
}
```

### 3. Tool Schema Generation (WHERE STRICT SHOULD BE BUT ISN'T)
**File**: `src/client/bedrock.rs:913-924`

```rust
if let Some(functions) = functions {
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
}
```

**Critical observation**: No `"strict"` field is set. The Responses API auto-normalizes all tool schemas to `strict: true` when the field is omitted (documented behavior). This means schemas that violate strict mode rules (missing properties in `required`, no `additionalProperties: false`) will be flagged by the API.

### 4. The Error Path (THE BUG)
**File**: `src/client/bedrock.rs:930-992` (`responses_api_chat_completions`)

```rust
async fn responses_api_chat_completions(
    builder: RequestBuilder,
    _model: &Model,
) -> Result<ChatCompletionsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = res.json().await?;
    if !status.is_success() {
        catch_error(&data, status.as_u16())?;
    }

    // Extract text from Responses API format
    let mut text = String::new();
    let mut tool_calls = vec![];
    if let Some(output_arr) = data["output"].as_array() {
        for item in output_arr {
            // ... parse message content and function_call items ...
        }
    }

    if text.is_empty() && tool_calls.is_empty() {
        bail!("Invalid Responses API response data: {data}");  // LINE 982 - THE ERROR
    }
    // ...
}
```

**The error triggers when**:
1. API returns HTTP 200 (success) with `"status": "completed"`
2. The `"output"` array is empty (`[]`)
3. Therefore both `text` and `tool_calls` remain empty
4. The `bail!` macro fires, producing the user-visible error

### 5. Contrast: Chat Completions Handles Empty Gracefully
**File**: `src/client/openai.rs:427-437`

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

The Chat Completions handler **warns** and returns empty output gracefully. The Responses API handler **crashes** with a hard error. This is an inconsistency that should be fixed.

### 6. JsonSchema Struct (No additionalProperties Support)
**File**: `src/function.rs:126-149`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_value: Option<String>,
    pub description: Option<String>,
    pub properties: Option<IndexMap<String, JsonSchema>>,
    pub items: Option<Box<JsonSchema>>,
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchema>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_value: Option<Vec<String>>,
    pub default: Option<Value>,
    pub required: Option<Vec<String>>,
}
```

**Critical missing fields**:
- No `additional_properties` / `additionalProperties` field
- The struct physically cannot represent `"additionalProperties": false`
- The `required` field only contains what's explicitly declared in `functions.json`
- There's no logic anywhere to auto-populate `required` with all property names

### 7. Function Declarations Loading
**File**: `src/function.rs:74-95`

Functions are loaded from `functions.json` via `Functions::init(declarations_path)`. The JSON is deserialized directly into `Vec<FunctionDeclaration>` with `serde_json`. No post-processing adds `additionalProperties` or validates `required` completeness.

### 8. `strict` Search Result
**Grep result**: The string "strict" appears **ZERO times** in the entire `src/` directory. Aichat never explicitly sets strict mode — it relies entirely on API defaults.

## Root Cause Chain (Code Path)

```
User configures model "openai.gpt-5.5" in bedrock provider
    → BedrockModelCategory::from_model_name("openai.gpt-5.5") = OpenAI
    → Endpoint: /openai/v1/responses
    → build_responses_api_body() constructs request
        → tools[] generated WITHOUT "strict" field
        → parameters from JsonSchema WITHOUT "additionalProperties"
        → required[] only has explicitly declared required params (not all)
    → Responses API auto-normalizes: strict=true applied
    → Schema violations detected by GPT-5.5 (12/22 tools have undeclared required props)
    → GPT-5.5 returns "completed" with output: [] (0 tokens)
    → responses_api_chat_completions() parses response
        → output array is empty → text="" and tool_calls=[]
        → bail!("Invalid Responses API response data: {data}")
```

## Potential Fixes (Code Level)

### Fix 1: Explicitly set `strict: false` on tools (prevents auto-normalization)
```rust
// In build_responses_api_body, change tool generation to:
json!({
    "type": "function",
    "name": v.name,
    "description": v.description,
    "parameters": v.parameters,
    "strict": false,  // Prevent auto-normalization to strict mode
})
```

### Fix 2: Handle empty output gracefully (matches Chat Completions behavior)
```rust
// In responses_api_chat_completions, replace bail! with warning:
if text.is_empty() && tool_calls.is_empty() {
    warn!("Responses API returned empty output. Full response: {data}");
    return Ok(ChatCompletionsOutput {
        text: String::new(),
        tool_calls: vec![],
        id: data["id"].as_str().map(|s| s.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
    });
}
```

### Fix 3: Add `additionalProperties` to JsonSchema and auto-populate `required`
```rust
// Add to JsonSchema struct:
#[serde(rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
pub additional_properties: Option<bool>,

// Post-processing after loading functions.json:
// For each schema with properties, set additionalProperties=false
// and add all property keys to required[]
```

### Fix 4 (Recommended): Combination of Fix 1 + Fix 2
- Set `strict: false` to prevent the API from enforcing strict schemas (fixes root cause)
- Also handle empty output gracefully (defense in depth for other empty-output scenarios)

## Files Involved

| File | Lines | Purpose |
|------|-------|---------|
| `src/client/bedrock.rs` | 38-44 | Model category routing |
| `src/client/bedrock.rs` | 91-95 | Responses API URI selection |
| `src/client/bedrock.rs` | 816-927 | `build_responses_api_body` - request construction |
| `src/client/bedrock.rs` | 913-924 | Tool schema JSON generation (missing strict field) |
| `src/client/bedrock.rs` | 930-992 | `responses_api_chat_completions` - response parsing |
| `src/client/bedrock.rs` | 981-982 | The error: bail on empty output |
| `src/client/bedrock.rs` | 995-1065 | `responses_api_streaming` - streaming handler |
| `src/client/openai.rs` | 427-437 | Chat Completions empty handling (graceful - contrast) |
| `src/function.rs` | 126-149 | `JsonSchema` struct (missing additionalProperties) |
| `src/function.rs` | 74-95 | Function loading from functions.json |
| `src/client/common.rs` | 281-286 | `ChatCompletionsData` struct |

## Key Insight

The Responses API code path in `bedrock.rs` was likely written for specific models and not fully tested with models that might return empty output. The Chat Completions code in `openai.rs` already handles this edge case gracefully (with a comment noting "Gemini and GPT reasoning models intermittently return null content"). The Responses API handler should adopt the same pattern.
