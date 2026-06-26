# 18: Strict Mode Interaction Patterns Across OpenAI Models

## Executive Summary

This document provides a comprehensive explainer of how `strict: true` interacts with tool schemas across different OpenAI models and APIs. The critical finding is that **strict mode behavior is not uniform** — it varies by:

1. **API surface**: Responses API auto-normalizes to strict; Chat Completions does not
2. **Model generation**: GPT-5.5 refuses to produce output when constraints are unsatisfiable; GPT-4o uses best-effort
3. **Schema compliance level**: Violations that are silently tolerated by one combination can cause total failure in another

The interaction between these three dimensions creates a **compatibility matrix** where the same tool schemas can work perfectly with GPT-4o on Chat Completions but produce zero output with GPT-5.5 on the Responses API.

---

## Part 1: What Strict Mode Is

### The Three Cardinal Rules

When `strict: true` is active on a tool/function definition, OpenAI enforces **constrained decoding** — the model's token generation is constrained to produce only JSON that exactly matches the schema. Three rules must hold:

| Rule | Requirement | Example |
|------|-------------|---------|
| 1. `additionalProperties: false` | Every object (root + nested) must include this | `{"type": "object", ..., "additionalProperties": false}` |
| 2. All properties in `required` | Every key defined in `properties` must appear in `required` | `"required": ["location", "units"]` (ALL of them) |
| 3. Nullable types for optional params | Use `"type": ["string", "null"]` instead of omitting from `required` | Model can output `null` for conceptually optional fields |

### Strict Mode as Constrained Decoding

Under strict mode, the model does not simply "try to follow" the schema — it is **physically constrained** at the token-generation level. The decoding process rejects any token sequence that would produce JSON not matching the schema. This is fundamentally different from non-strict "best-effort" mode where the model is free to generate any JSON.

This distinction is critical: under constrained decoding, if the schema creates impossible constraints, the model has **zero valid token sequences** to produce, resulting in zero output tokens.

---

## Part 2: How Strict Mode Differs by API

### The Asymmetry: Responses API vs Chat Completions

| Behavior | Responses API (`/v1/responses`) | Chat Completions API (`/v1/chat/completions`) |
|----------|-------------------------------|----------------------------------------------|
| **Default when `strict` omitted** | **Auto-normalizes to strict** | Non-strict (best-effort) |
| **Auto-normalization** | Adds `additionalProperties: false`, marks ALL properties as `required` | None |
| **Schema acceptance** | Accepts and normalizes invalid schemas (no rejection) | No normalization; schemas used as-is |
| **`strict: true` + invalid schema** | Documented: "request will be rejected" | Same |
| **`strict: false` (explicit)** | Opts out; best-effort mode | Same as default |
| **Impact on model** | Constrained decoding applied | Unconstrained generation |

### The Critical Quote (From OpenAI Documentation)

> "If you omit `strict`, the default depends on the API: **Responses requests will normalize your schema into strict mode** (for example, by setting `additionalProperties: false` and marking all fields as required), which can **make previously optional fields mandatory**, while Chat Completions requests remain non-strict by default. To opt out of strict mode in Responses and keep non-strict, best-effort function calling, explicitly set `strict: false`."

### What Auto-Normalization Actually Does

When the Responses API auto-normalizes a non-compliant schema:

1. **Adds `additionalProperties: false`** to all objects (prevents model from adding extra fields)
2. **Moves ALL properties into `required`** (previously optional fields become mandatory)
3. **Does NOT convert types to nullable** (no `["string", "null"]` added)

This creates a critical gap: fields become required AND non-nullable simultaneously. The model MUST provide a non-null value for every property but has no escape hatch (like passing `null`).

### Practical Example: Before and After Normalization

**Schema as sent (non-compliant):**
```json
{
  "type": "function",
  "name": "search_files",
  "parameters": {
    "type": "object",
    "properties": {
      "pattern": {"type": "string"},
      "directory": {"type": "string"},
      "max_depth": {"type": "integer"},
      "ignore_case": {"type": "boolean"}
    },
    "required": ["pattern", "directory"]
  }
}
```

**Schema after Responses API auto-normalization:**
```json
{
  "type": "function",
  "name": "search_files",
  "strict": true,
  "parameters": {
    "type": "object",
    "properties": {
      "pattern": {"type": "string"},
      "directory": {"type": "string"},
      "max_depth": {"type": "integer"},
      "ignore_case": {"type": "boolean"}
    },
    "required": ["pattern", "directory", "max_depth", "ignore_case"],
    "additionalProperties": false
  }
}
```

**The problem:** `max_depth` is now required AND must be an integer (no null allowed). `ignore_case` is now required AND must be a boolean. The model MUST provide specific values for these even when the user's request has no relevance to them.

---

## Part 3: GPT-5.5 vs GPT-4o Behavioral Differences

### Fundamental Philosophy Difference

| Dimension | GPT-4o (and GPT-4.1) | GPT-5.5 |
|-----------|----------------------|---------|
| **Schema adherence** | Best-effort heuristic | Literal compliance |
| **When constraints conflict** | Produces output anyway (may violate some constraints) | Refuses to produce output (generates 0 tokens) |
| **Instruction interpretation** | Infers intent, fills gaps | "Literal and thorough manner" |
| **Tool selection strategy** | Picks best tool, approximates args | Evaluates all tools formally, requires constraint satisfaction |
| **Empty output likelihood** | Rare (always tries to generate something) | Possible (refuses rather than produces invalid output) |
| **Strict mode treatment** | Guideline to follow | Absolute constraint to obey |
| **Reasoning model** | No (direct generation) | Yes (reasoning tokens used for planning) |
| **Default reasoning effort** | N/A | Medium |

### GPT-4o: "Best-Effort" Mode

GPT-4o (and GPT-4.1) treats tool schemas as **guidelines**:

- If a property is "required" but the model doesn't know what to put, it may omit it or guess
- If `additionalProperties: false` is set but the model wants to add a field, it might still try
- Schema violations produce degraded output but rarely zero output
- The model prioritizes **being helpful** over **being schema-compliant**

**Result:** Non-compliant schemas with GPT-4o on Chat Completions work fine in practice. The model fills in what it can, omits what it can't, and the user gets a response.

### GPT-5.5: "Literal Compliance" Mode

GPT-5.5 treats tool schemas as **hard constraints**:

- From docs: *"GPT-5.5 interprets prompts in a literal and thorough manner"*
- From docs: *"Reliable schema adherence with strict mode"*
- From docs: *"Higher reasoning effort isn't automatically better. If the task has conflicting instructions... higher effort can lead to overthinking"*

When `strict: true` is active and constraints are unsatisfiable:
1. GPT-5.5 performs reasoning (or attempts to) about which tool to call
2. It evaluates each tool's constraints formally
3. If no tool has satisfiable constraints, it produces **zero output tokens**
4. The API returns `"status": "completed"` with `"output": []`
5. This is NOT a refusal, NOT an error, NOT a content filter — it's the model's legitimate response to unsatisfiable constraints

**Result:** The same non-compliant schemas that work with GPT-4o cause **total output failure** with GPT-5.5 when strict mode is applied.

### Why GPT-5.5 Specifically Fails (Not GPT-4o)

The difference comes down to **constrained vs unconstrained decoding**:

```
GPT-4o + Chat Completions + schema violations:
  → Non-strict (unconstrained) decoding
  → Model can generate any valid JSON
  → Omits optional fields, fills required ones as best it can
  → ALWAYS produces some output
  → Result: Works (with potential schema mismatches)

GPT-5.5 + Responses API + schema violations:
  → Auto-normalized to strict (constrained) decoding
  → Model can ONLY generate JSON matching the normalized schema
  → All properties now required, all non-nullable
  → Many tools have semantically contradictory required params
  → Zero valid token sequences exist for some tools
  → Result: Empty output (0 tokens)
```

---

## Part 4: The Interaction Matrix

### The Full Compatibility Grid

| Model | API | `strict` field | Schema Quality | Result |
|-------|-----|---------------|---------------|--------|
| GPT-4o | Chat Completions | omitted | Non-compliant | ✅ Works (best-effort) |
| GPT-4o | Chat Completions | `true` | Non-compliant | ❌ API rejects request |
| GPT-4o | Chat Completions | `true` | Compliant | ✅ Works (constrained) |
| GPT-4o | Chat Completions | `false` | Any | ✅ Works (best-effort) |
| GPT-4.1 | Chat Completions | omitted | Non-compliant | ✅ Works (best-effort) |
| GPT-4.1 | Chat Completions | `true` | Compliant | ✅ Works (constrained) |
| GPT-5.5 | Responses API | omitted | Non-compliant | ⚠️ **SILENT FAILURE** (empty output) |
| GPT-5.5 | Responses API | omitted | Compliant | ✅ Works (auto-strict) |
| GPT-5.5 | Responses API | `true` | Non-compliant | ❌ API rejects request |
| GPT-5.5 | Responses API | `true` | Compliant | ✅ Works (constrained) |
| GPT-5.5 | Responses API | `false` | Any | ✅ Works (best-effort) |
| GPT-5.5 | Chat Completions | omitted | Non-compliant | ✅ Works (best-effort) |

### The Dangerous Quadrant

The **single most dangerous combination** is:
- **Model:** GPT-5.5 (or any reasoning model with literal compliance)
- **API:** Responses API (auto-normalizes)
- **`strict` field:** Omitted (triggers auto-normalization)
- **Schema quality:** Non-compliant (optional properties not nullable, not all in required)

This combination is dangerous because:
1. No API-level error occurs (request is accepted)
2. Auto-normalization creates unsatisfiable constraints silently
3. The model returns `"completed"` status (appears successful)
4. Output is empty (0 tokens, 0 reasoning)
5. The failure is **deterministic** (not intermittent)
6. There is no indication in the response of WHAT went wrong

### Migration Trap

Developers migrating from GPT-4o (Chat Completions) to GPT-5.5 (Responses API) hit this trap because:
- Their schemas were designed for non-strict mode
- Optional parameters are expressed by omitting from `required` (not nullable)
- Everything works perfectly until switching to Responses API
- The first symptom is a cryptic "empty output" with no error explanation

---

## Part 5: Schema Violation Severity Taxonomy

### When Strict Mode Is Active, Violations Are Not Equal

| Pattern | Description | GPT-4o Impact | GPT-5.5 Impact |
|---------|-------------|---------------|----------------|
| **A: Missing `additionalProperties`** | Object doesn't explicitly bar extra properties | None (best-effort) | Low-Medium (model already constrained) |
| **B: Empty `required` array** | All properties become mandatory via normalization | None (best-effort) | **CRITICAL** (mutually exclusive params all mandatory) |
| **C: Partial `required`** | Some optional props forced mandatory | None (best-effort) | **HIGH** (model must invent values for irrelevant params) |
| **D: Non-nullable optional** | `"type": "string"` instead of `["string", "null"]` | None (model omits param) | **HIGH** (model cannot pass null, must produce string) |
| **E: Contradictory params** | Params designed for different use cases, all now required | N/A (never all required) | **CRITICAL** (logically impossible to satisfy) |

### Worst Offender Examples

**Pattern E (Critical) — Mutually exclusive workflows:**
```json
// ralph_loop: has create_workspace, ralph_dir, project_dir, resume
// These are THREE different modes: create OR run OR resume
// After normalization: ALL must be provided simultaneously = impossible
```

**Pattern C+D (High) — Large optional surface:**
```json
// gui_session: 14 properties, only "action" originally required
// After normalization: model must provide 14 non-null values
// But action="list" needs ZERO additional params
// Model cannot determine valid strings for session_id, app_cmd, etc.
```

---

## Part 6: The Undocumented Edge Case

### "Completed" + Empty Output = Undocumented Behavior

OpenAI documents several failure modes for Responses API requests:

| Documented Failure | Status | Output | How It Manifests |
|-------------------|--------|--------|-----------------|
| Safety refusal | `"completed"` | Contains `refusal` content | Explicit refusal message in output |
| Max tokens exceeded | `"incomplete"` | Partial output | `incomplete_details` populated |
| Content filter | `"incomplete"` | May have partial output | Filter type documented |
| API error | N/A | Error response | HTTP error code returned |

**The undocumented case** (what GPT-5.5 produces with unsatisfiable strict schemas):

| Actual Behavior | Status | Output | Indicators |
|----------------|--------|--------|------------|
| Constraint unsatisfiable | `"completed"` | `[]` (empty) | 0 output tokens, 0 reasoning tokens, null error |

This is NOT documented anywhere in OpenAI's API reference. It appears to be the model's response when:
- It completes processing (hence "completed")
- It has no valid token sequence to produce (hence empty output)
- It's not a refusal, not filtered, not truncated
- The API infrastructure has no error to report

### Detection Heuristic

To detect this condition programmatically:
```
if status == "completed" AND output == [] AND output_tokens == 0:
    → Likely unsatisfiable strict mode constraints
    → Check tool schemas for normalization violations
    → Consider adding strict: false
```

---

## Part 7: The Tool Count Amplification Effect

### How Tool Count Interacts with Strict Mode

Tool count compounds the strict mode problem:

| Tools | Schema Quality | GPT-4o Behavior | GPT-5.5 Behavior |
|-------|---------------|-----------------|------------------|
| 1 (compliant) | Good | ✅ Works | ✅ Works |
| 1 (non-compliant) | Bad | ✅ Works (best-effort) | ⚠️ Usually works (simple enough to satisfy) |
| 5 (mixed compliance) | Mixed | ✅ Works | ⚠️ May work (finds compliant tool) |
| 22 (12 non-compliant) | Poor | ✅ Works (best-effort) | ❌ **Fails** (constraint explosion) |

### Why Scale Matters for GPT-5.5

With N tools where K are non-compliant:
1. GPT-5.5 evaluates all N tool schemas before deciding which to call
2. For each non-compliant tool, normalized constraints may be unsatisfiable
3. The model must find at least ONE tool it can call with valid arguments
4. If no satisfiable tool matches the user's request → zero output
5. More tools = higher probability that ALL matching tools are unsatisfiable

**OpenAI's documented soft limit**: "Aim for fewer than 20 functions available at the start of a turn."

This recommendation exists specifically because tool-heavy flows cause "misbehavior" — the empty output pattern is likely one instance of such misbehavior.

---

## Part 8: Model-Specific Strict Mode Recommendations

### GPT-4o / GPT-4.1 (Non-Reasoning Models)

| Recommendation | Reason |
|---------------|--------|
| `strict: true` with compliant schemas is recommended | Guarantees output matches schema exactly |
| Non-compliant schemas are tolerated | Best-effort mode fills gaps |
| Tool count not a hard blocker | Handles 20+ tools with best-effort |
| Chat Completions API preferred | No auto-normalization surprises |

### GPT-5.5 (Reasoning Model)

| Recommendation | Reason |
|---------------|--------|
| **Either** `strict: false` **OR** fully compliant schemas | Auto-normalization creates unsatisfiable constraints |
| Verify ALL properties in `required` when using strict | Model takes this literally |
| Use nullable types for optional params: `["type", "null"]` | Only escape hatch under strict mode |
| Keep tool count ≤ 20 | Documented recommendation; higher counts amplify failures |
| Use `tool_search` for large catalogs | Defers unused tools (gpt-5.4+) |
| Monitor for 0-token responses | Indicates constraint satisfaction failure |
| Always set `strict` explicitly | Never rely on API-default behavior |

### Cross-Model Migration Checklist

When migrating tool-calling code from GPT-4o to GPT-5.5:

- [ ] Switch from Chat Completions to Responses API (if not already)
- [ ] Add `"strict": false` to all tool definitions OR make schemas compliant
- [ ] If using strict: Convert all optional params to nullable (`["type", "null"]`)
- [ ] If using strict: Add ALL properties to `required` array
- [ ] If using strict: Add `additionalProperties: false` to all objects
- [ ] Reduce tool count below 20 or implement tool_search
- [ ] Add graceful handling for empty output (`output: []`) responses
- [ ] Test with representative prompts that exercise different tools

---

## Part 9: The aichat Case Study

### How This Bug Manifests

The aichat GPT-5.5 bug is a perfect real-world example of this interaction pattern:

```
Configuration:
  - Model: openai.gpt-5.5 (reasoning model, literal compliance)
  - API: Responses API via Bedrock Mantle (/openai/v1/responses)
  - strict field: OMITTED (triggers auto-normalization)
  - Schemas: 22 tools, 12 non-compliant (48 property violations total)
  - None have additionalProperties: false

Result:
  - API auto-normalizes all schemas → strict mode
  - 12 tools become unsatisfiable (optional params now required + non-nullable)
  - GPT-5.5 evaluates all tools → none satisfiable for the request
  - Returns: status="completed", output=[], tokens=0
  - aichat parser: bail!("Invalid Responses API response data")
```

### Why It Works with "1 Simple Tool Call"

When the user reports that "1 simple tool call works just fine":
- Single tool with all properties in `required` = compliant after normalization
- GPT-5.5 can satisfy all constraints for a simple, fully-specified tool
- The failure only manifests with complex multi-tool schemas having optional parameters

### The One-Line Fix

```rust
// In bedrock.rs tool serialization:
json!({
    "type": "function",
    "name": v.name,
    "description": v.description,
    "parameters": v.parameters,
    "strict": false,  // <-- Opts out of auto-normalization
})
```

This single addition restores the non-strict (best-effort) behavior that:
- Chat Completions uses by default
- GPT-4o uses by default
- All existing tool schemas were designed for

---

## Part 10: Decision Framework

### When to Use Each Strict Mode Setting

```
strict: true (explicit)
  ├── Use when: You need GUARANTEED schema compliance in model output
  ├── Requires: Fully compliant schemas (all props required, nullable optionals, additionalProperties)
  ├── Benefits: Zero schema-violating outputs; predictable parsing
  ├── Risks: API rejects non-compliant schemas; higher first-request latency
  └── Best for: Production APIs where output parsing must not fail

strict: false (explicit)
  ├── Use when: Schemas have optional params not converted to nullable
  ├── Requires: Nothing special (accepts any schema)
  ├── Benefits: Maximum flexibility; model fills what it can
  ├── Risks: Model may omit required fields or add unexpected fields
  └── Best for: Development, flexible tool interfaces, legacy schema compatibility

strict: omitted
  ├── On Chat Completions: Equivalent to strict: false (safe)
  ├── On Responses API: Equivalent to strict: true with auto-normalization (DANGEROUS)
  ├── NEVER rely on omitted default — always set explicitly
  └── This is the source of the GPT-5.5 migration trap
```

### The Golden Rule

> **Always set `strict` explicitly. Never rely on API-default behavior.**
> 
> - If your schemas are compliant: set `strict: true` for guaranteed output
> - If your schemas have optional params without nullable types: set `strict: false`
> - If migrating to Responses API: audit every tool schema before switching

---

## Part 11: Summary of Key Behavioral Differences

### GPT-4o vs GPT-5.5 — Complete Comparison

| Aspect | GPT-4o | GPT-5.5 |
|--------|--------|---------|
| **Primary API** | Chat Completions | Responses API |
| **Default strict behavior** | Non-strict (best-effort) | Auto-normalized to strict |
| **Schema tolerance** | High (works with violations) | Low (fails on violations) |
| **Instruction following** | Infers intent | Literal and thorough |
| **Output on unsatisfiable constraints** | Produces something anyway | Produces nothing (0 tokens) |
| **Reasoning tokens** | None (not a reasoning model) | Used for tool selection/planning |
| **Tool count sensitivity** | Moderate | High (>20 causes documented issues) |
| **Empty output frequency** | Extremely rare | Deterministic with violated schemas |
| **Constrained decoding** | Only with explicit strict:true | Default for Responses API |
| **Recommended strict setting** | `true` (if schemas compliant) or omit | `false` (if schemas non-compliant) or `true` (if schemas compliant) |
| **Migration path** | N/A | MUST audit schemas before switching |

### The Three-Variable Interaction

```
                    ┌──────────────────────────────────────────────┐
                    │          STRICT MODE BEHAVIOR MATRIX          │
                    ├──────────────────────────────────────────────┤
                    │                                              │
                    │  Model Compliance ──┐                        │
                    │       ↓             │                        │
                    │  ┌─────────┐   ┌────▼────┐   ┌──────────┐  │
                    │  │  API    │   │  Schema  │   │  Model   │  │
                    │  │ Surface │   │  Quality │   │ Behavior │  │
                    │  └────┬────┘   └────┬─────┘   └────┬─────┘  │
                    │       │             │              │         │
                    │       ▼             ▼              ▼         │
                    │  ┌────────────────────────────────────────┐  │
                    │  │           OUTCOME MATRIX               │  │
                    │  │                                        │  │
                    │  │ Responses + Bad Schema + GPT-5.5       │  │
                    │  │   = SILENT FAILURE (empty output)      │  │
                    │  │                                        │  │
                    │  │ Chat Comp + Bad Schema + GPT-4o        │  │
                    │  │   = WORKS (best-effort)                │  │
                    │  │                                        │  │
                    │  │ Any API + Good Schema + Any Model      │  │
                    │  │   = WORKS (constrained or best-effort) │  │
                    │  │                                        │  │
                    │  │ Any API + strict:false + Any Model     │  │
                    │  │   = WORKS (best-effort)                │  │
                    │  └────────────────────────────────────────┘  │
                    └──────────────────────────────────────────────┘
```

---

## References

1. **OpenAI Function Calling Guide** — Strict Mode section: https://platform.openai.com/docs/guides/function-calling#strict-mode
2. **OpenAI Structured Outputs Guide** — Supported Schemas: https://platform.openai.com/docs/guides/structured-outputs#supported-schemas
3. **OpenAI GPT-5.5 Guide**: https://platform.openai.com/docs/guides/latest-model
4. **OpenAI Responses API Reference**: https://platform.openai.com/docs/api-reference/responses/create
5. **aichat source code**: `src/client/bedrock.rs` (Responses API implementation), `src/function.rs` (JsonSchema struct)
6. **Related findings**: Tasks 01, 03, 06, 08, 09, 11, 14, 17 in this investigation

---

## Appendix A: Quick Reference Card

```
┌─────────────────────────────────────────────────────────────┐
│              STRICT MODE QUICK REFERENCE                     │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  SAFE COMBINATIONS:                                         │
│    ✅ Any model + strict:false + any schema                 │
│    ✅ Any model + strict:true + compliant schema            │
│    ✅ GPT-4o + Chat Completions + any schema (non-strict)   │
│                                                             │
│  DANGEROUS COMBINATION:                                     │
│    ❌ GPT-5.5 + Responses API + omitted strict +            │
│       non-compliant schema = SILENT EMPTY OUTPUT            │
│                                                             │
│  COMPLIANT SCHEMA CHECKLIST:                                │
│    □ additionalProperties: false on ALL objects             │
│    □ ALL properties listed in required array                │
│    □ Optional params use ["type", "null"] (nullable)        │
│    □ No unsupported JSON Schema features                    │
│    □ ≤ 5000 total properties, ≤ 10 nesting levels          │
│                                                             │
│  MIGRATION RULES:                                           │
│    1. NEVER omit strict on Responses API                    │
│    2. Set strict:false if schemas aren't compliant          │
│    3. Audit ALL tools before switching models/APIs          │
│    4. Handle empty output gracefully (defense-in-depth)     │
│    5. Keep tool count ≤ 20 for reliability                  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

## Appendix B: Diagnostic Flowchart

```
Tool calling returns empty output?
│
├── Check: Is API = Responses API?
│   ├── YES → Is `strict` field set on tools?
│   │   ├── Omitted → AUTO-NORMALIZATION is occurring
│   │   │   └── FIX: Add "strict": false to all tools
│   │   ├── true → Are schemas fully compliant?
│   │   │   ├── NO → FIX: Make schemas compliant OR set strict:false
│   │   │   └── YES → Check tool count, check prompt complexity
│   │   └── false → Strict mode ruled out; check other causes
│   └── NO (Chat Completions) → Strict mode unlikely the cause
│
├── Check: Model = GPT-5.5 (or other reasoning model)?
│   ├── YES → Model uses literal compliance; schema violations = failure
│   └── NO → Model likely tolerant; check other error causes
│
└── Check: response.output_tokens == 0 AND status == "completed"?
    ├── YES → Strong indicator of unsatisfiable schema constraints
    └── NO → Different failure mode (content filter, max tokens, etc.)
```
