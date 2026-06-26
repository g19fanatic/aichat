# GPT-5.5 Tool Calling Behavior Research

## Model Overview

| Attribute | Value |
|-----------|-------|
| Model ID | `gpt-5.5` |
| Context Window | 1M tokens |
| Max Output | 128K tokens |
| Reasoning Effort Default | `medium` |
| Knowledge Cutoff | December 1, 2025 |
| Input Pricing | $5 / 1M tokens |
| Output Pricing | $30 / 1M tokens |
| Supported Tools | Functions, Web search, File search, Computer use |
| API | Responses API (primary), Chat Completions (legacy) |

## Tool Calling Capabilities

### Strengths (from official documentation)

1. **"Stronger and more precise tool use"** — GPT-5.5 is "especially useful on large tool surfaces, multi-step service workflows, and long-running agent tasks"
2. **"More efficient reasoning"** — reaches strong results with fewer reasoning tokens, especially in "complex, tool-heavy, or multi-step workflows where token savings compound"
3. **"Stronger task execution with outcome-first prompts"** — better at working from a clear goal, preserving constraints
4. Supports parallel tool calls
5. Supports tool search (for deferring large tool catalogs, available since gpt-5.4)

### Maximum Tool Count

- **No hard limit documented** in the official API documentation
- **Soft recommendation**: "Aim for fewer than 20 functions available at the start of a turn at any one time, though this is just a soft suggestion"
- **The user's case**: 22 tools — exceeds the soft recommendation by 2
- **Mitigation**: For large tool catalogs, OpenAI recommends using `tool_search` to "defer large or infrequently used parts of your tool surface instead of exposing everything up front"
- **Token impact**: "functions are injected into the system message in a syntax the model has been trained on. This means callable function definitions count against the model's context limit and are billed as input tokens"

### Tool Count Impact on Performance

From the docs: "If you run into token limits, we suggest limiting the number of functions loaded up front, shortening descriptions where possible, or using tool search so deferred tools are loaded only when needed."

The documentation explicitly acknowledges that large tool counts affect:
1. Token consumption (functions count as input tokens)
2. Accuracy (more tools = harder for model to select correctly)
3. Performance ("Evaluate your performance with different numbers of functions")

## Strict Mode Requirements (Critical)

### Rules When `strict: true`

1. **`additionalProperties` must be `false`** for each object in the `parameters`
2. **ALL fields in `properties` must be marked as `required`**
3. Optional fields must use nullable types: `"type": ["string", "null"]`
4. Maximum 5000 object properties total, up to 10 levels of nesting
5. Maximum 1000 enum values across all enum properties

### Responses API Default Behavior (KEY FINDING)

> "If you omit strict, the default depends on the API: **Responses requests will normalize your schema into strict mode** (for example, by setting `additionalProperties: false` and marking all fields as required), which can make previously optional fields mandatory, while Chat Completions requests remain non-strict by default. To opt out of strict mode in Responses and keep non-strict, best-effort function calling, explicitly set `strict: false`."

This means:
- The Responses API **auto-normalizes** schemas to be strict-compliant
- If `strict` is omitted, the API itself may add fields to `required` and set `additionalProperties: false`
- If `strict: true` is explicitly set AND the schema violates requirements → **the API should reject the request with error details**

## Behavior When Schema Validation Fails

### Case 1: API-Level Rejection (Expected for Invalid Strict Schemas)

From docs: "If you send `strict: true` and your schema does not meet the requirements above, the **request will be rejected** with details about the missing constraints."

### Case 2: Auto-Normalization (Responses API Default)

If strict is omitted or auto-applied, the Responses API normalizes the schema. This may:
- Add missing fields to `required`
- Set `additionalProperties: false`
- Convert formerly optional parameters to mandatory

### Case 3: Empty Output with "completed" Status (The User's Case)

The user observes:
- Status: `"completed"` (API says success)
- Output: `[]` (EMPTY — no content generated)
- Output tokens: 0
- Reasoning tokens: 0

This is **NOT a documented normal behavior**. Possible causes from documentation:
1. **Safety refusal** → Would produce `refusal` content in output (NOT empty array)
2. **Max tokens hit** → Would produce status: `"incomplete"` (NOT "completed")
3. **Content filter** → Would produce status: `"incomplete"`
4. **Model decides not to produce output** → Returns "completed" with empty output

The documented edge cases (refusal, max tokens, content filter) do NOT match the observed behavior. An empty output array with "completed" status represents an **undocumented edge case** where the model completed processing but generated zero output items.

## Hypothesis: Why GPT-5.5 Returns Empty Output

Based on the documentation analysis:

1. **Schema normalization may create impossible constraints**: When the Responses API normalizes schemas by marking all properties as required, the model is then constrained to provide ALL fields. If the model cannot determine values for all required fields, it may produce zero output rather than risk an invalid response.

2. **GPT-5.5's literal instruction following**: The docs note GPT-5.5 "interprets prompts in a literal and thorough manner." This increased strictness means it may refuse to produce output rather than violate perceived constraints.

3. **Interaction between strict mode and 22 tools**: With the soft limit being <20 tools and strict mode requiring perfect schema adherence, the combination of:
   - 22 tools (above recommendation)
   - Properties NOT in required arrays (violates strict mode rules)
   - `strict: true` explicitly set on all tools
   
   ...may cause the model to enter a state where it cannot produce any valid function call that satisfies all constraints simultaneously.

4. **"1 simple tool call works just fine"**: This observation from the user supports the hypothesis — with a single tool, the schema constraints are manageable. With 22 tools all having strict schema violations, the model may become overwhelmed.

## GPT-5.5 Specific Behavioral Notes

From the GPT-5.5 guide:
- "GPT-5.5 is better suited to complex coding tasks that require planning, tool use, codebase navigation, verification, and multi-step execution"
- "Put most tool-specific guidance in the tool descriptions themselves: what the tool does, when to use it, required inputs, side effects, retry safety, and common error modes"
- "GPT-5.5 works best in the Responses API"
- Reasoning effort defaults to `medium` — this means the model IS thinking about which tool to use, but may be spending reasoning tokens realizing it can't produce valid output
- "Higher reasoning effort isn't automatically better. If the task has conflicting instructions, weak stopping criteria, or open-ended tool access, higher effort can lead to overthinking"

## Key Differences from GPT-4o/GPT-4.1

| Aspect | GPT-4o/4.1 | GPT-5.5 |
|--------|------------|---------|
| Strict mode default | Non-strict (best effort) | Auto-normalized to strict in Responses API |
| Schema validation | Lenient | Strict enforcement |
| Tool calling | Best-effort schema adherence | "Reliable" schema adherence with strict mode |
| Empty output on schema issues | Unlikely (best-effort) | Possible (refuses rather than violates) |
| Instruction following | Good | "Literal and thorough" |

## Recommendations for the Bug

1. **Ensure all properties are in the `required` array** when `strict: true` is set
2. **Use nullable types** for optional fields: `"type": ["string", "null"]`
3. **Set `additionalProperties: false`** on all parameter objects
4. **Consider reducing tool count** below 20, or use tool_search for deferred loading
5. **Consider setting `strict: false`** as a workaround if schema compliance is difficult

## Sources

- OpenAI Models Page: https://platform.openai.com/docs/models
- OpenAI Function Calling Guide: https://platform.openai.com/docs/guides/function-calling
- OpenAI GPT-5.5 Guide: https://platform.openai.com/docs/guides/latest-model
- OpenAI Structured Outputs Guide: https://platform.openai.com/docs/guides/structured-outputs
- OpenAI Prompt Guidance: https://platform.openai.com/docs/guides/prompt-guidance
