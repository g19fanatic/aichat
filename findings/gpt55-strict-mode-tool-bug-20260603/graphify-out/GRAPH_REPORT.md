# Graph Report - findings  (2026-06-03)

## Corpus Check
- 16 files · ~26,526 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 515 nodes · 499 edges · 30 communities
- Extraction: 100% EXTRACTED · 0% INFERRED · 0% AMBIGUOUS
- Token cost: 0 input · 0 output

## Community Hubs (Navigation)
- [[_COMMUNITY_Community 0|Community 0]]
- [[_COMMUNITY_Community 1|Community 1]]
- [[_COMMUNITY_Community 2|Community 2]]
- [[_COMMUNITY_Community 3|Community 3]]
- [[_COMMUNITY_Community 4|Community 4]]
- [[_COMMUNITY_Community 5|Community 5]]
- [[_COMMUNITY_Community 6|Community 6]]
- [[_COMMUNITY_Community 7|Community 7]]
- [[_COMMUNITY_Community 8|Community 8]]
- [[_COMMUNITY_Community 9|Community 9]]
- [[_COMMUNITY_Community 10|Community 10]]
- [[_COMMUNITY_Community 11|Community 11]]
- [[_COMMUNITY_Community 12|Community 12]]
- [[_COMMUNITY_Community 13|Community 13]]
- [[_COMMUNITY_Community 14|Community 14]]
- [[_COMMUNITY_Community 15|Community 15]]
- [[_COMMUNITY_Community 16|Community 16]]
- [[_COMMUNITY_Community 17|Community 17]]
- [[_COMMUNITY_Community 18|Community 18]]
- [[_COMMUNITY_Community 19|Community 19]]
- [[_COMMUNITY_Community 20|Community 20]]
- [[_COMMUNITY_Community 21|Community 21]]
- [[_COMMUNITY_Community 22|Community 22]]
- [[_COMMUNITY_Community 23|Community 23]]
- [[_COMMUNITY_Community 24|Community 24]]
- [[_COMMUNITY_Community 25|Community 25]]
- [[_COMMUNITY_Community 26|Community 26]]
- [[_COMMUNITY_Community 27|Community 27]]
- [[_COMMUNITY_Community 28|Community 28]]
- [[_COMMUNITY_Community 29|Community 29]]

## God Nodes (most connected - your core abstractions)
1. `18: Strict Mode Interaction Patterns Across OpenAI Models` - 16 edges
2. `08: OpenAI Structured Outputs Requirements` - 14 edges
3. `Response JSON Structure Analysis` - 13 edges
4. `11: Responses API vs Chat Completions API — Tool Schema Handling Differences` - 12 edges
5. `Root Cause Analysis: aichat GPT-5.5 "Invalid Responses API response data" Error` - 11 edges
6. `Empty Output Root Cause Analysis` - 10 edges
7. `Error Path Analysis: "Invalid Responses API response data"` - 10 edges
8. `GPT-5.5 Tool Calling Behavior Research` - 10 edges
9. `Tool Schema Strict Mode Violations Analysis` - 10 edges
10. `GitHub Issues Research: aichat + GPT-5.5 / Strict Mode / Responses API / Empty Output` - 10 edges

## Surprising Connections (you probably didn't know these)
- None detected - all connections are within the same source files.

## Communities (30 total, 0 thin omitted)

### Community 0 - "Community 0"
Cohesion: 0.05
Nodes (39): Claude (`claude.rs:291-298`), code:jq (required: [flag_options[] | select(.required == true) | .id ), code:rust (let function_declarations: Vec<_> = functions.into_iter().ma), code:json ({), code:json ({), code:json ({), code:python (if type_.endswith("?"):  # comes from Optional[T] annotation), code:javascript (if (/^\[.*\]$/.test(name)) {) (+31 more)

### Community 1 - "Community 1"
Cohesion: 0.06
Nodes (34): 08: OpenAI Structured Outputs Requirements, API Differences Summary, Auto-Normalization Behavior (CRITICAL), code:json ("additionalProperties": false), code:json ({), code:json ({), code:json ({), code:json ({) (+26 more)

### Community 2 - "Community 2"
Cohesion: 0.06
Nodes (34): 11: Responses API vs Chat Completions API — Tool Schema Handling Differences, Chat Completions API (openai.rs), Chat Completions (openai.rs:427-440) — GRACEFUL, Chat Completions Response, code:json ({), code:rust (// In bedrock.rs responses_api_chat_completions()), code:block11 (strict: boolean), code:block12 (strict: boolean (optional)) (+26 more)

### Community 3 - "Community 3"
Cohesion: 0.06
Nodes (30): 1. Entry Point: `common.rs:72-77`, 2. Dispatch: `bedrock.rs:248-255`, 3. Model Category Routing: `bedrock.rs:39-45`, 4. Request Construction: `bedrock.rs:82-95`, 5. Body Builder: `bedrock.rs:818-928` (`build_responses_api_body`), 6. Response Handler (THE ERROR): `bedrock.rs:932-992`, bedrock.rs:981 (CRASH — Responses API), code:rust (async fn chat_completions(&self, input: Input) -> Result<Cha) (+22 more)

### Community 4 - "Community 4"
Cohesion: 0.06
Nodes (31): Category (a): Source Code Fixes, Category (c): Model-Side Mitigations, code:rust (if text.is_empty() && tool_calls.is_empty() {), code:rust (if model.real_name().contains("gpt-5") {), code:block15 (For this turn, only use tools from this priority list: fs_ca), code:yaml (# For tool-heavy roles:), code:rust (// For GPT-5.5 models with tools:), code:diff (--- a/src/client/bedrock.rs) (+23 more)

### Community 5 - "Community 5"
Cohesion: 0.07
Nodes (28): 1. Model Routing Decision, 2. Responses API Request Construction, 3. Tool Schema Generation (WHERE STRICT SHOULD BE BUT ISN'T), 4. The Error Path (THE BUG), 5. Contrast: Chat Completions Handles Empty Gracefully, 6. JsonSchema Struct (No additionalProperties Support), 7. Function Declarations Loading, 8. `strict` Search Result (+20 more)

### Community 6 - "Community 6"
Cohesion: 0.07
Nodes (28): 1. `additionalProperties` must be `false`, 2. ALL properties must be listed in `required`, 3. Optional parameters use nullable types, code:json ({), code:json ({), Complete Valid Strict Schema Example, Core Requirements When `strict: true`, First-Request Latency (+20 more)

### Community 7 - "Community 7"
Cohesion: 0.07
Nodes (27): 1. `status` — Response Completion Status, 2. `error` — Error Information, 3. `incomplete_details` — Incompletion Reason, 4. `output_tokens: 0` — Token Usage Indicator, 5. `reasoning.effort: "medium"` — Reasoning Budget, Bedrock Responses API Client (`bedrock.rs:980-982`):, code:block1 (common.rs:77   → "Failed to call chat-completions api" (oute), code:rust (// 1. Iterates over data["output"] array items) (+19 more)

### Community 8 - "Community 8"
Cohesion: 0.07
Nodes (27): AFTER, Appendix: Alternative Fix (Schema Compliance) — Not Recommended as Primary, BEFORE, code:rust (if text.is_empty() && tool_calls.is_empty() {), code:block11 (Model must produce: gui_session(action="list", session_id=??), code:block12 (Model produces: gui_session(action="list")), code:block13 (1. Request Construction (bedrock.rs:818-928)), code:block14 (4. Response Parsing — Empty Output Edge Case) (+19 more)

### Community 9 - "Community 9"
Cohesion: 0.09
Nodes (22): code:json ({), code:json ({), code:json ({), code:json ({), code:json ({), Compliant Tools (No Violations in required Array), Critical Observation: `additionalProperties` Missing Everywhere, Detailed Violation Breakdown by Tool (+14 more)

### Community 10 - "Community 10"
Cohesion: 0.09
Nodes (22): Causal Chain (Complete Step-by-Step), Classification, code:block1 (Error: Failed to call chat-completions api), code:rust (if text.is_empty() && tool_calls.is_empty() {), code:block3 (1. User configures model "openai.gpt-5.5" in Bedrock provide), Conclusion, CONTRIBUTING Factors (Ranked by Impact), Evidence Summary (From Tasks 1-11) (+14 more)

### Community 11 - "Community 11"
Cohesion: 0.09
Nodes (21): Existing Fixes/Workarounds From Issues:, GitHub Issues Research: aichat + GPT-5.5 / Strict Mode / Responses API / Empty Output, GPT-5 Model Addition Request, Issue #1306 — "Crash" (May 29, 2025), Issue #1325 — "structured output (for function calling): null objects aren't apart of the supported schema types" (Jun 23, 2025), Issue #1338 — "Calling regenerate with an empty output causes a panic" (Jul 2, 2025), Issue #1389 — "Function calling does not work" (Aug 26, 2025), Issue #1395 — "Add openai:gpt-5 model" (Sep 4, 2025) (+13 more)

### Community 12 - "Community 12"
Cohesion: 0.10
Nodes (20): 1. Schema Violations Are Universal (HIGH IMPACT), 2. Tool Count Exceeds Recommendation (MEDIUM IMPACT), 3. GPT-5.5 Literal Instruction Following (MEDIUM IMPACT), 4. Reasoning Effort at "medium" (LOW-MEDIUM IMPACT), 5. System Prompt Size (LOW IMPACT), Conclusion, Contributing Factors (Ranked by Impact), Documented vs. Observed Behavior (+12 more)

### Community 13 - "Community 13"
Cohesion: 0.10
Nodes (20): All Tools Scenario (FAILS), Code-Level Root Cause, code:block10 (Tools available: [22 tools, 12 violating strict mode]), code:json ({), code:rust (pub struct JsonSchema {), code:rust (body["tools"] = functions.iter().map(|v| {), code:block9 (Tools available: [fs_cat]  (1 tool, compliant schema)), Composite Severity Score Explanation (+12 more)

### Community 14 - "Community 14"
Cohesion: 0.11
Nodes (18): Behavior When Schema Validation Fails, Case 1: API-Level Rejection (Expected for Invalid Strict Schemas), Case 2: Auto-Normalization (Responses API Default), Case 3: Empty Output with "completed" Status (The User's Case), GPT-5.5 Specific Behavioral Notes, GPT-5.5 Tool Calling Behavior Research, Hypothesis: Why GPT-5.5 Returns Empty Output, Key Differences from GPT-4o/GPT-4.1 (+10 more)

### Community 15 - "Community 15"
Cohesion: 0.12
Nodes (17): AFTER (Fixed Behavior — Success Path), AFTER (Fixed Code), AFTER: Tool Object Sent to API (with fix), BEFORE (Current Behavior — Failure Path), BEFORE (Current Code), BEFORE: Tool Object Sent to API, code:rust (body["tools"] = functions), code:rust (body["tools"] = functions) (+9 more)

### Community 16 - "Community 16"
Cohesion: 0.15
Nodes (13): code:json ({), code:json ({), code:json ({), code:json ({), code:json ({), code:json ({), code:json ({), code:json ({) (+5 more)

### Community 17 - "Community 17"
Cohesion: 0.22
Nodes (9): Category (b): Configuration Workarounds, code:yaml (---), code:yaml (---), code:json ({), code:json ({), Workaround B1: Reduce Tool Count Below 20 (Effectiveness: HIGH), Workaround B2: Fix functions.json to Be Strict-Mode Compliant (Effectiveness: HIGH), Workaround B3: Use Streaming Mode (Effectiveness: MEDIUM) (+1 more)

### Community 18 - "Community 18"
Cohesion: 0.25
Nodes (7): 18: Strict Mode Interaction Patterns Across OpenAI Models, Appendix A: Quick Reference Card, Appendix B: Diagnostic Flowchart, code:block11 (┌───────────────────────────────────────────────────────────), code:block12 (Tool calling returns empty output?), Executive Summary, References

### Community 19 - "Community 19"
Cohesion: 0.29
Nodes (7): code:json ({), code:json ({), Part 2: How Strict Mode Differs by API, Practical Example: Before and After Normalization, The Asymmetry: Responses API vs Chat Completions, The Critical Quote (From OpenAI Documentation), What Auto-Normalization Actually Does

### Community 20 - "Community 20"
Cohesion: 0.33
Nodes (6): code:block3 (GPT-4o + Chat Completions + schema violations:), Fundamental Philosophy Difference, GPT-4o: "Best-Effort" Mode, GPT-5.5: "Literal Compliance" Mode, Part 3: GPT-5.5 vs GPT-4o Behavioral Differences, Why GPT-5.5 Specifically Fails (Not GPT-4o)

### Community 21 - "Community 21"
Cohesion: 0.33
Nodes (6): code:block7 (Configuration:), code:rust (// In bedrock.rs tool serialization:), How This Bug Manifests, Part 9: The aichat Case Study, The One-Line Fix, Why It Works with "1 Simple Tool Call"

### Community 22 - "Community 22"
Cohesion: 0.40
Nodes (5): code:json (// ralph_loop: has create_workspace, ralph_dir, project_dir,), code:json (// gui_session: 14 properties, only "action" originally requ), Part 5: Schema Violation Severity Taxonomy, When Strict Mode Is Active, Violations Are Not Equal, Worst Offender Examples

### Community 23 - "Community 23"
Cohesion: 0.50
Nodes (4): code:block6 (if status == "completed" AND output == [] AND output_tokens ), "Completed" + Empty Output = Undocumented Behavior, Detection Heuristic, Part 6: The Undocumented Edge Case

### Community 24 - "Community 24"
Cohesion: 0.50
Nodes (4): code:block9 (strict: true (explicit)), Part 10: Decision Framework, The Golden Rule, When to Use Each Strict Mode Setting

### Community 25 - "Community 25"
Cohesion: 0.50
Nodes (4): Cross-Model Migration Checklist, GPT-4o / GPT-4.1 (Non-Reasoning Models), GPT-5.5 (Reasoning Model), Part 8: Model-Specific Strict Mode Recommendations

### Community 26 - "Community 26"
Cohesion: 0.50
Nodes (4): code:block10 (┌──────────────────────────────────────────────┐), GPT-4o vs GPT-5.5 — Complete Comparison, Part 11: Summary of Key Behavioral Differences, The Three-Variable Interaction

### Community 27 - "Community 27"
Cohesion: 0.50
Nodes (4): Migration Trap, Part 4: The Interaction Matrix, The Dangerous Quadrant, The Full Compatibility Grid

### Community 28 - "Community 28"
Cohesion: 0.67
Nodes (3): How Tool Count Interacts with Strict Mode, Part 7: The Tool Count Amplification Effect, Why Scale Matters for GPT-5.5

### Community 29 - "Community 29"
Cohesion: 0.67
Nodes (3): Part 1: What Strict Mode Is, Strict Mode as Constrained Decoding, The Three Cardinal Rules

## Knowledge Gaps
- **306 isolated node(s):** `Summary`, `code:rust (impl BedrockModelCategory {)`, `code:rust (BedrockModelCategory::OpenAI => {)`, `code:rust (if let Some(functions) = functions {)`, `code:rust (if text.is_empty() && tool_calls.is_empty() {)` (+301 more)
  These have ≤1 connection - possible missing edges or undocumented components.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **Why does `18: Strict Mode Interaction Patterns Across OpenAI Models` connect `Community 18` to `Community 19`, `Community 20`, `Community 21`, `Community 22`, `Community 23`, `Community 24`, `Community 25`, `Community 26`, `Community 27`, `Community 28`, `Community 29`?**
  _High betweenness centrality (0.011) - this node is a cross-community bridge._
- **Why does `Fix Validation: Tracing the Recommended Fix Through the Error Scenario` connect `Community 8` to `Community 15`?**
  _High betweenness centrality (0.006) - this node is a cross-community bridge._
- **Why does `Fix Recommendations: aichat GPT-5.5 "Invalid Responses API response data" Error` connect `Community 4` to `Community 17`?**
  _High betweenness centrality (0.005) - this node is a cross-community bridge._
- **What connects `Summary`, `code:rust (impl BedrockModelCategory {)`, `code:rust (BedrockModelCategory::OpenAI => {)` to the rest of the system?**
  _306 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Community 0` be split into smaller, more focused modules?**
  _Cohesion score 0.05 - nodes in this community are weakly interconnected._
- **Should `Community 1` be split into smaller, more focused modules?**
  _Cohesion score 0.05714285714285714 - nodes in this community are weakly interconnected._
- **Should `Community 2` be split into smaller, more focused modules?**
  _Cohesion score 0.05714285714285714 - nodes in this community are weakly interconnected._