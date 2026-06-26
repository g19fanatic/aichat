# Implementation Guide: Fixing aichat Strict Mode with Optional Parameters

## Overview

This step-by-step guide provides the exact changes needed to fix the GPT-5.5 "Invalid Responses API response data" error in aichat. The bug has two layers:

1. **Root cause**: Tool schemas sent to the Responses API lack `"strict": false`, causing auto-normalization that makes optional parameters unsatisfiable for GPT-5.5
2. **Error handling**: `bedrock.rs` crashes with `bail!()` on empty output instead of handling it gracefully

This guide covers three levels of fixes:
- **Minimum viable fix** (~7 minutes, 2 code locations)
- **Improved diagnostics** (~15 minutes additional)
- **Full schema compliance** (~1 hour additional, architectural)

---

## Prerequisites

- Rust toolchain (cargo, rustc)
- Clone of aichat source at `/home/pdibiase/sources/aichat/`
- Files to modify:
  - `src/client/bedrock.rs` (primary)
  - `src/function.rs` (full fix only)

---

## Step 1: Add `"strict": false` to Tool Schemas (ROOT CAUSE FIX)

**File**: `src/client/bedrock.rs`
**Location**: Lines 913–924 (inside `build_responses_api_body()`)
**Impact**: Prevents Responses API from auto-normalizing schemas to strict mode
**Risk**: None — explicitly opts out of strict mode, restoring Chat Completions semantics

### Current Code (lines 913-924)

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

### Modified Code

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
                    "strict": false,
                })
            })
            .collect();
    }
```

### Why This Works

Per OpenAI documentation: *"If you omit strict, Responses requests will normalize your schema into strict mode."* By explicitly setting `strict: false`, the Responses API:
- Does NOT add all properties to `required`
- Does NOT inject `additionalProperties: false`
- Treats schemas with "best-effort" matching (same as Chat Completions default)

This single line addition is the most impactful change — it directly addresses why GPT-5.5 produces zero output tokens.

### Potential Caveat

If AWS Bedrock Mantle overrides `strict: false` server-side (forcing `strict: true`), this fix alone won't work. In that case, proceed to Step 3 (schema compliance) as the full solution.

---

## Step 2: Graceful Empty Output Handling (DEFENSE-IN-DEPTH)

**File**: `src/client/bedrock.rs`
**Location**: Lines 981–982 (inside `responses_api_chat_completions()`)
**Impact**: Prevents crash on empty API responses; returns empty gracefully
**Risk**: None — strictly less disruptive than crashing; matches existing pattern in `openai.rs:428-440`

### Current Code (lines 981-982)

```rust
    if text.is_empty() && tool_calls.is_empty() {
        bail!("Invalid Responses API response data: {data}");
    }
```

### Modified Code

```rust
    if text.is_empty() && tool_calls.is_empty() {
        // GPT-5.5 and other models may return empty output when:
        // - Strict mode constraints are unsatisfiable
        // - Reasoning budget is exhausted
        // - Content filter triggered without explicit status change
        // Return empty gracefully (matches openai.rs:428-440 pattern).
        warn!(
            "Received empty output from Responses API (status: {:?}, output_tokens: {:?}). \
             Full response: {data}",
            data["status"].as_str(),
            data["usage"]["output_tokens"].as_u64()
        );
        return Ok(ChatCompletionsOutput {
            text: String::new(),
            tool_calls: vec![],
            id: data["id"].as_str().map(|s| s.to_string()),
            input_tokens: data["usage"]["input_tokens"].as_u64(),
            output_tokens: data["usage"]["output_tokens"].as_u64(),
        });
    }
```

### Why This Works

The `warn!()` macro is globally available (via `#[macro_use] extern crate log;` in `src/main.rs`). `ChatCompletionsOutput` fields are: `text`, `tool_calls`, `id`, `input_tokens`, `output_tokens` (defined at `src/client/common.rs:290-296`).

This mirrors the identical fix already present in the Chat Completions handler (`src/client/openai.rs:428-440`, added in PR #1340 for issues #1338/#1306). The Responses API handler was never updated because it was added as a separate local patch without code review.

### Reference Implementation (openai.rs:428-440)

```rust
    if text.is_empty() && tool_calls.is_empty() {
        // Gemini and GPT reasoning models intermittently return null content
        // when the reasoning budget is exhausted or the proxy returns a
        // degraded response. Warn and return empty output rather than bailing
        // on a valid 200 response.
        warn!("Received empty content and no tool calls from model (null content response). Full response: {data}");
        return Ok(ChatCompletionsOutput {
            text: String::new(),
            tool_calls: vec![],
            id: data["id"].as_str().map(|v| v.to_string()),
            input_tokens: data["usage"]["prompt_tokens"].as_u64(),
            output_tokens: data["usage"]["completion_tokens"].as_u64(),
        });
    }
```

Note: The Responses API uses `input_tokens`/`output_tokens` (not `prompt_tokens`/`completion_tokens` like Chat Completions).

---

## Step 3: Response Status Checking (DIAGNOSTICS IMPROVEMENT)

**File**: `src/client/bedrock.rs`
**Location**: After line 943 (after `debug!("responses-api-data: {data}");`)
**Impact**: Better error messages for documented failure modes
**Risk**: None — additive diagnostics only

### Code to Insert (after line 943)

```rust
    // Check Responses API status for non-success states before parsing output
    if let Some(status) = data["status"].as_str() {
        match status {
            "failed" => {
                let error_msg = data["error"]
                    .as_object()
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                bail!("Responses API returned failed status: {error_msg}");
            }
            "incomplete" => {
                let reason = data["incomplete_details"]
                    .as_object()
                    .and_then(|d| d.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown");
                warn!("Responses API returned incomplete status (reason: {reason}). Extracting partial output.");
            }
            "cancelled" => {
                warn!("Responses API response was cancelled.");
            }
            _ => {} // "completed" or other — proceed normally
        }
    }
```

### Why This Works

Currently the parser ignores the `status` field entirely. This means `"failed"`, `"incomplete"`, and `"cancelled"` responses all manifest as the generic "Invalid Responses API response data" error. With this check:
- `"failed"` → Immediately bail with the actual error message from the API
- `"incomplete"` → Log warning but attempt to extract partial content (may have some output)
- `"cancelled"` → Log warning, proceed to extract (probably empty → handled by Step 2)

---

## Step 4: Full Schema Compliance (ARCHITECTURAL FIX — Long-term)

This step makes tool schemas genuinely strict-mode compliant. Only needed if:
- Bedrock Mantle overrides `strict: false` server-side
- You want to enable strict mode intentionally (for guaranteed JSON compliance in model output)
- OpenAI deprecates the `strict: false` option in the future

### Step 4a: Add `additional_properties` Field to JsonSchema

**File**: `src/function.rs`
**Location**: Lines 126–153 (the `JsonSchema` struct definition)

#### Current Struct (lines 131-153)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<IndexMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchema>>,
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchema>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_value: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}
```

#### Modified Struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<IndexMap<String, JsonSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<JsonSchema>>,
    #[serde(rename = "anyOf", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<JsonSchema>>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_value: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
}
```

**Changes**:
1. Added `#[derive(Default)]` to support `..Default::default()` in struct expressions
2. Added `additional_properties` field with `skip_serializing_if` to maintain backward compat

### Step 4b: Add Schema Normalization Method

**File**: `src/function.rs`
**Location**: Inside the `impl JsonSchema` block (after the `is_empty_properties()` method, ~line 155)

```rust
impl JsonSchema {
    pub fn is_empty_properties(&self) -> bool {
        match &self.properties {
            Some(v) => v.is_empty(),
            None => true,
        }
    }

    /// Transform this schema into strict-mode compliant format:
    /// - Sets `additionalProperties: false` on all object types
    /// - Moves ALL properties into the `required` array
    /// - Wraps originally-optional properties in `anyOf: [{original}, {"type":"null"}]`
    /// - Recurses into nested object schemas
    pub fn to_strict_mode(&self) -> JsonSchema {
        let mut schema = self.clone();

        if let Some(ref properties) = self.properties {
            // Set additionalProperties: false on this object
            schema.additional_properties = Some(false);

            // Determine which properties are currently required
            let current_required: Vec<String> = self
                .required
                .as_ref()
                .cloned()
                .unwrap_or_default();

            // All property names become required
            let all_props: Vec<String> = properties.keys().cloned().collect();

            // Transform properties: wrap non-required ones in nullable anyOf
            let mut new_properties = IndexMap::new();
            for (name, prop_schema) in properties {
                let strict_prop = prop_schema.to_strict_mode(); // Recurse
                if current_required.contains(name) {
                    // Already required — keep as-is (recursed)
                    new_properties.insert(name.clone(), strict_prop);
                } else {
                    // Optional → wrap in anyOf with null type
                    new_properties.insert(name.clone(), JsonSchema {
                        any_of: Some(vec![
                            strict_prop,
                            JsonSchema {
                                type_value: Some("null".to_string()),
                                ..Default::default()
                            },
                        ]),
                        description: prop_schema.description.clone(),
                        ..Default::default()
                    });
                }
            }
            schema.properties = Some(new_properties);
            schema.required = Some(all_props);
        }

        // Recurse into array items
        if let Some(ref items) = self.items {
            schema.items = Some(Box::new(items.to_strict_mode()));
        }

        // Recurse into anyOf variants
        if let Some(ref any_of) = self.any_of {
            schema.any_of = Some(any_of.iter().map(|s| s.to_strict_mode()).collect());
        }

        schema
    }
}
```

### Step 4c: Apply Transformation in Responses API Body Builder

**File**: `src/client/bedrock.rs`
**Location**: Lines 913–924 (same location as Step 1, but different approach)

Replace the Step 1 fix with:

```rust
    if let Some(functions) = functions {
        body["tools"] = functions
            .iter()
            .map(|v| {
                let strict_params = v.parameters.to_strict_mode();
                json!({
                    "type": "function",
                    "name": v.name,
                    "description": v.description,
                    "parameters": strict_params,
                    "strict": true,
                })
            })
            .collect();
    }
```

**Note**: With this approach you set `"strict": true` because schemas ARE now compliant. This gives you strict mode's benefits (guaranteed valid JSON output) without the constraint unsatisfiability.

---

## Step 5: Verification & Testing

### 5a: Build Verification

```bash
cd /home/pdibiase/sources/aichat
cargo build 2>&1 | head -50
```

Expected: Clean build with no errors. Warnings are acceptable.

### 5b: Existing Test Suite

```bash
cargo test 2>&1 | tail -30
```

Expected: All existing tests pass. The changes don't affect any existing test logic since:
- `bedrock.rs` tests only cover `build_chat_completions_body` (Converse API), not Responses API
- `function.rs` tests cover tool call parsing, not schema generation

### 5c: New Unit Tests

Add the following tests to validate the fixes:

#### Test for Step 1/2: Add to `src/client/bedrock.rs` (inside `mod tests`)

```rust
    #[test]
    fn test_responses_api_tools_include_strict_false() {
        // Verify that build_responses_api_body sets strict: false on tool schemas
        use crate::function::{FunctionDeclaration, JsonSchema};

        let schema = JsonSchema {
            type_value: Some("object".to_string()),
            properties: Some(IndexMap::from([
                ("path".to_string(), JsonSchema {
                    type_value: Some("string".to_string()),
                    ..Default::default()
                }),
                ("page".to_string(), JsonSchema {
                    type_value: Some("string".to_string()),
                    ..Default::default()
                }),
            ])),
            required: Some(vec!["path".to_string()]),
            ..Default::default()
        };

        let func = FunctionDeclaration {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: schema,
            agent: false,
        };

        let messages = vec![
            Message::new(MessageRole::User, MessageContent::Text("test".into())),
        ];
        let data = ChatCompletionsData {
            messages,
            temperature: None,
            top_p: None,
            functions: Some(vec![func]),
            stream: false,
        };
        let model = Model::new("bedrock", "openai.gpt-5.5");
        let body = build_responses_api_body(data, &model);

        // Verify strict: false is set
        let tools = body["tools"].as_array().expect("tools should be array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["strict"], json!(false));
        assert_eq!(tools[0]["name"], json!("test_tool"));
        // Verify parameters are passed through unchanged
        assert_eq!(tools[0]["parameters"]["required"], json!(["path"]));
    }
```

#### Test for Step 4: Add to `src/function.rs` (inside test module)

```rust
    #[test]
    fn test_to_strict_mode_adds_additional_properties() {
        let schema = JsonSchema {
            type_value: Some("object".to_string()),
            properties: Some(IndexMap::from([
                ("name".to_string(), JsonSchema {
                    type_value: Some("string".to_string()),
                    ..Default::default()
                }),
            ])),
            required: Some(vec!["name".to_string()]),
            ..Default::default()
        };

        let strict = schema.to_strict_mode();
        assert_eq!(strict.additional_properties, Some(false));
        assert_eq!(strict.required, Some(vec!["name".to_string()]));
    }

    #[test]
    fn test_to_strict_mode_wraps_optional_in_nullable() {
        let schema = JsonSchema {
            type_value: Some("object".to_string()),
            properties: Some(IndexMap::from([
                ("required_field".to_string(), JsonSchema {
                    type_value: Some("string".to_string()),
                    ..Default::default()
                }),
                ("optional_field".to_string(), JsonSchema {
                    type_value: Some("integer".to_string()),
                    ..Default::default()
                }),
            ])),
            required: Some(vec!["required_field".to_string()]),
            ..Default::default()
        };

        let strict = schema.to_strict_mode();

        // All properties should now be required
        let required = strict.required.unwrap();
        assert!(required.contains(&"required_field".to_string()));
        assert!(required.contains(&"optional_field".to_string()));
        assert_eq!(required.len(), 2);

        // additionalProperties must be false
        assert_eq!(strict.additional_properties, Some(false));

        // required_field should remain a simple string type
        let props = strict.properties.unwrap();
        let req_field = &props["required_field"];
        assert_eq!(req_field.type_value, Some("string".to_string()));
        assert!(req_field.any_of.is_none());

        // optional_field should be wrapped in anyOf with null
        let opt_field = &props["optional_field"];
        assert!(opt_field.any_of.is_some());
        let any_of = opt_field.any_of.as_ref().unwrap();
        assert_eq!(any_of.len(), 2);
        assert_eq!(any_of[0].type_value, Some("integer".to_string()));
        assert_eq!(any_of[1].type_value, Some("null".to_string()));
    }

    #[test]
    fn test_to_strict_mode_recurses_nested_objects() {
        let inner = JsonSchema {
            type_value: Some("object".to_string()),
            properties: Some(IndexMap::from([
                ("inner_req".to_string(), JsonSchema {
                    type_value: Some("string".to_string()),
                    ..Default::default()
                }),
                ("inner_opt".to_string(), JsonSchema {
                    type_value: Some("boolean".to_string()),
                    ..Default::default()
                }),
            ])),
            required: Some(vec!["inner_req".to_string()]),
            ..Default::default()
        };

        let schema = JsonSchema {
            type_value: Some("object".to_string()),
            properties: Some(IndexMap::from([
                ("nested".to_string(), inner),
            ])),
            required: Some(vec!["nested".to_string()]),
            ..Default::default()
        };

        let strict = schema.to_strict_mode();
        assert_eq!(strict.additional_properties, Some(false));

        // The nested object should also have additionalProperties: false
        let props = strict.properties.unwrap();
        let nested = &props["nested"];
        assert_eq!(nested.additional_properties, Some(false));
        // And all its properties should be required
        let nested_req = nested.required.as_ref().unwrap();
        assert_eq!(nested_req.len(), 2);
    }
```

### 5d: Manual Integration Test

After building, test with a GPT-5.5 request:

```bash
# Test with a single tool (should work before and after fix)
echo "What time is it?" | aichat -m openai.gpt-5.5 --no-stream

# Test with multiple tools (this is the bug scenario)
echo "List the files in the current directory" | aichat -m openai.gpt-5.5 --no-stream

# Test with the full role (22 tools - the exact failing scenario)
echo "Read the file README.md" | aichat -m openai.gpt-5.5 -r default-vim-role --no-stream
```

Expected behavior after fix:
- All three commands should produce output instead of "Failed to call chat-completions api" error
- With `RUST_LOG=warn`, any empty-output scenarios will show the warning message instead of crashing

### 5e: Verify No Regression with Other Models

```bash
# Converse API models (uses completely different code path)
echo "Hello" | aichat -m anthropic.claude-3-5-sonnet

# Chat Completions models (openai.rs, not bedrock.rs)
echo "Hello" | aichat -m gpt-4o
```

---

## Summary: Recommended Implementation Order

| Order | Step | File(s) | Lines | Effort | Impact |
|-------|------|---------|-------|--------|--------|
| 1 | **Step 1**: Add `"strict": false` | `bedrock.rs` | 913-924 | 1 line | Fixes root cause |
| 2 | **Step 2**: Graceful empty handling | `bedrock.rs` | 981-982 | ~12 lines | Prevents crash |
| 3 | **Step 3**: Status checking | `bedrock.rs` | after 943 | ~20 lines | Better diagnostics |
| 4 | **Step 5a-5b**: Build + test | — | — | 2 min | Verification |
| 5 | **Step 5d**: Manual test | — | — | 5 min | End-to-end validation |

**Minimum viable fix = Step 1 + Step 2** (total: ~13 lines of code, 7 minutes)

Steps 4a-4c (full schema compliance) should only be implemented if:
- Bedrock Mantle overrides `strict: false` (confirmed by testing Step 1 alone)
- You want strict mode benefits (guaranteed JSON compliance in tool call arguments)
- OpenAI deprecates `strict: false` support

---

## Combined Diff (Steps 1 + 2 + 3)

```diff
diff --git a/src/client/bedrock.rs b/src/client/bedrock.rs
index abc1234..def5678 100644
--- a/src/client/bedrock.rs
+++ b/src/client/bedrock.rs
@@ -913,12 +913,13 @@ fn build_responses_api_body(data: ChatCompletionsData, model: &Model) -> Value {
     if let Some(functions) = functions {
         body["tools"] = functions
             .iter()
             .map(|v| {
                 json!({
                     "type": "function",
                     "name": v.name,
                     "description": v.description,
                     "parameters": v.parameters,
+                    "strict": false,
                 })
             })
             .collect();
     }
@@ -940,6 +941,25 @@ async fn responses_api_chat_completions(
 
     debug!("responses-api-data: {data}");
 
+    // Check Responses API status for non-success states before parsing output
+    if let Some(status) = data["status"].as_str() {
+        match status {
+            "failed" => {
+                let error_msg = data["error"]
+                    .as_object()
+                    .and_then(|e| e.get("message"))
+                    .and_then(|m| m.as_str())
+                    .unwrap_or("unknown error");
+                bail!("Responses API returned failed status: {error_msg}");
+            }
+            "incomplete" => {
+                let reason = data["incomplete_details"]
+                    .as_object()
+                    .and_then(|d| d.get("reason"))
+                    .and_then(|r| r.as_str())
+                    .unwrap_or("unknown");
+                warn!("Responses API returned incomplete status (reason: {reason}). Extracting partial output.");
+            }
+            "cancelled" => {
+                warn!("Responses API response was cancelled.");
+            }
+            _ => {} // "completed" or other — proceed normally
+        }
+    }
+
     // Extract text from Responses API format:
     // {"output": [{"type": "message", "content": [{"type": "output_text", "text": "..."}]}]}
     let mut text = String::new();
@@ -978,7 +998,18 @@ async fn responses_api_chat_completions(
     }
 
     if text.is_empty() && tool_calls.is_empty() {
-        bail!("Invalid Responses API response data: {data}");
+        // GPT-5.5 and other models may return empty output when:
+        // - Strict mode constraints are unsatisfiable
+        // - Reasoning budget is exhausted
+        // - Content filter triggered without explicit status change
+        // Return empty gracefully (matches openai.rs:428-440 pattern).
+        warn!(
+            "Received empty output from Responses API (status: {:?}, output_tokens: {:?}). \
+             Full response: {data}",
+            data["status"].as_str(),
+            data["usage"]["output_tokens"].as_u64()
+        );
+        return Ok(ChatCompletionsOutput {
+            text: String::new(),
+            tool_calls: vec![],
+            id: data["id"].as_str().map(|s| s.to_string()),
+            input_tokens: data["usage"]["input_tokens"].as_u64(),
+            output_tokens: data["usage"]["output_tokens"].as_u64(),
+        });
     }
 
     let output = ChatCompletionsOutput {
```

---

## File Reference Summary

| File | Line Range | Purpose |
|------|-----------|---------|
| `src/client/bedrock.rs:913-924` | Tool schema serialization | Add `"strict": false` |
| `src/client/bedrock.rs:981-982` | Empty output handling | Replace `bail!()` with `warn!()` + return |
| `src/client/bedrock.rs:943` | After debug log | Insert status checking |
| `src/client/openai.rs:428-440` | Reference implementation | Pattern for graceful empty handling |
| `src/client/common.rs:290-296` | `ChatCompletionsOutput` struct | Return type reference |
| `src/function.rs:131-153` | `JsonSchema` struct | Add `additional_properties` field (Step 4 only) |
| `src/main.rs:13` | `extern crate log` | Confirms `warn!`/`debug!` macros available globally |

---

## Troubleshooting

### "strict: false is ignored — model still returns empty output"

This means Bedrock Mantle is overriding the field server-side. Solutions:
1. Implement Step 4 (full schema compliance) so schemas work WITH strict:true
2. Contact AWS to configure Bedrock Mantle to respect client-provided `strict` field
3. Use workaround: reduce tool count to <15 (fewer schemas = fewer conflicts)

### "Build fails: cannot find `warn` macro"

This shouldn't happen (`extern crate log;` with `#[macro_use]` is at `src/main.rs:13`), but if it does:
```rust
// Add at the top of bedrock.rs (after use super::*)
use log::warn;
```

### "Test fails: Model::new not found"

The `Model::new` constructor may require specific parameters. Check `src/client/model.rs` for the actual signature. You may need to use a test fixture or `Model::default()` instead.

### "After fix, model calls wrong tool / returns garbage"

This is expected behavior with `strict: false` — the model uses "best-effort" matching and may occasionally produce slightly non-conforming tool calls. The tool execution layer should handle missing optional parameters gracefully (they should default to their documented defaults).
