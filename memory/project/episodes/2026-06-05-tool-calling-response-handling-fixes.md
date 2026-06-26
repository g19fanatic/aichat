---
summary: "Fixed two critical aichat tool-calling bugs: (1) GPT-5.5 strict mode empty output, (2) non-JSON response body parsing across all clients"
created: 2026-06-03
updated: 2026-06-05
type: episode
scope: project
project: aichat
session_start: "2026-06-03T16:14:00-04:00"
session_end: "2026-06-05T08:30:00-04:00"
outcome: accomplished
accomplishments:
  - "Fixed GPT-5.5 strict mode bug: added strict:false to Responses API tool schemas in bedrock.rs:920"
  - "Fixed empty output crash: replaced bail!() with graceful empty ChatCompletionsOutput in bedrock.rs:982"
  - "Fixed non-JSON response handling: created response_to_json() helper in common.rs:496"
  - "Replaced all 14 res.json().await? call sites across 7 client modules"
  - "Added 9 unit tests for response_to_json covering empty body, HTML, binary, status codes"
  - "All tests pass (44 total), build clean"
  - "Completed 30/30 ralph loop research+implementation tasks across both bugs"
failures: []
discoveries:
  - "OpenAI Responses API auto-normalizes schemas to strict mode when strict flag is omitted (unlike Chat Completions)"
  - "GPT-5.5 refuses to generate output (returns empty output:[]) rather than violate strict schema constraints"
  - "The error path for non-JSON responses is through reqwest's .json() which wraps serde_json errors unhelpfully"
  - "bedrock.rs handles both Bedrock AND OpenAI Responses API paths (shared code)"
  - "14 res.json().await? calls existed across openai.rs, bedrock.rs, cohere.rs, vertexai.rs, gemini.rs, claude.rs, openai_compatible.rs"
unresolved: []
tasks_touched:
  - "GPT-5.5 strict mode tool schema fix"
  - "Non-JSON response error handling improvement"
files_modified:
  - "src/client/bedrock.rs:920,982"
  - "src/client/common.rs:496+"
  - "src/client/openai.rs:91,206"
  - "src/client/cohere.rs:114,194"
  - "src/client/vertexai.rs:189,205,242"
  - "src/client/gemini.rs:114"
  - "src/client/claude.rs:73"
  - "src/client/openai_compatible.rs:123"
next_session_needs: "Commit and deploy fixes. Consider upstreaming to sigoden/aichat. May also want to add proper strict-mode-compliant schema generation (making all properties required + nullable) as an alternative to strict:false."
importance: critical
tags: [bug-fix, tool-calling, openai, responses-api, error-handling]
---

# Episode: aichat Tool Calling Response Handling Fixes

## What Happened

Two users hit critical errors when using aichat with different model providers:

### Bug 1: GPT-5.5 Empty Output (Jun 3)
- **Symptom**: "Invalid Responses API response data" with `"output":[]`
- **Investigation**: Launched 20-step ralph research loop. Discovered OpenAI Responses API auto-normalizes tool schemas to strict mode, forcing all properties to be required+non-nullable. GPT-5.5's literal compliance means it generates zero tokens rather than violate constraints.
- **Fix**: Added `"strict": false` to tool schema JSON in bedrock.rs:920, plus graceful empty output handling at bedrock.rs:982.

### Bug 2: Non-JSON Response Body (Jun 5)  
- **Symptom**: "error decoding response body: expected value at line 1 column 1" from narsil-admin opus 4.6
- **Investigation**: Launched 10-step ralph implementation loop. Found 14 vulnerable `res.json().await?` call sites that provide zero diagnostic info when endpoint returns empty/HTML/non-JSON.
- **Fix**: Created `response_to_json()` helper in common.rs that reads body as text first, provides informative errors with status code + body preview. Replaced all 14 call sites. Added 9 unit tests.

## Key Insight

The two bugs are related but at different layers:
- Bug 1: API response IS valid JSON but has empty content → aichat crashes on unexpected empty structure
- Bug 2: Response body is NOT valid JSON at all → reqwest/serde crash before aichat even sees it

Both represent insufficient error handling at the HTTP response deserialization boundary.

## Methodology

Used ralph_loop (agent-loop skill) extensively:
- Bug 1: 20-step research loop → 18 findings documents + knowledge graph
- Bug 2: 10-step implementation loop → helper function + tests + all replacements
- Multiple resume cycles needed due to worker timeouts (300s limit per iteration)
