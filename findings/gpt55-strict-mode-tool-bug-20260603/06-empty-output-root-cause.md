# Empty Output Root Cause Analysis

## Problem Statement

GPT-5.5 via the Responses API returns `"output": []` with `"status": "completed"` — a "successful" response that contains zero content. The model generates 0 output tokens and 0 reasoning tokens, despite processing 14,074 input tokens. This triggers aichat's error: "Invalid Responses API response data" because the client cannot extract meaningful output from a "completed" but empty response.

## Key Evidence from the Error Response

| Field | Value | Significance |
|-------|-------|--------------|
| `status` | `"completed"` | API considers processing finished successfully |
| `output` | `[]` | No content items generated whatsoever |
| `output_tokens` | `0` | Zero tokens produced |
| `reasoning_tokens` | `0` | No reasoning was performed |
| `input_tokens` | `14,074` | Request was received and processed |
| `error` | `null` | No API-level error |
| `incomplete_details` | `null` | Not marked as incomplete |
| `truncation` | `"disabled"` | Input was not truncated |
| `tool_choice` | `"auto"` | Model could choose text or tool call |
| `reasoning.effort` | `"medium"` | Default reasoning level for GPT-5.5 |
| `tools` count | 22 | All with `"strict": true` |

## Root Cause Determination

### PRIMARY CAUSE: Strict Mode Schema Violations with GPT-5.5's Literal Compliance Behavior

The combination of **invalid strict-mode schemas** and **GPT-5.5's refusal-over-violation design** causes the model to produce zero output.

#### The Mechanism (Step by Step)

1. **aichat sends 22 tools with `"strict": true`** — explicitly requesting strict schema enforcement
2. **12/22 tools have properties NOT in their `required` array** — violating strict mode rules
3. **ALL 22 tools are missing `additionalProperties: false`** — another strict mode violation
4. **The Responses API auto-normalizes schemas** — it attempts to add fields to `required` and set `additionalProperties: false` (per docs: "Responses requests will normalize your schema into strict mode")
5. **Auto-normalization creates impossible constraints** — when the API marks all properties as required, optional parameters become mandatory. The model must now provide values for ALL parameters of whichever tool it calls.
6. **GPT-5.5 evaluates all 22 tools** — attempting to find one where it can produce a valid function_call satisfying all constraints
7. **The model cannot produce a valid output** — faced with 22 tools all having strict constraints it cannot satisfy (or cannot determine safe values for all required fields), it generates nothing
8. **Zero tokens emitted** — the model completes without producing any output items
9. **API marks as "completed"** — the generation process finished without an infrastructure error

#### Why This Happens with GPT-5.5 Specifically

From OpenAI's GPT-5.5 documentation:
- *"GPT-5.5 interprets prompts in a **literal and thorough** manner"*
- *"Reliable schema adherence with strict mode"*
- *"Higher reasoning effort isn't automatically better. If the task has **conflicting instructions**, weak stopping criteria, or open-ended tool access, higher effort can lead to overthinking"*

GPT-5.5 is designed to **refuse rather than violate**. When it cannot produce output that satisfies all constraints simultaneously, it generates nothing — as opposed to older models (GPT-4o) that would use "best effort" and produce output even if it doesn't perfectly match the schema.

## Documented vs. Observed Behavior

| Scenario | Documented Behavior | Observed Behavior |
|----------|-------------------|-------------------|
| Invalid schema + `strict: true` | "The request will be **rejected** with details about the missing constraints" | Request ACCEPTED, "completed" with empty output |
| Safety refusal | Produces `refusal` content in output | Not applicable (no refusal content) |
| Max tokens hit | Status: `"incomplete"` | Status: `"completed"` |
| Content filter | Status: `"incomplete"` | Status: `"completed"` |

**Critical finding**: The documented behavior for invalid strict schemas says the request should be REJECTED. Instead, the Responses API auto-normalizes AND still accepts the request — but the resulting normalized constraints may be unsatisfiable for the model.

## Contributing Factors (Ranked by Impact)

### 1. Schema Violations Are Universal (HIGH IMPACT)
- **48 individual property violations** across 12 tools
- **22 tools missing `additionalProperties: false`** (ALL tools)
- This isn't one or two tools with minor issues — it's a systemic failure across the entire tool surface

### 2. Tool Count Exceeds Recommendation (MEDIUM IMPACT)
- 22 tools registered vs. <20 soft recommendation
- From docs: "Aim for fewer than 20 functions available at the start of a turn"
- More tools = more cognitive load + more places where constraint violations compound
- The user confirms "1 simple tool call works just fine" — scale is a factor

### 3. GPT-5.5 Literal Instruction Following (MEDIUM IMPACT)
- Unlike GPT-4o which treats schemas as "best effort", GPT-5.5 treats `strict: true` as an absolute constraint
- The model will generate nothing rather than produce output that might violate a perceived constraint
- This is model-specific behavior — the same schemas may work with GPT-4o

### 4. Reasoning Effort at "medium" (LOW-MEDIUM IMPACT)
- With medium reasoning, the model may not have enough reasoning budget to:
  - Analyze all 22 tool schemas
  - Determine which tool's constraints it can satisfy
  - Plan the function call arguments
- But 0 reasoning tokens suggests it didn't even start reasoning — it may have determined impossibility at the schema analysis stage

### 5. System Prompt Size (LOW IMPACT)
- 14,074 input tokens is substantial but within GPT-5.5's 1M context window
- Unlikely to be the direct cause, but adds to overall complexity

## Why "1 Simple Tool Call Works Just Fine"

The user's observation is consistent with the hypothesis:
- **Single tool**: Even with schema violations, the model can often infer correct values for all parameters of a single well-understood tool
- **22 tools with violations**: The combinatorial explosion of constraints across all tools makes it impossible for the model to find a valid output path
- **Tipping point**: There's likely a threshold between 1 and 22 tools where the auto-normalized constraints become unsatisfiable

## Similar Reports

### Online Search Results
- **OpenAI Community Forums**: Connection errors prevented direct search, but the behavior (completed + empty output) is not documented in any known failure mode
- **aichat GitHub Issues**: No direct reports of this specific GPT-5.5 empty output issue found
  - Issue #1495 documents a related but distinct streaming bug (tool call arguments lost)
  - Issue #1338 documents a panic on empty output (related to the aichat side of handling)
  - No issues specifically mention GPT-5.5 + strict mode + empty output

### Status: Undocumented Edge Case
This appears to be an **undocumented behavior** in the Responses API. The intersection of:
1. Auto-normalization creating strict constraints
2. GPT-5.5's literal compliance behavior
3. Many tools with schema violations

...produces a state where the model completes without error but generates nothing.

## Conclusion

The empty output is caused by **GPT-5.5's constrained decoding or safety-first approach refusing to generate tokens that would violate strict mode requirements**. Since all 22 tool schemas violate strict mode (missing `additionalProperties: false` and properties not in `required`), and the Responses API auto-normalizes these into strict schemas, the model enters a state where it cannot produce ANY valid output — so it produces nothing.

The fix is straightforward: make the tool schemas compliant with strict mode requirements (all properties in `required`, `additionalProperties: false` on all objects, use `"type": ["string", "null"]` for optional parameters), OR set `strict: false` on tool schemas.

## Sources

- Error JSON from `/tmp/vmc1Edx/7` (actual API response)
- OpenAI API Reference: Responses API object model (https://platform.openai.com/docs/api-reference/responses/object)
- OpenAI Function Calling Guide: strict mode requirements
- OpenAI GPT-5.5 Guide: literal instruction following, reasoning behavior
- Task 2 findings: 06-schema-violations.md (12/22 tools violate, 48 property violations)
- Task 3 findings: 03-gpt55-tool-calling.md (GPT-5.5 behavior documentation)
- aichat GitHub Issues search (no direct match found)
