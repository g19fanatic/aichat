# Audit: Current State of Cache Implementation Wiring

## Executive Summary

**The core wiring IS complete.** Data flows end-to-end from vim JSON input → parsing → message building → Claude API request body. The remaining 4 compiler warnings are about dead/unused code that should be cleaned up, NOT about missing wiring.

## Data Flow (Fully Wired)

```
vim-llm-assistant sends JSON
    → src/config/input.rs:from_files() [line 126-135]
        ├── parse_vim_history_turns(contents)  → Input.vim_history_turns
        ├── split_json_content_blocks(contents) → Input.cache_content_blocks
        └── detect_cache_warm_field(contents)   → Input.cache_warm

Input.build_messages() [line 354-378]
    └── Prepends vim_history_turns as User/Assistant message pairs before last user message

Input.prepare_completion_data() [line 323-341]
    └── Passes cache_content_blocks + cache_warm into ChatCompletionsData struct

claude_build_chat_completions_body(data) [line 164+]
    ├── Destructures: cache_content_blocks, cache_warm from data [line 170-176]
    ├── cache_warm → sets max_tokens=1 [line 289-291]
    ├── cache_content_blocks → replaces last user message content with block array [line 347-369]
    ├── History cache breakpoint → last assistant message before current user [line 384-417]
    └── Stepping-stone breakpoint → midpoint in long history [line 418-360]
```

## Current Warnings (4 total)

### Warning 1: Unused imports in mod.rs (line 8)
```rust
pub use self::input::{VimHistoryTurn, parse_vim_history_turns, CacheContentBlock, split_json_content_blocks};
```
- `CacheContentBlock` IS used by `src/client/common.rs` (in ChatCompletionsData struct)
- `VimHistoryTurn`, `parse_vim_history_turns`, `split_json_content_blocks` are NOT used outside the `config` module
- These re-exports exist for hypothetical external crate use but aichat is a binary, not a library

### Warning 2: CacheHints struct fields never read (input.rs:34-40)
```rust
pub struct CacheHints {
    pub breakpoint_after: Vec<String>,  // NEVER READ
    pub stable_fields: Vec<String>,     // NEVER READ
    pub dynamic_fields: Vec<String>,    // NEVER READ
}
```
- The `split_json_content_blocks()` function parses `_cache_hints` JSON directly without using this struct
- The struct is defined but NEVER constructed anywhere in the codebase
- It's pure dead code — a design artifact that was superseded

### Warning 3: field_name never read (input.rs:50)
```rust
pub struct CacheContentBlock {
    pub field_name: String,   // SET in split_json_content_blocks() but NEVER READ
    pub text: String,         // Used in claude.rs
    pub is_breakpoint: bool,  // Used in claude.rs
}
```
- `field_name` is populated during parsing but claude.rs only uses `.text` and `.is_breakpoint`
- It's informational dead weight — could be useful for debugging but triggers a warning

### Warning 4: Accessor methods never used (input.rs:287-299)
```rust
pub fn vim_history_turns(&self) -> &[VimHistoryTurn] { ... }
pub fn cache_content_blocks(&self) -> &[CacheContentBlock] { ... }
pub fn is_cache_warm(&self) -> bool { ... }
```
- These public methods on `Input` are never called by external code
- The implementation uses direct field access (`self.vim_history_turns`, `self.cache_content_blocks`, `self.cache_warm`) within the same `impl Input` block
- The accessor methods were written anticipating external callers that don't exist

## Assessment of Original Fix Plan Tasks

| Task | Status | Explanation |
|------|--------|-------------|
| 2. Wire vim_history_turns into claude.rs | ✅ ALREADY DONE | `build_messages()` handles this at line 354-378 |
| 3. Wire cache_content_blocks into claude.rs | ✅ ALREADY DONE | `claude_build_chat_completions_body()` handles this at line 347-369 |
| 4. Wire is_cache_warm into request flow | ✅ ALREADY DONE | `claude_build_chat_completions_body()` sets max_tokens=1 at line 289-291 |
| 5. Clean up dead code warnings | ⚠️ STILL NEEDED | 4 warnings remain |

## Recommended Fix (Task 5)

1. **Remove unused re-exports from mod.rs**: Remove `VimHistoryTurn`, `parse_vim_history_turns`, `split_json_content_blocks` from the `pub use` line (keep `CacheContentBlock`)
2. **Remove `CacheHints` struct entirely**: It's dead code never used anywhere
3. **Remove `field_name` from `CacheContentBlock`**: Not read anywhere (or add `#[allow(dead_code)]` if debugging value is desired)
4. **Remove accessor methods or add `#[allow(dead_code)]`**: Either remove `vim_history_turns()`, `cache_content_blocks()`, `is_cache_warm()` OR mark with `#[allow(dead_code)]` if they're intended as public API for future use

### Simplest approach to zero warnings:
- Remove `CacheHints` struct
- Remove `field_name` from `CacheContentBlock`
- Remove the three unused accessor methods (data flows via direct field access)
- Fix mod.rs re-export line to only export `CacheContentBlock`
