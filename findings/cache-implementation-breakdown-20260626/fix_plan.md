# fix_plan.md
# Anthropic Prompt Cache Implementation — Full Breakdown
# Source: findings/anthropic-cache-optimization-20260626/10-implementation-plan.md
#
# Phases 1-3: ~90% of benefit, ~5 hours
# Phases 4-5: Compounding optimization, ~11 hours
#
# DEPENDENCY KEY:
#   Depends: [N] means task N must be [x] before this task starts

## Phase 1: aichat Critical Fixes (highest ROI)
- [ ] 1. Add cache_control to last tool in tools array: In claude_build_chat_completions_body(), after the if should_cache block (~line 311), add cache_control on last tool. Pattern: `tools_arr.last_mut() → last_tool["cache_control"] = json!({"type": "ephemeral"})`. Files: src/client/claude.rs:306-335
- [ ] 2. Sort tools alphabetically for deterministic cache key: In the functions→tools mapping block, collect into Vec, sort_by name, then assign to body["tools"]. Files: src/client/claude.rs:289-303
- [ ] 3. Extract Anthropic cache metrics from response usage: In claude_extract_chat_completions(), read data["usage"]["cache_creation_input_tokens"] and data["usage"]["cache_read_input_tokens"], pack into ChatCompletionsOutput.extra as Some(json!(...)). Files: src/client/claude.rs:345-395
- [ ] 4. Display cache metrics to stderr: In common.rs where Bifrost cache is displayed (~line 480-495), add equivalent block: if extra has cache_creation/cache_read fields, eprintln!("📦 Cache: {} read, {} written"). Depends: [3]. Files: src/client/common.rs:480-495
- [ ] 5. Remove top-level body["cache_control"] = json!({"type": "ephemeral"}): This frees 1 of 4 breakpoint slots for explicit use. It's line ~311 inside the `if should_cache` block. The explicit breakpoints on system + last message already cover what auto mode does. Files: src/client/claude.rs:311

## Phase 2: aichat Advanced Caching
- [ ] 6. Add intermediate conversation breakpoint for long sessions: After the last-message cache logic (~line 335), if messages.len() > 20, find mid-point user message, convert its content to array format with cache_control. Uses the breakpoint slot freed by task 5. Depends: [5]. Files: src/client/claude.rs (after line 335)
- [ ] 7. Add 1-hour extended TTL to system prompt cache_control: In the system message caching block (lines 312-318), conditionally use json!({"type": "ephemeral", "ttl": "1h"}) when claude_supports_extended_cache() returns true. Files: src/client/claude.rs:312-318
- [ ] 8. Mirror tool caching for Claude-via-OpenRouter: In openai.rs Claude caching block (~lines 353-386), add same pattern: get tools array, add cache_control to last tool's function field. Files: src/client/openai.rs:353-386

## Phase 3: vim-llm-assistant Quick Wins
- [ ] 9. Remove cursor_line/cursor_col from JSON payload: In llm#run(), after building l:data, extract cursor values to separate let, remove from dict before JSON encode. Pass via env vars in adapter cmd: AICHAT_CURSOR_LINE=N AICHAT_CURSOR_COL=M prepended to command. Update l:ordered_keys to remove cursor entries. Files: autoload/llm.vim:37-44,648-651, autoload/llm/adapters/aichat.vim:161-170
- [ ] 10. aichat: Read cursor env vars and append to user message suffix: In input.rs, after building the main text content, check for AICHAT_CURSOR_LINE/COL env vars and append a small "cursor_line:N, cursor_col:M" note at the END of the user message (after all cached content). Files: src/config/input.rs
- [ ] 11. Reorder l:ordered_keys — move file_arguments before active_buffer: In llm#encode(), change ordering to ['llm_history', 'buffers', 'file_arguments', 'active_buffer', 'prompt']. file_arguments is more stable (set once per session) than active_buffer (changes with edits). Files: autoload/llm.vim:37-44

## Phase 4: vim-llm-assistant Architecture
- [ ] 12. Implement llm#parse_history_turns() function: New function that reads g:llm_scratch_bufnr lines, splits on '==== ... ====' delimiters, extracts 'Prompt: ...' as user text, remaining as assistant text. Returns list of dicts [{timestamp, user, assistant}]. Files: autoload/llm.vim (new function)
- [ ] 13. Add llm_history_turns to data dict in llm#run(): Call llm#parse_history_turns(), if non-empty add as l:data.llm_history_turns. Keep legacy l:data.llm_history for backward compat. Depends: [12]. Files: autoload/llm.vim:617-620
- [ ] 14. Add _cache_hints field to data dict: In llm#run(), add l:data._cache_hints = {'breakpoint_after': ['llm_history', 'buffers'], 'stable_fields': ['llm_history','buffers','file_arguments'], 'dynamic_fields': ['prompt']}. Files: autoload/llm.vim (in llm#run, after data assembly)
- [ ] 15. Implement :LLMWarm command for cache warming: New llm#warm_cache() function that builds context exactly as llm#run() but with _cache_warm=1 and dummy prompt. New :LLMWarm command in plugin/llm.vim. Fires async, reports cache_creation result. Files: autoload/llm.vim, plugin/llm.vim

## Phase 5: Full Multi-Turn Architecture
- [ ] 16. aichat: Parse llm_history_turns from JSON input: In input.rs, add parse_vim_history_turns() that detects the llm_history_turns array field in JSON, extracts Vec of (user_text, assistant_text) pairs. Handle missing/empty fields gracefully. Depends: [13]. Files: src/config/input.rs
- [ ] 17. aichat: Build proper multi-turn messages from parsed turns: When llm_history_turns present, prepend user/assistant message pairs to the messages array BEFORE the current user context message. Each turn → Message{role:user, content:user_text} + Message{role:assistant, content:assistant_text}. Depends: [16]. Files: src/config/input.rs (message building logic)
- [ ] 18. aichat: Handle multi-turn edge cases: Empty assistant (turn in progress), turns with only user text, extremely long assistant responses (truncation?), special characters in content. Add unit tests for edge cases. Depends: [17]. Files: src/config/input.rs, tests/
- [ ] 19. aichat: Place cache breakpoint at last history turn boundary: In claude_build_chat_completions_body(), when multi-turn history messages are detected (more than 2 messages from turns), place cache_control on the LAST assistant message from history (right before the current user message). Depends: [17]. Files: src/client/claude.rs
- [ ] 20. aichat: Adjust intermediate breakpoint logic for multi-turn: Task 6's logic (>20 messages) needs to account for the new multi-turn messages. Place stepping stone in the HISTORY messages rather than current-context messages. Depends: [6, 19]. Files: src/client/claude.rs
- [ ] 21. aichat: Parse _cache_hints from JSON and split user message into content blocks: When _cache_hints present, split single user message text at designated boundary markers into multiple content array blocks. Each block = separate cache unit. Depends: [14]. Files: src/config/input.rs
- [ ] 22. aichat: Place cache_control on hint-designated content blocks: For blocks marked by breakpoint_after in _cache_hints, add cache_control to that content block's last element. Depends: [21]. Files: src/client/claude.rs
- [ ] 23. vim: Signal multi-turn readiness via env var: In aichat adapter command construction, when llm_history_turns is populated, add AICHAT_MULTITURN_READY=1 to the env prefix. Depends: [13]. Files: autoload/llm/adapters/aichat.vim
- [ ] 24. vim: Feature-flag flat history removal: Add g:llm_multiturn_mode (default 0). When set to 1 AND llm_history_turns is populated, skip adding llm_history (flat text) to l:data — saves ~50% payload size. Depends: [23]. Files: autoload/llm.vim (in llm#run)
- [ ] 25. aichat: Support cache warming (max_tokens:1): Detect _cache_warm field in input or AICHAT_CACHE_WARM=1 env var. Set max_tokens to 1 (minimum allowed). Suppress normal output display. Return only usage metrics showing cache_creation. Depends: [15]. Files: src/config/input.rs, src/client/claude.rs
- [ ] 26. Integration test: Verify multi-turn cache reads grow per turn: Script that makes 3 sequential requests with growing history. Verify via stderr metrics that cache_read_input_tokens increases each turn while cache_creation decreases. Depends: [4, 17, 19]. Files: tests/ or scripts/
- [ ] 27. Document full multi-turn cache architecture: Write project_info/cache-architecture.md covering: breakpoint allocation strategy, multi-turn message building, turn parsing contract, cache hint system, verification strategy. Depends: [all prior]. Files: project_info/cache-architecture.md
