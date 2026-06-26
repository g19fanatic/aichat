# OpenAI Strict Mode Requirements for Function/Tool Schemas

## Source
- Primary: https://platform.openai.com/docs/guides/function-calling (Strict mode section)
- Secondary: https://platform.openai.com/docs/guides/structured-outputs (Supported schemas section)
- Date retrieved: 2025-07-18

## Core Requirements When `strict: true`

When `strict` is set to `true` on a function/tool definition, the following constraints are **mandatory**:

### 1. `additionalProperties` must be `false`
Every `object` type in the `parameters` schema must include `"additionalProperties": false`. This applies to:
- The root parameters object
- Any nested objects within properties
- Objects inside array `items`

### 2. ALL properties must be listed in `required`
Every field defined in `properties` **must** appear in the `required` array. There are no truly optional fields in strict mode.

### 3. Optional parameters use nullable types
To represent a parameter that is conceptually "optional", you use a union type with `null`:

```json
{
  "type": ["string", "null"],
  "description": "The unit to return the temperature in",
  "enum": ["celsius", "fahrenheit"]
}
```

The field is still listed in `required`, but the model is allowed to pass `null` as the value.

## Complete Valid Strict Schema Example

```json
{
  "type": "function",
  "name": "get_weather",
  "description": "Retrieves current weather for the given location.",
  "strict": true,
  "parameters": {
    "type": "object",
    "properties": {
      "location": {
        "type": "string",
        "description": "City and country e.g. Bogotá, Colombia"
      },
      "units": {
        "type": ["string", "null"],
        "enum": ["celsius", "fahrenheit"],
        "description": "Units the temperature will be returned in."
      }
    },
    "required": ["location", "units"],
    "additionalProperties": false
  }
}
```

## What Happens When Schema Violates Strict Mode Rules

### If `strict: true` is explicitly set AND schema doesn't meet requirements:
> "If you send `strict: true` and your schema does not meet the requirements above, **the request will be rejected** with details about the missing constraints."

### If `strict` is omitted (Responses API default behavior):
> "If you omit `strict`, the default depends on the API: **Responses requests will normalize your schema into strict mode** (for example, by setting `additionalProperties: false` and marking all fields as required), which can **make previously optional fields mandatory**, while Chat Completions requests remain non-strict by default. To opt out of strict mode in Responses and keep non-strict, best-effort function calling, explicitly set `strict: false`."

**This is critical**: The Responses API auto-normalizes schemas to strict mode when `strict` is not explicitly set. This auto-normalization:
- Sets `additionalProperties: false`
- Marks ALL fields as `required`
- Can make previously optional fields mandatory (without converting them to nullable types)

## Responses API vs Chat Completions API

| Behavior | Responses API | Chat Completions API |
|----------|--------------|---------------------|
| Default `strict` | Auto-normalizes to strict | Non-strict (best-effort) |
| Schema normalization | Yes (adds additionalProperties, marks all required) | No |
| `strict: true` enforcement | Rejects invalid schemas | Rejects invalid schemas |
| `strict: false` | Opt-out of normalization | Same as default |

## Schema Limitations (Supported Schemas)

### Supported Types
- String, Number, Boolean, Integer, Object, Array, Enum, anyOf

### NOT Supported
- `allOf`, `not`, `dependentRequired`, `dependentSchemas`, `if`, `then`, `else`

### Size Limits
- Up to **5000 object properties** total across all schemas
- Up to **10 levels of nesting**
- Total string length of all property names, definition names, enum values, and const values cannot exceed **120,000 characters**
- Up to **1000 enum values** across all enum properties
- For a single enum with string values: total string length cannot exceed **15,000 characters** when >250 values

### Supported String Properties
- `pattern` (regex)
- `format`: date-time, time, date, duration, email, hostname, ipv4, ipv6, uuid

### Supported Number Properties
- `multipleOf`, `maximum`, `exclusiveMaximum`, `minimum`, `exclusiveMinimum`

### Supported Array Properties
- `minItems`, `maxItems`

### Key Ordering
Outputs are produced in the same order as the ordering of keys in the schema.

## Tool-Specific Notes

### Tool Count Recommendations
> "Aim for fewer than 20 functions available at the start of a turn at any one time, though this is just a soft suggestion."

For larger tool counts, OpenAI recommends using `tool_search` (only gpt-5.4+ supports this).

### Parallel Tool Calls and Strict Mode
> "Currently, if you are using a fine tuned model and the model calls multiple functions in one turn then strict mode will be disabled for those calls."

### First-Request Latency
> "Schemas undergo additional processing on the first request (and are then cached). If your schemas vary from request to request, this may result in higher latencies."

## Relevance to the aichat GPT-5.5 Bug

### Key Observations:
1. **Schema validation should reject invalid strict schemas** — But our error shows `status: "completed"` with empty `output: []`, suggesting the schema was accepted but the model produced no output.

2. **Possible explanation**: The Responses API may have **auto-normalized** the schema (adding missing required fields, setting additionalProperties) even with `strict: true` explicitly set. After normalization, the model received a schema where previously optional params became mandatory without nullable types, confusing the tool-calling logic.

3. **22 tools is above the soft limit of 20** — While not a hard limit, this increases the likelihood of the model failing to produce output.

4. **The "1 simple tool call works fine" observation** supports the hypothesis that schema complexity (many tools with violations) causes the failure, not a fundamental API incompatibility.

### Recommended Investigation
- Check if aichat includes `additionalProperties: false` in its generated schemas
- Check if aichat lists ALL properties in `required` arrays
- Check if optional parameters use nullable types (`["type", "null"]`)
- Consider whether 22 tools with complex schemas exceeds practical limits
