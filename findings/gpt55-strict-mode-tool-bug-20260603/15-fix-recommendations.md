# Fix Recommendations: aichat GPT-5.5 "Invalid Responses API response data" Error

## Summary

Based on root cause analysis (Task 14), this document provides specific actionable fixes ranked by category:
- **(a) Source code fixes** — changes to aichat Rust source
- **(b) Configuration workarounds** — no code changes required
- **(c) Model-side mitigations** — reduce likelihood of triggering the issue

The PRIMARY root cause is two-layered:
1. **Schema incompatibility**: Responses API auto-normalizes to strict mode, making non-compliant schemas unsatisfiable for GPT-5.5
2. **Crash on empty output**: `bedrock.rs:981` uses `bail!()` instead of graceful handling when the model produces no output

---

## Category (a): Source Code Fixes

### Fix A1: Graceful Empty Output Handling (Priority: CRITICAL — Defense in Depth)

**Impact**: Prevents crash regardless of WHY output is empty
**Effort**: ~5 minutes (one-block change)
**Risk**: Low — matches existing pattern in `openai.rs:428-437`

**File**: `src/client/bedrock.rs:981-982`

**Before** (current code):
```rust
if text.is_empty() && tool_calls.is_empty() {
    bail!("Invalid Responses API response data: {data}");
}
```

**After** (fix):
```rust
if text.is_empty() && tool_calls.is_empty() {
    // GPT-5.5 and other models may return empty output when strict mode
    // constraints are unsatisfiable, reasoning budget is exhausted, or
    // content filtering triggers. Return empty gracefully.
    warn!("Received empty output from Responses API (status: {:?}, output_tokens: {:?}). Full response: {data}",
        data["status"].as_str(),
        data["usage"]["output_tokens"].as_u64());
    return Ok(ChatCompletionsOutput {
        text: String::new(),
        tool_calls: vec![],
        id: data["id"].as_str().map(|s| s.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
    });
}
```

**Why this works**: The crash is replaced with a warning + empty return. This matches the pattern in `openai.rs:428-437` (added by PR #1340) and allows the calling code to handle the empty response gracefully. Even after other fixes, this is essential defense-in-depth against future empty-output scenarios (content filtering, rate limiting, model confusion, API changes).

**Note**: Ensure `use log::warn;` or equivalent macro is in scope at the top of `bedrock.rs`.

---

### Fix A2: Add `strict: false` to Responses API Tool Schemas (Priority: HIGH — Root Cause Fix)

**Impact**: Prevents Responses API from auto-normalizing schemas to strict mode
**Effort**: ~2 minutes (one field addition)
**Risk**: Low — explicitly opts out of strict mode; restores Chat Completions semantics

**File**: `src/client/bedrock.rs:916-922`

**Before** (current code):
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

**After** (fix):
```rust
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
```

**Why this works**: Per OpenAI documentation: *"If you omit strict, Responses requests will normalize your schema into strict mode."* By explicitly setting `strict: false`, we tell the Responses API to NOT auto-normalize schemas. This restores the same "best-effort" schema matching that Chat Completions uses by default — which is what all existing tool schemas were designed for.

**Caveat**: If Bedrock Mantle (the AWS proxy) overrides this field with `strict: true` server-side (as observed in the error response), this fix alone may not be sufficient. In that case, Fix A3 (schema compliance) becomes mandatory, or the user must coordinate with AWS to configure Bedrock Mantle's behavior.

**Testing**: After applying this fix, if the error persists (Bedrock Mantle overrides), the `warn!()` from Fix A1 will show in logs instead of a crash, confirming Mantle is the blocker.

---

### Fix A3: Add `additionalProperties: false` to JsonSchema Struct (Priority: HIGH — Architectural)

**Impact**: Enables strict-mode compliant schema generation
**Effort**: ~30 minutes (struct change + serialization + pipeline transformation)
**Risk**: Medium — changes serialization behavior for ALL clients

**File**: `src/function.rs:127-143`

**Step 1: Add field to JsonSchema struct**
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
    // NEW: Required for OpenAI strict mode compliance
    #[serde(rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
}
```

**Step 2: Add strict-mode normalization function**
```rust
impl JsonSchema {
    /// Transform schema to be strict-mode compliant:
    /// - All properties added to `required`
    /// - `additionalProperties: false` set on all objects
    /// - Original optional params become nullable (anyOf with null)
    pub fn to_strict_mode(&self) -> JsonSchema {
        let mut schema = self.clone();
        
        if let Some(ref properties) = schema.properties {
            // Set additionalProperties: false on this object
            schema.additional_properties = Some(false);
            
            // Collect all property names
            let all_props: Vec<String> = properties.keys().cloned().collect();
            let current_required: Vec<String> = schema.required
                .as_ref()
                .map(|r| r.clone())
                .unwrap_or_default();
            
            // Transform non-required properties to nullable
            let mut new_properties = IndexMap::new();
            for (name, prop_schema) in properties {
                let mut new_prop = prop_schema.to_strict_mode(); // Recurse
                if !current_required.contains(name) {
                    // Wrap in anyOf with null for optional params
                    new_prop = JsonSchema {
                        any_of: Some(vec![
                            new_prop,
                            JsonSchema {
                                type_value: Some("null".to_string()),
                                ..Default::default()
                            },
                        ]),
                        description: prop_schema.description.clone(),
                        ..Default::default()
                    };
                }
                new_properties.insert(name.clone(), new_prop);
            }
            schema.properties = Some(new_properties);
            
            // ALL properties must be in required
            schema.required = Some(all_props);
        }
        
        // Recurse into items (for array types)
        if let Some(ref items) = schema.items {
            schema.items = Some(Box::new(items.to_strict_mode()));
        }
        
        schema
    }
}
```

**Step 3: Apply transformation in Responses API body builder**
```rust
// In build_responses_api_body (bedrock.rs:914-926):
body["tools"] = functions
    .iter()
    .map(|v| {
        let strict_params = v.parameters.to_strict_mode();
        json!({
            "type": "function",
            "name": v.name,
            "description": v.description,
            "parameters": strict_params,
            "strict": true,  // Now schemas ARE compliant
        })
    })
    .collect();
```

**Why this works**: Instead of fighting the Responses API's strict mode, we make schemas genuinely compliant. The model receives schemas where:
- All properties are required (it must provide values)
- Optional parameters accept `null` (the model can pass null when unsure)
- `additionalProperties: false` is explicit (no extra fields)

This is the "proper" fix but requires more testing. GPT-5.5 can now produce valid tool calls by passing `null` for optional parameters.

---

### Fix A4: Check Response Status and Error Fields (Priority: MEDIUM — Diagnostics)

**Impact**: Better error messages; potential recovery from documented failure modes
**Effort**: ~15 minutes
**Risk**: Low — additive diagnostics

**File**: `src/client/bedrock.rs:938-942` (after `let data: Value = res.json().await?;`)

**Addition** (insert after HTTP status check, before output extraction):
```rust
// Check Responses API status field for non-success states
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
            warn!("Responses API returned cancelled status.");
        }
        _ => {} // "completed" or other
    }
}
```

**Why this works**: Currently the parser ignores the `status` field entirely (proceeds to extract from `output[]` regardless). This means errors, incomplete responses, and cancellations all manifest as the same "Invalid Responses API response data" error. With this check, users get meaningful error messages for documented failure modes.

---

### Fix A5: Add Configurable Strict Mode Toggle (Priority: LOW — Future-Proofing)

**Impact**: Allows users to control strict mode behavior per-model
**Effort**: ~1 hour (config plumbing + client integration)
**Risk**: Low — opt-in feature, defaults to current behavior

**Concept**: Add a `strict_tools` option to model configuration:
```yaml
# In ~/.config/aichat/config.yaml (model-level):
clients:
  - type: bedrock
    models:
      - name: openai.gpt-5.5
        strict_tools: true  # false = send strict:false, true = normalize schemas
```

This would allow users to choose between:
- `strict_tools: false` (default) — sends `strict: false`, no schema transformation
- `strict_tools: true` — applies `to_strict_mode()` transformation and sends `strict: true`

---

## Category (b): Configuration Workarounds

### Workaround B1: Reduce Tool Count Below 20 (Effectiveness: HIGH)

**Approach**: In the role definition, restrict `use_tools` to only the tools needed for the current task.

**Before** (role YAML):
```yaml
---
use_tools: code_assistant
---
```

Where `code_assistant` registers all 22 tools.

**After** (role YAML — selective tools):
```yaml
---
use_tools: fs_cat,fs_read,fs_write,fs_ls,recursive_grep,safe_script_executor,patch
---
```

**Why this works**: Reducing from 22 to <20 tools (ideally <15) reduces schema complexity below GPT-5.5's misbehavior threshold. OpenAI explicitly recommends: *"Aim for fewer than 20 functions available at the start of a turn."* By providing only task-relevant tools, the model has fewer constraints to satisfy.

**Trade-off**: Reduced tool availability per session. May need multiple role definitions for different task types.

---

### Workaround B2: Fix functions.json to Be Strict-Mode Compliant (Effectiveness: HIGH)

**Approach**: Manually edit `~/.config/aichat/functions/functions.json` to make all schemas strict-mode compliant.

**For each tool, apply these transformations:**

1. Add ALL property names to the `required` array
2. Add `"additionalProperties": false` to the top-level parameters object
3. Wrap optional parameters in nullable union type

**Example — Transform `fs_read` schema:**

**Before:**
```json
{
  "name": "fs_read",
  "description": "Read a specific page of a file...",
  "parameters": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "The path of the file to read" },
      "page": { "type": "string", "description": "Page number (default: 1)" },
      "size": { "type": "string", "description": "Lines per page (default: 10)" }
    },
    "required": ["path"]
  }
}
```

**After (strict-compliant):**
```json
{
  "name": "fs_read",
  "description": "Read a specific page of a file...",
  "parameters": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "The path of the file to read" },
      "page": {
        "anyOf": [{ "type": "string" }, { "type": "null" }],
        "description": "Page number (default: 1)"
      },
      "size": {
        "anyOf": [{ "type": "string" }, { "type": "null" }],
        "description": "Lines per page (default: 10)"
      }
    },
    "required": ["path", "page", "size"],
    "additionalProperties": false
  }
}
```

**Full list of tools requiring manual fixes** (12/22 — from Task 2):
| Tool | Properties to add to `required` |
|------|-------------------------------|
| `fs_git_diff` | cached, path |
| `fs_info` | max_preview_lines |
| `fs_read` | page, size |
| `fs_write` | contents |
| `gui_session` | session_id, screen, app_cmd, commands, context, debug, delay_ms, include_base64, include_screenshot, model, output_path, prompt, resize_width |
| `patch` | encoding |
| `ralph_loop` | create_workspace, findings_slug, max_consecutive_errors, max_iterations, max_parallel, project_dir, ralph_dir, resume, steps, worker_timeout |
| `recursive_grep` | count, exclude_hidden, file_pattern, ignore_case, line_number, max_depth, whole_word |
| `skills` | skill_dir |
| `subagent` | model, parent_task_id, original_cwd |
| `whitelist_command` | command, list_options |
| `sequentialthinking` | branchFromThought, branchId, isRevision, needsMoreThoughts, revisesThought |

**Effort**: ~30-60 minutes manual JSON editing
**Trade-off**: Must be maintained as tools evolve. Will be overwritten if tools are regenerated.

**Important**: Even after adding all properties to `required`, you MUST also add `"additionalProperties": false` to EVERY object-typed schema (including nested objects) for full compliance.

---

### Workaround B3: Use Streaming Mode (Effectiveness: MEDIUM)

**Approach**: If aichat supports streaming configuration, use streaming for GPT-5.5 requests.

**Why**: The streaming handler (`responses_api_streaming` at bedrock.rs:997-1060) does NOT have a `bail!()` for empty output. It simply processes SSE events and returns `Ok(())` when `response.completed` is received. An empty-output response would result in no text/tool_calls being emitted to the handler — which may surface as an empty response to the user rather than a crash.

**Limitation**: This doesn't fix the root cause (model still produces nothing). It just prevents the crash. The user would see an empty response instead of an error.

---

### Workaround B4: Switch to Chat Completions Endpoint (Effectiveness: HIGH — if possible)

**Approach**: If the Bedrock provider supports routing `openai.gpt-5.5` through Chat Completions instead of Responses API, configure that.

**Why**: Chat Completions API:
- Does NOT auto-normalize schemas to strict mode (defaults to non-strict)
- Already has graceful empty-output handling in aichat (`openai.rs:428-437`)
- Is what all existing tool schemas were designed for

**Implementation**: This would require changes to `bedrock.rs:39-45` (the `from_model_name` routing) OR the AWS Bedrock Mantle configuration. If Bedrock Mantle supports a `/openai/v1/chat/completions` endpoint, changing the URI from `/openai/v1/responses` to `/openai/v1/chat/completions` at `bedrock.rs:91` would route to the compatible API.

**Risk**: The Responses API was likely chosen for specific capabilities (built-in tool execution, conversation state). Switching to Chat Completions may lose those features.

---

## Category (c): Model-Side Mitigations

### Mitigation C1: Increase Reasoning Effort (Effectiveness: LOW-MEDIUM)

**Approach**: Set `reasoning.effort` to `"high"` instead of the default `"medium"`.

**Why**: With higher reasoning effort, GPT-5.5 gets more tokens for internal deliberation. This MIGHT allow it to:
- Analyze schemas more carefully
- Find tools with satisfiable constraints
- Work around normalized strict schemas

**Implementation**: In `build_responses_api_body`, check if model supports reasoning and set:
```rust
if model.real_name().contains("gpt-5") {
    body["reasoning"] = json!({"effort": "high"});
}
```

**Limitation**: OpenAI docs warn that *"Higher reasoning effort isn't automatically better. If the task has conflicting instructions... higher effort can lead to overthinking."* Since the constraints are genuinely unsatisfiable, more reasoning may not help — the model may just spend more tokens determining it cannot produce valid output.

---

### Mitigation C2: Use Tool Phasing via System Prompt (Effectiveness: MEDIUM)

**Approach**: Instead of registering all 22 tools, use a phased approach where the system prompt instructs the model to only consider certain tools for the current task.

**Example system prompt addition:**
```
For this turn, only use tools from this priority list: fs_cat, fs_read, recursive_grep, patch, safe_script_executor.
All other tools are available but should only be used if the primary tools cannot accomplish the task.
```

**Why**: Even with 22 tools registered, if the model is instructed to prioritize a subset, it may only attempt to construct calls for the prioritized tools. If those tools have simpler (or compliant) schemas, the model can succeed.

**Limitation**: This is a "soft" mitigation — GPT-5.5's literal compliance behavior may still evaluate ALL tool schemas regardless of the system prompt instruction.

---

### Mitigation C3: Switch to GPT-4o for Tool-Heavy Tasks (Effectiveness: HIGH)

**Approach**: Use `openai.gpt-4o` or `openai.gpt-4o-mini` instead of `openai.gpt-5.5` when the task requires many tools.

**Why**: GPT-4o uses "best-effort" schema matching — it will produce output even when schemas aren't strictly compliant. It doesn't interpret strict mode as literally as GPT-5.5.

**Trade-off**: GPT-4o has lower reasoning capability than GPT-5.5. For complex tasks that need strong reasoning AND many tools, this is a compromise.

**Implementation**: Configure model per-role:
```yaml
# For tool-heavy roles:
---
model: openai.gpt-4o
use_tools: all
---

# For reasoning-heavy roles (few tools):
---
model: openai.gpt-5.5
use_tools: fs_cat,fs_read,safe_script_executor
---
```

---

### Mitigation C4: Add `phase` Parameter to Responses API Request (Effectiveness: MEDIUM)

**Approach**: Use the Responses API `phase` parameter (introduced for GPT-5.5) to control tool execution flow.

**Why**: From OpenAI docs: *"The phase parameter prevents early stopping by ensuring the model completes its planned tool sequence."* While this is designed for multi-tool orchestration, it may help the model commit to a specific tool call path rather than evaluating all 22 tools simultaneously.

**Implementation** (add to `build_responses_api_body`):
```rust
// For GPT-5.5 models with tools:
if model.real_name().contains("gpt-5") && functions.is_some() {
    body["phase"] = json!("tool_call");
}
```

**Limitation**: The `phase` parameter is poorly documented and may not be available through Bedrock Mantle.

---

## Recommended Fix Order

For immediate relief, apply fixes in this order:

| Order | Fix | Impact | Effort | Dependencies |
|-------|-----|--------|--------|-------------|
| 1 | **A1** (graceful empty output) | Prevents crash | 5 min | None |
| 2 | **A2** (strict: false) | Fixes root cause | 2 min | None |
| 3 | **B1** (reduce tool count) | Immediate workaround | 5 min | None |
| — | *Test after steps 1-3* | — | — | — |
| 4 | **A4** (status checking) | Better diagnostics | 15 min | A1 |
| 5 | **A3** (schema compliance) | Proper long-term fix | 30 min | None |
| 6 | **B2** (fix functions.json) | Manual workaround | 60 min | None |
| 7 | **C3** (use GPT-4o) | Fallback | 5 min | None |

**Minimum viable fix**: A1 + A2 (7 minutes of code changes, 2 lines total impact)

**If Bedrock Mantle overrides `strict: false`**: A1 + B2 or A1 + A3

---

## Fix Validation Matrix

| Fix | Addresses Root Cause? | Prevents Crash? | Works if Mantle Overrides? | Backward Compatible? |
|-----|----------------------|-----------------|---------------------------|---------------------|
| A1 | ❌ (defense only) | ✅ | ✅ | ✅ |
| A2 | ✅ | ❌ (needs A1 too) | ❌ (Mantle may override) | ✅ |
| A3 | ✅ | ❌ (needs A1 too) | ✅ | ⚠️ (changes schema wire format) |
| A4 | ❌ (diagnostics) | ❌ | ✅ | ✅ |
| B1 | Partially | ❌ | ✅ | ✅ |
| B2 | ✅ | ❌ (needs A1 too) | ✅ | ⚠️ (tool runners must handle null) |
| C3 | ❌ (avoids trigger) | ❌ | ✅ | ✅ |

---

## Complete Minimum Patch (Copy-Paste Ready)

For the two highest-priority source code fixes (A1 + A2), here's the combined diff:

```diff
--- a/src/client/bedrock.rs
+++ b/src/client/bedrock.rs
@@ -913,6 +913,7 @@ fn build_responses_api_body(...) -> Value {
             json!({
                 "type": "function",
                 "name": v.name,
                 "description": v.description,
                 "parameters": v.parameters,
+                "strict": false,
             })
         })
         .collect();
@@ -978,7 +979,15 @@ async fn responses_api_chat_completions(...) -> Result<ChatCompletionsOutput> {
     }
 
     if text.is_empty() && tool_calls.is_empty() {
-        bail!("Invalid Responses API response data: {data}");
+        // GPT-5.5 and other models may return empty output when strict mode
+        // constraints are unsatisfiable or reasoning budget is exhausted.
+        warn!("Received empty output from Responses API (status: {:?}, output_tokens: {:?}). Full response: {data}",
+            data["status"].as_str(),
+            data["usage"]["output_tokens"].as_u64());
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

## Sources

- Root cause analysis: `findings/14-root-cause-analysis.md`
- Schema generation code: `findings/07-schema-generation-code.md`
- Error path analysis: `findings/12-error-path-analysis.md`
- Empty output root cause: `findings/06-empty-output-root-cause.md`
- aichat source: `src/client/bedrock.rs` (lines 913-992)
- aichat source: `src/client/openai.rs` (lines 428-437, graceful handling reference)
- aichat source: `src/function.rs` (lines 127-143, JsonSchema struct)
- OpenAI docs: function calling strict mode, Responses API, GPT-5.5 guide
