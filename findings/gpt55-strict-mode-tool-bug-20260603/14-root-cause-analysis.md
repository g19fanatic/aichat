# Root Cause Analysis: aichat GPT-5.5 "Invalid Responses API response data" Error

## Executive Summary

The error occurs because aichat's Responses API implementation (a custom local patch in `bedrock.rs`) sends tool schemas to the OpenAI Responses API **without** the `strict: false` field. The Responses API auto-normalizes all schemas to strict mode, transforming optional parameters into required non-nullable fields. GPT-5.5's literal compliance behavior prevents it from generating output that would violate these normalized constraints, resulting in `"output": []` with 0 tokens. The `bedrock.rs` response parser then crashes with `bail!("Invalid Responses API response data")` because it cannot extract text or tool calls from an empty output array — unlike the Chat Completions handler (`openai.rs:427`) which handles this gracefully with a warning.

---

## The Observed Error

```
Error: Failed to call chat-completions api
Caused by: Invalid Responses API response data: {full JSON response}
```

**Key response characteristics:**
| Field | Value | Meaning |
|-------|-------|---------|
| `status` | `"completed"` | API considers request fully processed |
| `output` | `[]` | Zero content items generated |
| `output_tokens` | `0` | No tokens produced |
| `reasoning_tokens` | `0` | No reasoning attempted |
| `input_tokens` | `14,074` | Request received and processed |
| `error` | `null` | No API-level error |
| `incomplete_details` | `null` | Not truncated or filtered |
| `tool_choice` | `"auto"` | Model could choose any tool or text |
| `tools` count | 22 | All with `"strict": true` |

---

## PRIMARY Root Cause

### Responses API Strict Mode Auto-Normalization + Non-Compliant Schemas → Unsatisfiable Constraints

The fundamental issue is an **API behavioral mismatch**: aichat was designed for Chat Completions semantics where omitting the `strict` field means "best-effort schema matching." When the same schemas are sent through the Responses API (for Bedrock's OpenAI models), the semantics flip to **strict enforcement by default**, creating constraints the model cannot satisfy.

#### The Three-Part Failure

1. **aichat never sets `strict: false`** on tool schemas sent to the Responses API (0 occurrences of "strict" in entire `src/` directory). Per OpenAI docs: *"If you omit strict, Responses requests will normalize your schema into strict mode."*

2. **12/22 tool schemas violate strict mode** (48 total property violations). Properties defined in `properties` but not listed in `required` are auto-normalized to required non-nullable fields. Tools like `gui_session` (13/14 properties missing from required) and `ralph_loop` (10/10 missing) become impossible to call correctly.

3. **GPT-5.5 refuses rather than guesses.** Unlike GPT-4o's best-effort approach, GPT-5.5 *"interprets prompts in a literal and thorough manner"* and will not fabricate values for fields it cannot meaningfully fill. When ALL tools have unsatisfiable constraints, it produces zero output.

---

## CONTRIBUTING Factors (Ranked by Impact)

### Factor 1: `JsonSchema` Struct Cannot Represent Strict-Mode Fields (HIGH)
**File:** `src/function.rs:127-149`

The `JsonSchema` struct has no `additional_properties` field — it structurally cannot emit `"additionalProperties": false`. It also has no mechanism to emit nullable union types (`"type": ["string", "null"]`). This is an architectural limitation: the struct was designed before OpenAI introduced strict mode.

### Factor 2: bedrock.rs Crashes on Empty Output Instead of Warning (HIGH)
**File:** `src/client/bedrock.rs:981-982`

```rust
if text.is_empty() && tool_calls.is_empty() {
    bail!("Invalid Responses API response data: {data}");  // CRASH
}
```

Contrast with the Chat Completions handler (`openai.rs:427-437`) which returns empty gracefully with `warn!()`. Even if the schema issue is fixed, other scenarios (content filtering, model confusion) can produce empty output. The bail!() is a defense-in-depth failure.

### Factor 3: Tool Count Exceeds Soft Limit (MEDIUM)
22 tools exceed the documented soft recommendation of <20. From docs: *"Aim for fewer than 20 functions available at the start of a turn."* More tools = more schema complexity + more opportunities for constraint conflicts.

### Factor 4: Responses API Code Is a Custom Unreviewed Patch (MEDIUM)
**Commit:** `b92c83d` (Jun 3, 2026: "add bedrock-mantle support for openai models!")

PR #1318 proposing Responses API support was explicitly **rejected** by the upstream maintainer in Jun 2025: *"We will not support this feature."* The code in bedrock.rs was added later without upstream review. This explains why the bail!() inconsistency and the missing `strict: false` were never caught — no code review, no test coverage for edge cases.

### Factor 5: Zero Schema Transformation in Pipeline (LOW-MEDIUM)
The schema pipeline is pure passthrough: `functions.json` → deserialize → filter by role → serialize into API body. No validation, no transformation, no strict-mode compliance checking. Whatever the user puts in `functions.json` is sent verbatim to the API.

### Factor 6: Undocumented API Behavior (LOW)
The Responses API documentation says invalid strict schemas should be **rejected** at request time. Instead, it auto-normalizes AND accepts the request — but the resulting normalized constraints may be unsatisfiable. This is an OpenAI documentation gap.

---

## Causal Chain (Complete Step-by-Step)

```
1. User configures model "openai.gpt-5.5" in Bedrock provider
   └─→ BedrockModelCategory::from_model_name() returns OpenAI (bedrock.rs:38-44)

2. Request routes to /openai/v1/responses endpoint (bedrock.rs:91-95)
   └─→ build_responses_api_body() constructs the request (bedrock.rs:818-928)

3. Tool schemas serialized WITHOUT "strict" field (bedrock.rs:913-924)
   └─→ json!({"type":"function", "name":..., "parameters":...})
   └─→ No "strict": false → API will auto-normalize

4. Responses API receives 22 tool schemas and auto-normalizes to strict mode:
   └─→ Sets additionalProperties: false on all objects
   └─→ Adds ALL properties to required array
   └─→ Does NOT convert types to nullable ([type, null])
   └─→ Result: optional params become required non-nullable fields

5. GPT-5.5 evaluates normalized schemas:
   └─→ gui_session: must provide 14 non-null params for ANY action (contradictory)
   └─→ ralph_loop: must provide 10 mutually-exclusive params simultaneously
   └─→ recursive_grep: must provide all 9 params including boolean flags
   └─→ Model cannot produce valid tool call satisfying all constraints
   └─→ Model cannot produce plain text (reasoning shows 0 tokens)

6. GPT-5.5 returns "completed" with output: [] (0 output tokens, 0 reasoning tokens)
   └─→ API marks as "completed" (no infrastructure error)

7. responses_api_chat_completions() parses response (bedrock.rs:930-992):
   └─→ Iterates output array (empty) → text="" and tool_calls=[]
   └─→ if text.is_empty() && tool_calls.is_empty() → bail!()

8. Error propagates:
   └─→ bail!("Invalid Responses API response data: {data}")
   └─→ Wrapped by common.rs:77: "Failed to call chat-completions api"
   └─→ Displayed to user with full JSON response body
```

---

## Evidence Summary (From Tasks 1-11)

| Finding | Source Task | Key Evidence |
|---------|------------|--------------|
| Strict mode requires ALL props in required + additionalProperties:false | Task 1, 8 | OpenAI docs (function-calling guide, structured-outputs guide) |
| 12/22 tools violate strict mode (48 violations) | Task 2 | Analysis of error JSON tool schemas |
| ALL 22 tools missing additionalProperties:false | Task 2 | Universal violation |
| GPT-5.5 "literal and thorough" interpretation | Task 3 | OpenAI GPT-5.5 documentation |
| <20 tool soft limit recommendation | Task 3 | OpenAI function calling best practices |
| Responses API auto-normalizes when strict omitted | Task 1, 8, 11 | OpenAI docs: "Responses requests will normalize your schema" |
| Chat Completions defaults non-strict (different behavior) | Task 11 | OpenAI docs: "Chat Completions remain non-strict by default" |
| bail!() at bedrock.rs:982 (the crash) | Task 4 | Source code inspection |
| openai.rs:427 handles same condition gracefully | Task 4, 11 | Source code inspection |
| aichat never sets "strict" (0 occurrences in src/) | Task 4, 7 | grep of entire source directory |
| JsonSchema struct has no additionalProperties field | Task 7 | Struct definition at function.rs:127-149 |
| Schema pipeline is pure passthrough (no transformation) | Task 7 | Code path analysis |
| gui_session worst offender: 13/14 props missing from required | Task 9 | Schema analysis with severity ranking |
| ralph_loop: mutually exclusive params all become mandatory | Task 9 | Semantic analysis of tool parameters |
| Responses API code is custom patch (not upstream) | Task 10 | GitHub PR #1318 rejected; commit b92c83d is local |
| Related crashes on empty output fixed only for Chat Completions | Task 10 | Issues #1338, #1306 → PR #1340 (openai.rs only) |
| No existing GitHub issue reports this exact bug | Task 10 | Search of sigoden/aichat issues |

---

## Why "1 Simple Tool Call Works Just Fine"

The user's observation is **entirely consistent** with the root cause:

1. **Single tool, compliant schema**: If the tool has all properties in `required` (e.g., `fs_cat` with just `path`), auto-normalization is effectively a no-op. The model can satisfy all constraints.

2. **Single tool, non-compliant schema**: Even with violations, a single tool gives the model only ONE set of constraints to satisfy. It may find values for all forced-required params (or the schema may be simple enough to comply after normalization).

3. **22 tools, 12 non-compliant**: The model must evaluate ALL 22 tools to find one it can call. When 12/22 have impossible constraints AND the remaining 10 may not match the user's request, the model enters a state where no valid output path exists.

**The tipping point** is not a specific number but a function of: (violations × complexity × tool_count). At 22 tools with 48 violations across 12 tools (including the severe cases like gui_session with 13 contradictions), GPT-5.5's literal compliance behavior causes total output failure.

---

## Relationship to Known Issues

| Related Issue | Connection | Status |
|--------------|------------|--------|
| PR #1318 (Responses API support) | Rejected upstream; code appeared in fork anyway without review | Closed (rejected) |
| Issue #1431 (Responses API support) | Official support still requested; our code is custom workaround | Open |
| Issue #1338/#1306 (empty output crash) | Same pattern: model returns empty → aichat crashes. Fixed in PR #1340 for Chat Completions ONLY | Closed (Chat Completions only) |
| Issue #1325 (null schema validation) | Same class of bug: schema format incompatible with OpenAI validation | Closed (different fix) |
| Issue #1389 (GPT-5 function calling) | Same error message "Failed to call chat-completions api" with GPT-5 family | Closed (different root cause) |
| Issue #1495 (tool args lost) | Different mechanism but same fragility in tool call handling | Open |

---

## Classification

| Aspect | Classification |
|--------|---------------|
| Bug category | API compatibility (behavioral mismatch between APIs) |
| Severity | Critical (complete feature failure for GPT-5.5 with tools) |
| Reproducibility | Deterministic (every request with 22 violated tools fails) |
| Scope | Affects ALL Bedrock OpenAI models using Responses API + tools |
| Root cause type | Missing configuration (`strict: false`) + missing capability (`additionalProperties`) |
| Fix complexity | Low (one-line fix: add `"strict": false` to tool objects in bedrock.rs) |
| Defense-in-depth fix | Medium (graceful empty output handling in bedrock.rs) |
| Full fix | High (schema struct refactor + transformation pipeline) |

---

## Conclusion

This is a **two-layer bug**:

1. **Layer 1 (Root Cause)**: The Responses API's strict-mode auto-normalization creates unsatisfiable constraints from non-compliant tool schemas, causing GPT-5.5 to produce zero output. Fix: add `"strict": false` to tool definitions in `bedrock.rs:913-924`, OR make schemas compliant.

2. **Layer 2 (Error Handling)**: The bedrock.rs response parser crashes with `bail!()` on empty output instead of handling it gracefully like the Chat Completions parser. Fix: replace `bail!()` with `warn!()` + return empty at `bedrock.rs:981-982`.

Both layers should be fixed: Layer 1 prevents the model failure, Layer 2 provides resilience against future empty-output scenarios (content filters, rate limits, model confusion, etc.).

The single most impactful fix is a **one-line change**: adding `"strict": false` to the tool schema JSON in `bedrock.rs:913-924`. This opts out of auto-normalization, restoring the non-strict (best-effort) behavior that Chat Completions uses and that all existing tool schemas were designed for.
