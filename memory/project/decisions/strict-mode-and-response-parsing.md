---
summary: "Two fixes for tool-calling: (1) explicit strict:false on Responses API tool schemas, (2) response_to_json() helper replacing all res.json().await? calls"
created: 2026-06-03
updated: 2026-06-05
type: decision
scope: project
project: aichat
importance: critical
confidence: verified
tags: [tool-calling, openai, responses-api, error-handling, strict-mode]
sources:
  - type: file
    path: src/client/bedrock.rs
    note: "Lines 920 (strict:false) and 982 (graceful empty output)"
  - type: file
    path: src/client/common.rs
    note: "Line 496+ (response_to_json helper)"
see_also:
  - memory/project/problems/bedrock-profile-silently-ignored.md
---

# Decision: Strict Mode Handling + Response JSON Parsing

## Context

Two separate users hit critical errors using different model providers through aichat:
1. GPT-5.5 via Responses API → empty output array due to strict schema violations
2. narsil-admin opus 4.6 (openai_compatible) → non-JSON response body

## Decisions Made

### 1. Set `strict: false` on all Responses API tool schemas (bedrock.rs:920)

**Rationale**: The OpenAI Responses API auto-normalizes schemas to strict mode when the flag is omitted. This forces ALL properties into `required` and adds `additionalProperties: false`. For tools with optional params (most of ours), this makes the schema unsatisfiable for GPT-5.5 which refuses to generate output rather than violate constraints.

**Alternative considered**: Making all schemas strict-compliant (all props in required, nullable types for optional). Rejected because: too many tools, constant maintenance burden, and the current tool schemas are designed with optional params that should remain optional.

### 2. Created `response_to_json()` helper replacing all `res.json().await?` calls (common.rs:496)

**Rationale**: The bare reqwest `.json().await?` produces uninformative errors ("expected value at line 1 column 1") when the response body is empty or non-JSON. The helper reads body as text first, then provides errors including HTTP status code and body preview (first 200 chars).

**Impact**: All 14 `res.json().await?` call sites across 7 client files replaced:
- openai.rs (2 sites)
- bedrock.rs (4 sites)
- cohere.rs (2 sites)
- vertexai.rs (3 sites)
- gemini.rs (1 site)
- claude.rs (1 site)
- openai_compatible.rs (1 site)

### 3. Graceful handling of empty Responses API output (bedrock.rs:982)

**Rationale**: Defense-in-depth. Even with strict:false, if a model ever returns empty output, the code should not crash with `bail!()`. Instead, return an empty `ChatCompletionsOutput` and let upstream handle it.

## Test Coverage

- 9 unit tests added for response_to_json (valid JSON, empty body, HTML body, binary body, status code inclusion)
- 44 total tests passing after changes
