# GitHub Issues Research: aichat + GPT-5.5 / Strict Mode / Responses API / Empty Output

## Summary

Searched the `sigoden/aichat` GitHub repository for issues related to GPT-5.5, strict mode, Responses API, and empty output errors. **No existing issue matches our exact bug** (GPT-5.5 + strict mode schema violations → empty output → "Invalid Responses API response data" error). However, several related issues provide critical context for understanding the bug's origins and potential fixes.

---

## Key Finding: Responses API Was Explicitly REJECTED

### PR #1318 — "Fix o1-pro and o3-pro by adding support for OpenAI Responses API" (Jun 16, 2025)
- **Status**: CLOSED (rejected by maintainer)
- **URL**: https://github.com/sigoden/aichat/pull/1318
- **Maintainer response (sigoden)**: *"We will not support this feature. There are no current plans to support the Responses API."*
- **Context**: User `gcv` submitted a PR because o1-pro/o3-pro require the Responses API. Maintainer rejected it and removed o1-pro from models.yaml.
- **Later developments**: The Responses API code in our `src/client/bedrock.rs` was added via a **local commit** (`b92c83d`, Jun 3, 2026: "add bedrock-mantle support for openai models!"). This is NOT upstream aichat code — it's a custom fork extension.
- **Implication**: The bedrock.rs Responses API implementation is custom code without upstream review/testing, explaining why it has a bail!() on empty output instead of graceful handling.

### Issue #1431 — "Support OpenAI Responses API" (Oct 30, 2025)
- **Status**: OPEN (enhancement request)
- **URL**: https://github.com/sigoden/aichat/issues/1431
- **Content**: OpenAI is moving all models to the Responses API. `gpt5-codex` is ONLY available via Responses API. Community contributor offered a PR.
- **Community comments on PR #1318**: "All the modern OpenAI models are available via Responses API, a complete switch is better" — users note this is increasingly blocking. One user asked "any recommendations on a tool similar to aichat that supports all modern openai models?"

---

## Related Issues: Tool Schema / Strict Mode

### Issue #1325 — "structured output (for function calling): null objects aren't apart of the supported schema types" (Jun 23, 2025)
- **Status**: CLOSED
- **URL**: https://github.com/sigoden/aichat/issues/1325
- **Problem**: aichat sends `content: null` in assistant tool_calls messages, which violates OpenAI's structured output schema validation (`null` is not a supported schema type)
- **Fix**: Commit `ffcb19d` removed the `content` field entirely from tool_calls messages
- **Relevance**: Demonstrates that aichat's tool schema/message formatting has repeatedly caused compatibility issues with OpenAI's strict validation systems. The same class of bug (non-compliant schemas) is at the root of our GPT-5.5 issue.

### Issue #1389 — "Function calling does not work" (Aug 26, 2025)
- **Status**: CLOSED
- **URL**: https://github.com/sigoden/aichat/issues/1389
- **Problem**: User with **GPT-5** on Azure reported "Failed to call chat-completions api" error. Root cause: aichat omits `content` field from assistant tool_calls messages, causing Azure API to reject with FST_ERR_VALIDATION
- **User's own fix**: Setting `"content": null` in the assistant message resolves it
- **Relevance**: Nearly identical error pattern to our bug ("Failed to call chat-completions api"). Shows that tool message formatting issues with GPT-5 family models have been reported before but were fixed only for Chat Completions, not for Responses API.

---

## Related Issues: Empty Output Handling

### Issue #1338 — "Calling regenerate with an empty output causes a panic" (Jul 2, 2025)
- **Status**: CLOSED (fixed in #1340)
- **URL**: https://github.com/sigoden/aichat/issues/1338
- **Problem**: When model returns empty output and user calls `.regenerate`, aichat panics at `message.rs:230` with "index out of bounds: len is 0 but index is 0"
- **Fix**: PR #1340 ("enhance .regenerate")
- **Relevance**: Shows aichat has a pattern of crashing on empty outputs rather than handling them gracefully. Our bug is the same pattern — bedrock.rs bails on empty output array instead of warning.

### Issue #1306 — "Crash" (May 29, 2025)
- **Status**: CLOSED (fixed in #1340)
- **URL**: https://github.com/sigoden/aichat/issues/1306
- **Problem**: Gemini model returns empty output (blocked by `blockReason: "OTHER"`), then `.regenerate` triggers same panic at message.rs:230
- **Relevance**: Another instance of models returning empty output for various reasons (content filtering, safety blocks). The pattern: API returns "success" status but empty content → aichat crashes.

---

## Related Issues: Tool Call Streaming/Processing Bugs

### Issue #1495 — "Tool call arguments lost (sent as {}) on OpenAI-compatible providers" (Mar 22, 2026)
- **Status**: OPEN
- **URL**: https://github.com/sigoden/aichat/issues/1495
- **Problem**: Streaming tool call boundary detection bug causes function arguments to be flushed as `{}`. The boundary detection formula in `openai.rs` uses `{id}/{index}` which fails when `id` is absent in continuation chunks.
- **Relevance**: Shows active tool handling bugs in aichat's streaming code. While a different mechanism than our bug, it demonstrates fragility in tool call processing.

---

## No Direct Matches Found

The following searches returned **zero results**:
- `"Invalid Responses API"` — our exact error message has never been reported
- `gpt-5.5` — no issues mention this model
- `additionalProperties` — no issues discuss this strict mode requirement
- `responses bedrock` — no issues about the Responses API via Bedrock
- `tool count limit` — no issues about maximum tool count

---

## GPT-5 Model Addition Request

### Issue #1395 — "Add openai:gpt-5 model" (Sep 4, 2025)
- **Status**: CLOSED
- **URL**: https://github.com/sigoden/aichat/issues/1395
- **Content**: User requested GPT-5 be added to models.yaml. It was added in a subsequent models.yaml update.
- **Note**: GPT-5 was added to Chat Completions client (openai.rs), not Responses API. GPT-5.5 likely requires Responses API routing (hence the bedrock-mantle path).

---

## Synthesis: What This Means for Our Bug

### Why No One Has Reported This Exact Bug:
1. **The Responses API code is custom/local** — commit `b92c83d` ("add bedrock-mantle support for openai models!") is not in upstream aichat. Only our fork has this code.
2. **GPT-5.5 is a very new model** — no GitHub issues mention it at all.
3. **The strict mode + empty output combination is novel** — it requires: (a) Responses API routing via bedrock-mantle, (b) GPT-5.5's strict schema compliance behavior, (c) schemas that violate strict mode rules, (d) enough tools to trigger the behavior.

### Existing Fixes/Workarounds From Issues:

| Issue | Fix Applied | Applicable to Our Bug? |
|-------|-------------|----------------------|
| #1325 | Remove `content: null` from tool messages | No — different issue |
| #1338/#1306 | Enhanced `.regenerate` handling | Partially — shows need for graceful empty handling |
| #1389 | Omit content field entirely | No — Chat Completions specific |
| ffcb19d | Remove content field from tool_calls | No — different layer |

### Recommended Workarounds Based on Issue Research:

1. **Immediate**: Reduce tool count below 20 (GPT-5.5 soft limit noted in our earlier research)
2. **Code fix**: Change `bail!()` at bedrock.rs:982 to `warn()` + return empty (like openai.rs:341-355 already does for Chat Completions)
3. **Schema fix**: Make all tool schemas strict-mode-compliant (add all properties to required, add `additionalProperties: false`)
4. **Upstream awareness**: Issue #1431 is still open — the official aichat has NO Responses API support. Our bedrock-mantle code is a custom extension that needs the same graceful error handling as the upstream Chat Completions code.

---

## Timeline of Related Events

| Date | Event |
|------|-------|
| Jun 16, 2025 | PR #1318 rejected: "No plans to support Responses API" |
| Jun 23, 2025 | Issue #1325: null schema validation issues reported |
| Jun 23, 2025 | Commit ffcb19d: fix content field in tool messages |
| Jul 2-3, 2025 | Issues #1306/#1338: empty output panics reported & fixed |
| Jul 6, 2025 | v0.30.0 released |
| Sep 4, 2025 | Issue #1395: GPT-5 model addition requested |
| Oct 30, 2025 | Issue #1431: Responses API support requested (still open) |
| Jun 3, 2026 | Commit b92c83d: Local bedrock-mantle Responses API added |
| Current | Our bug: GPT-5.5 + strict mode + empty output + bail!() |
