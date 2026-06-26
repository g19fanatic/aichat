# Tool Schema Strict Mode Violations Analysis

## Summary

| Metric | Value |
|--------|-------|
| Total tools in request | 22 |
| Tools with `strict: true` | 22 (ALL) |
| Tools violating strict mode | **12** (54.5%) |
| Tools compliant | 10 (45.5%) |
| Total property-level violations | **48** |
| `additionalProperties: false` present | **0 / 22** (NONE) |

## Strict Mode Rules (OpenAI Requirements)

When `"strict": true` is set on a tool schema:
1. **ALL** properties defined in `properties` MUST be listed in the `required` array
2. `additionalProperties: false` MUST be set on the object schema
3. Optional parameters should use nullable types (`{"type": ["string", "null"]}`) instead of being omitted from `required`

## Violation Table

| # | Tool Name | Total Props | Required | Missing from Required | Violation Count | Severity |
|---|-----------|------------|----------|----------------------|-----------------|----------|
| 1 | `fs_git_diff` | 2 | 0 | `path`, `cached` | 2 | 🔴 HIGH (100% missing) |
| 2 | `fs_info` | 2 | 1 | `max_preview_lines` | 1 | 🟡 MEDIUM |
| 3 | `fs_read` | 3 | 1 | `size`, `page` | 2 | 🟡 MEDIUM |
| 4 | `fs_write` | 2 | 1 | `contents` | 1 | 🟡 MEDIUM |
| 5 | `gui_session` | 14 | 1 | `session_id`, `screen`, `app_cmd`, `commands`, `prompt`, `model`, `output_path`, `include_base64`, `include_screenshot`, `resize_width`, `delay_ms`, `context`, `debug` | 13 | 🔴 HIGH (93% missing) |
| 6 | `patch` | 11 | 10 | `encoding` | 1 | 🟢 LOW |
| 7 | `ralph_loop` | 10 | 0 | `create_workspace`, `steps`, `max_parallel`, `ralph_dir`, `project_dir`, `max_iterations`, `resume`, `findings_slug`, `max_consecutive_errors`, `worker_timeout` | 10 | 🔴 HIGH (100% missing) |
| 8 | `recursive_grep` | 9 | 2 | `file_pattern`, `ignore_case`, `whole_word`, `line_number`, `count`, `exclude_hidden`, `max_depth` | 7 | 🔴 HIGH (78% missing) |
| 9 | `skills` | 6 | 5 | `skill_dir` | 1 | 🟢 LOW |
| 10 | `subagent` | 8 | 5 | `model`, `parent_task_id`, `original_cwd` | 3 | 🟡 MEDIUM |
| 11 | `whitelist_command` | 3 | 1 | `command`, `list_options` | 2 | 🟡 MEDIUM |
| 12 | `sequentialthinking_sequentialthinking` | 9 | 4 | `isRevision`, `revisesThought`, `branchFromThought`, `branchId`, `needsMoreThoughts` | 5 | 🔴 HIGH (56% missing) |

## Compliant Tools (No Violations in required Array)

| # | Tool Name | Properties | Required | Status |
|---|-----------|-----------|----------|--------|
| 1 | `code_navigator` | 4 | 4 | ✅ All props in required |
| 2 | `ddg_fetch` | 1 | 1 | ✅ All props in required |
| 3 | `fetch_url_via_curl` | 1 | 1 | ✅ All props in required |
| 4 | `fs_cat` | 1 | 1 | ✅ All props in required |
| 5 | `fs_ls` | 1 | 1 | ✅ All props in required |
| 6 | `fs_mkdir` | 1 | 1 | ✅ All props in required |
| 7 | `fs_rm` | 1 | 1 | ✅ All props in required |
| 8 | `get_current_time` | 1 | 1 | ✅ All props in required |
| 9 | `safe_script_executor` | 5 | 5 | ✅ All props in required |
| 10 | `search_wikipedia` | 1 | 1 | ✅ All props in required |

**Note**: Even compliant tools are missing `additionalProperties: false`, which is also a strict mode requirement.

## Critical Observation: `additionalProperties` Missing Everywhere

**NONE** of the 22 tool schemas include `"additionalProperties": false` in their parameter objects. This is a UNIVERSAL strict mode violation across ALL tools, even the ones that correctly list all properties in `required`.

## Worst Offenders (Sorted by Violation Count)

1. **`gui_session`** — 13 properties missing from required (out of 14 total)
2. **`ralph_loop`** — 10 properties missing from required (out of 10 total, required=[] is EMPTY)
3. **`recursive_grep`** — 7 properties missing from required (out of 9 total)
4. **`sequentialthinking_sequentialthinking`** — 5 properties missing (out of 9 total)
5. **`subagent`** — 3 properties missing (out of 8 total)

## Impact Analysis

### Why This Causes Empty Output

When GPT-5.5 receives tool schemas with `strict: true` but the schemas are **invalid** (properties not in required, missing additionalProperties), the model faces a conflict:
- It's told to use strict schema validation
- But the schemas themselves don't pass strict validation rules
- The model cannot generate a valid tool call that satisfies the (contradictory) constraints
- Result: **empty output** (`"output": []`) with status `"completed"` and 0 output tokens

### Why "1 Simple Tool Call Works Fine"

The user reports that a single tool call works. This is likely because:
- With fewer tools, the model can reason about schema constraints more easily
- With 22 tools (12 of which are invalid for strict mode), the model's schema validator may reject ALL tool options
- The model may also hit internal token/complexity limits when processing 22 invalid schemas simultaneously

## Detailed Violation Breakdown by Tool

### `fs_git_diff` (CRITICAL - 100% violation)
```json
{
  "required": [],
  "properties": { "path": {...}, "cached": {...} }
}
```
**Issue**: Empty required array means NO properties are required, but strict mode demands ALL properties be in required.

### `gui_session` (CRITICAL - 93% violation)
```json
{
  "required": ["action"],
  "properties": { "action": {...}, "session_id": {...}, "screen": {...}, ... 11 more }
}
```
**Issue**: Only `action` is required, but 13 additional properties are defined as optional — all should be in required with nullable types.

### `ralph_loop` (CRITICAL - 100% violation)
```json
{
  "required": [],
  "properties": { "create_workspace": {...}, "steps": {...}, ... 8 more }
}
```
**Issue**: Empty required array with 10 defined properties. This is the most egregious violation.

### `recursive_grep` (HIGH - 78% violation)
```json
{
  "required": ["pattern", "directory"],
  "properties": { "pattern": {...}, "directory": {...}, "file_pattern": {...}, ... 5 more }
}
```
**Issue**: 7 out of 9 properties are optional (not in required).

### `sequentialthinking_sequentialthinking` (HIGH - 56% violation)
```json
{
  "required": ["thought", "nextThoughtNeeded", "thoughtNumber", "totalThoughts"],
  "properties": { ..., "isRevision": {...}, "revisesThought": {...}, ... 3 more }
}
```
**Issue**: 5 properties for branching/revision are optional.

## Fix Required

To resolve the strict mode violations, ALL tool schemas must be updated to:
1. List ALL properties in the `required` array
2. Add `"additionalProperties": false` to each schema object
3. Make previously-optional properties nullable: change `"type": "string"` to `"type": ["string", "null"]` (or use oneOf/anyOf with null)

This fix needs to happen in the aichat source code where tool schemas are generated and serialized for the OpenAI Responses API.
