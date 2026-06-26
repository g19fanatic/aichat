# 08: OpenAI Structured Outputs Requirements

## Summary

This document details the exact schema rules enforced by OpenAI's Structured Outputs feature, which applies identically to both `response_format` (JSON output) and function calling with `strict: true`. These rules are the root cause of the aichat GPT-5.5 empty output bug.

**Source**: https://platform.openai.com/docs/guides/structured-outputs and https://platform.openai.com/docs/guides/function-calling#strict-mode

---

## The Three Cardinal Rules

When `strict: true` is active (explicitly or via auto-normalization), ALL of these must hold:

### Rule 1: `additionalProperties: false` on EVERY object

Every object in the schema — root level, nested objects, items within arrays — must include:
```json
"additionalProperties": false
```

**OpenAI docs**: "additionalProperties controls whether it is allowable for an object to contain additional keys / values that were not defined in the JSON Schema. Structured Outputs only supports generating specified keys / values, so we require developers to set additionalProperties: false to opt into Structured Outputs."

### Rule 2: ALL properties must be in `required` array

Every property defined in `properties` must appear in the `required` array. No exceptions.

**OpenAI docs**: "To use Structured Outputs, all fields or function parameters must be specified as required."

### Rule 3: Optional parameters use nullable type union

To emulate optional parameters, use `"type": ["<base_type>", "null"]`:
```json
{
  "unit": {
    "type": ["string", "null"],
    "description": "Optional: the unit to return the temperature in",
    "enum": ["F", "C"]
  }
}
```

The field is still in `required`, but the model can output `null` for it.

---

## Complete Compliant Schema Example (Function Calling)

```json
{
  "type": "function",
  "name": "get_weather",
  "description": "Fetches the weather in the given location",
  "strict": true,
  "parameters": {
    "type": "object",
    "properties": {
      "location": {
        "type": "string",
        "description": "The location to get the weather for"
      },
      "unit": {
        "type": ["string", "null"],
        "description": "The unit to return the temperature in",
        "enum": ["F", "C"]
      }
    },
    "additionalProperties": false,
    "required": ["location", "unit"]
  }
}
```

## Non-Compliant Schema Example (What aichat sends)

```json
{
  "type": "function",
  "name": "fs_read",
  "parameters": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "The path of the file to read"
      },
      "page": {
        "type": "string",
        "description": "Page number to read (default: 1)"
      },
      "size": {
        "type": "string",
        "description": "Number of lines per page (default: 10)"
      }
    },
    "required": ["path"]
  }
}
```

**Violations:**
1. ❌ Missing `additionalProperties: false`
2. ❌ `page` and `size` not in `required` array
3. ❌ No nullable type union for optional `page`/`size`
4. ❌ No `strict` field set (triggers auto-normalization in Responses API)

---

## Auto-Normalization Behavior (CRITICAL)

### Responses API Default (What GPT-5.5 uses)

From OpenAI documentation:

> "If you omit `strict`, the default depends on the API: **Responses requests will normalize your schema into strict mode** (for example, by setting `additionalProperties: false` and marking all fields as required), which can make previously optional fields mandatory, while Chat Completions requests remain non-strict by default. To opt out of strict mode in Responses and keep non-strict, best-effort function calling, explicitly set `strict: false`."

This means:
1. **Responses API** (used by GPT-5.5 via aichat): `strict` defaults to **auto-normalized strict**
2. **Chat Completions API** (used by GPT-4o via aichat): `strict` defaults to **non-strict**

### What Auto-Normalization Does

When `strict` is omitted in a Responses API request:
- Sets `additionalProperties: false` on all objects
- Marks ALL properties as `required`
- Previously optional fields become mandatory

### The Problem for aichat

aichat sends tool schemas **without** `strict` field and **without** `additionalProperties: false`. When routed through the Responses API:

1. API normalizes the schema to strict mode
2. Properties like `page`, `size`, `cached` (which have no default handling) become mandatory
3. The model is now constrained to produce ALL properties on every call
4. But the original schema didn't have nullable types for optional params
5. Model cannot satisfy the constraint → produces 0 output tokens → "completed" with empty output[]

### The Fix Options

1. **Set `strict: false` explicitly** — Opts out of strict mode, allows best-effort function calling
2. **Make schemas strict-compliant** — Add `additionalProperties: false`, put ALL props in `required`, use `"type": ["string", "null"]` for optional params

---

## Supported Schema Subset

### Supported Types
- String
- Number
- Boolean
- Integer
- Object
- Array
- Enum
- anyOf

### Supported String Properties
- `pattern` — regex constraint
- `format` — date-time, time, date, duration, email, hostname, ipv4, ipv6, uuid

### Supported Number Properties
- `multipleOf`, `maximum`, `exclusiveMaximum`, `minimum`, `exclusiveMinimum`

### Supported Array Properties
- `minItems`, `maxItems`

### NOT Supported
- `allOf`, `not`, `dependentRequired`, `dependentSchemas`, `if`, `then`, `else`
- Root level `anyOf` (root must be an object)
- Lookarounds in regex

---

## Schema Limits

| Limit | Value |
|-------|-------|
| Max object properties (total across schema) | 5,000 |
| Max nesting depth | 10 levels |
| Max total string length (names, enums, const) | 120,000 chars |
| Max enum values (across all properties) | 1,000 |
| Max string length for enum values (>250 enums) | 15,000 chars |

---

## Key Ordering Guarantee

When using Structured Outputs, the model produces output keys in the **same order** as defined in the schema. This is relevant for streaming parsers.

---

## Recursive Schemas

Supported via `$ref`:
- Root recursion: `{"$ref": "#"}`
- Named definitions: `{"$ref": "#/$defs/node_name"}`

---

## Refusals

When strict mode is active but the model refuses (for safety):
- Chat Completions: `message.refusal` field is set
- Responses API: `output[].content[].type === "refusal"` with `.refusal` text

---

## Relevance to aichat Bug

### Direct Connection

The aichat GPT-5.5 bug occurs because:

1. aichat generates tool schemas with optional properties NOT in `required`
2. aichat does NOT set `additionalProperties: false`
3. aichat does NOT set `strict: true` or `strict: false`
4. The Responses API auto-normalizes to strict mode
5. Auto-normalization makes all properties required (but without nullable types)
6. GPT-5.5 cannot satisfy the conflicting constraints
7. Model produces 0 output tokens
8. aichat's bedrock.rs parser crashes on empty output with `bail!`

### Correct Fix Pattern

For each tool with optional parameters, transform from:
```json
{
  "properties": {
    "required_param": {"type": "string"},
    "optional_param": {"type": "string"}
  },
  "required": ["required_param"]
}
```

To:
```json
{
  "properties": {
    "required_param": {"type": "string"},
    "optional_param": {"type": ["string", "null"]}
  },
  "required": ["required_param", "optional_param"],
  "additionalProperties": false
}
```

Or simply set `strict: false` on the tool definition to opt out of strict mode entirely.

---

## API Differences Summary

| Behavior | Chat Completions | Responses API |
|----------|-----------------|---------------|
| Default strict mode | Non-strict (best-effort) | Auto-normalized to strict |
| Tool schema location | `tools[].function.parameters` | `tools[].parameters` |
| Strict flag location | `tools[].function.strict` | `tools[].strict` |
| Schema validation on request | Only if `strict: true` | Always (auto-normalizes) |
| Optional params handling | Just omit from `required` | MUST use nullable + include in required |
| `additionalProperties` needed | Only if `strict: true` | Always (auto-set if missing) |

---

## References

1. OpenAI Structured Outputs Guide: https://platform.openai.com/docs/guides/structured-outputs
2. OpenAI Function Calling Guide: https://platform.openai.com/docs/guides/function-calling
3. Supported Schemas: https://platform.openai.com/docs/guides/structured-outputs#supported-schemas
4. Strict Mode section: https://platform.openai.com/docs/guides/function-calling#strict-mode
