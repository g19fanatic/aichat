# vim-llm-assistant Call Structure Analysis

## Executive Summary

vim-llm-assistant is a Vim plugin that builds a JSON context payload from the current editing environment and passes it to `aichat` via the CLI adapter. The plugin already implements **deliberate cache-friendly content ordering** — placing stable, large data first and volatile, small data last — but actual API-level caching (`cache_control` markers) must be implemented in aichat's Rust code. The plugin's architecture provides excellent cache leverage opportunities because it separates static content from dynamic content at the structural level.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│  vim-llm-assistant (VimScript)                                   │
│                                                                   │
│  :LLM [prompt]  →  llm#run()  →  builds JSON  →  writes tempfile │
│                                                                   │
│  Adapter (aichat.vim):                                            │
│    aichat --role <role> --model <model> [-f file ...] --file <json> [-- prompt] │
│                                                                   │
└────────────────────────────────────────────┬────────────────────────┘
                                             │
                                             ▼
┌─────────────────────────────────────────────────────────────────┐
│  aichat (Rust CLI)                                               │
│                                                                   │
│  Input::from_files() → load documents, order content             │
│  Role system → system prompt (default-vim-role.md, 39KB)         │
│  Tool definitions → from functions/ directory                     │
│                                                                   │
│  API request to Anthropic:                                        │
│    system: [role content]                                         │
│    tools:  [tool definitions]                                     │
│    messages: [{role: user, content: [documents + prompt]}]        │
└─────────────────────────────────────────────────────────────────┘
```

---

## Data Flow Detail

### 1. User Triggers Request

Two commands:
- `:LLM [prompt]` — context + optional prompt
- `:LLMFile <files> [-- prompt]` — additional file attachments + optional prompt

Both route to `llm#run()` (`autoload/llm.vim:449`).

### 2. Context Assembly (`llm#run()`)

Gathers from the Vim environment:

| Field | Source | Volatility |
|-------|--------|-----------|
| `llm_history` | `[LLM-Scratch]` buffer content (all prior turns) | **APPEND-ONLY** — earlier turns never change |
| `buffers[]` | All tab-visible buffers (except active, scratch, snippet) | **SEMI-STATIC** — stable within editing session |
| `active_buffer` | Currently focused buffer `{filename, contents}` | **SEMI-STATIC** — changes as user edits |
| `file_arguments` | Explicit file paths from `:LLMFile` | **STATIC** per invocation |
| `prompt` | User's inline question | **DYNAMIC** — changes every request |
| `cursor_line` | Current line number | **DYNAMIC** — changes constantly |
| `cursor_col` | Current column number | **DYNAMIC** — changes constantly |

### 3. Deterministic JSON Encoding (`llm#encode()`)

**KEY CACHE OPTIMIZATION ALREADY PRESENT** — `autoload/llm.vim:32-70`

The plugin enforces **deterministic top-level key ordering** specifically for cache prefix stability:

```vim
let l:ordered_keys = [
      \ 'llm_history',      " 1. Large, stable prefix (earlier turns never change)
      \ 'buffers',          " 2. Stable across requests in same session
      \ 'active_buffer',    " 3. Stable most of the time
      \ 'file_arguments',   " 4. Stable per session
      \ 'prompt',           " 5. Changes per request
      \ 'cursor_line',      " 6. Changes constantly (small)
      \ 'cursor_col',       " 7. Changes constantly (small)
      \ ]
```

Comment in code: *"stable (large) → variable (small) for maximum prefix cache hits"*

This means the JSON content, when read by aichat, has a **long stable prefix** (history + buffers) followed by a **short volatile suffix** (prompt + cursor position).

### 4. Command Construction (`aichat.vim:161-170`)

The adapter builds:
```bash
LLM_OUTPUT=<tempfile> aichat --role <g:llm_role> --model <model> [-f file1 -f file2 ...] --file <context.json> [-- <prompt>]
```

Key parameters:
- `--role default-vim-role` → loads `default-vim-role.md` (39KB / ~5,200 words / ~13,000 tokens)
- `--model claude-3-7-sonnet-20250219`
- `-f file1 -f file2 ...` → extracted from JSON `file_arguments` field
- `--file context.json` → the assembled JSON context
- `-- prompt` → optional direct prompt text

### 5. Content Ordering in aichat (`src/config/input.rs:57-105`)

aichat's `Input::from_files()` arranges content explicitly for caching:
1. **Documents first** — annotated with comment: "static content — cache-friendly prefix"
2. **Last reply** — annotated: "semi-static — stable per session turn"
3. **Raw text (prompt) last** — annotated: "dynamic content — changes every request"

---

## Content Size Estimates

| Component | Typical Size | Token Estimate | Cache Stability |
|-----------|-------------|---------------|-----------------|
| Role (`default-vim-role.md`) | 39 KB | ~13,000 tokens | **PERFECT** — never changes between requests |
| Tool definitions | ~10-20 KB | ~3,000-7,000 tokens | **PERFECT** — never changes |
| `llm_history` | 5-100 KB (grows) | ~2,000-30,000 tokens | **APPEND-ONLY** — prefix always stable |
| `buffers[]` | 2-50 KB | ~1,000-15,000 tokens | **HIGH** — stable within editing session |
| `active_buffer` | 1-30 KB | ~500-10,000 tokens | **MEDIUM** — changes with edits |
| `file_arguments` content | 0-200 KB | ~0-60,000 tokens | **HIGH** — same files per session |
| `prompt` | 10-500 bytes | ~5-150 tokens | **NONE** — changes every request |
| `cursor_line/col` | ~20 bytes | ~5 tokens | **NONE** — changes every request |

**Total cacheable prefix per request**: Typically 20,000-60,000+ tokens of stable content.

---

## Existing Cache-Awareness Features

### Already Implemented
1. **Deterministic JSON key ordering** — `llm#encode()` ensures stable prefix
2. **Content ordering within JSON** — stable content first, volatile last
3. **aichat content ordering** — documents before prompt in text assembly
4. **Comment annotations** — code comments explicitly reference cache optimization goals

### Not Yet Implemented (Requires aichat Changes)
1. **`cache_control` markers** — no API-level cache breakpoints are set
2. **System prompt caching** — role content not marked with cache_control
3. **Tool definition caching** — tools not marked with cache_control
4. **Conversation prefix caching** — llm_history not given cache breakpoints
5. **Multi-turn cache breakpoint movement** — no dynamic breakpoint management

---

## Extension Points for Cache Control

### `g:llm_adapter_cmd_extra` Hook

The adapter supports a command augmentation function (`autoload/llm/adapters/aichat.vim:138-145`):

```vim
if exists('g:llm_adapter_cmd_extra') && has_key(g:llm_adapter_cmd_extra, 'aichat')
    let l:cmd_extra_func = g:llm_adapter_cmd_extra.aichat
    if exists('*'.l:cmd_extra_func)
        let l:cmd_extra = call(l:cmd_extra_func, [a:json_filename, a:prompt, l:model])
    endif
endif
```

This hook can:
- Prepend environment variables (e.g., `AICHAT_CACHE_MODE=explicit`)
- Add CLI flags (e.g., `--cache-breakpoints auto`)
- Pass configuration signals to aichat for cache behavior

### Snippet System for Context Reduction

The snippet system (`llm#get_buffer_content()`) replaces full buffer content with specific line ranges. This:
- Reduces total token count (fewer tokens = faster)
- Keeps content more stable (specific relevant sections don't change as often)
- Could be leveraged to keep cached prefixes smaller and more stable

---

## Static vs Dynamic Content Summary

### Completely Static (Perfect Cache Candidates)
- **System prompt** (role file): 39KB, ~13,000 tokens — sent identically every request
- **Tool definitions**: ~10-20KB — sent identically every request
- **Skills content** (loaded via role): Variable — same for a given role configuration

### Append-Only (Cache-Prefix Stable)
- **`llm_history`**: Earlier conversation turns never change; new turns are appended
- **This is the MOST VALUABLE cache opportunity** — a 10-turn conversation with 5,000 tokens/turn = 50,000 cached tokens on every subsequent request

### Session-Stable (Cache-Hit Likely Within Session)
- **`buffers[]`**: Same files stay open during an editing session
- **`file_arguments`**: Same files attached for related questions
- **`active_buffer`**: Changes with edits but stable between edits

### Always Dynamic (Cannot Cache)
- **`prompt`**: New question each time
- **`cursor_line` / `cursor_col`**: Changes with every cursor movement

---

## Critical Architecture Observations for Implementation

### 1. The JSON Context is a Single User Message
The entire vim context JSON becomes ONE text block in the Anthropic API's `messages` array. For cache_control to work on sub-parts (e.g., just the history prefix), aichat would need to **split this into multiple content blocks** within the user message.

### 2. Role Content is Separate (System Prompt)
The role file (`default-vim-role.md`) is already isolated as the system prompt — it can get its own `cache_control` marker trivially.

### 3. The -f Files Are Concatenated
Files passed via `-f` flags are loaded as "documents" and concatenated into the text. They could each get their own content block with cache markers.

### 4. History Growth Pattern
`llm_history` grows append-only. In a 5-turn conversation:
- Turn 1: 5,000 tokens (history)
- Turn 2: 10,000 tokens (turn 1 + turn 2 history)
- Turn 3: 15,000 tokens
- ...

Each request's history has the **previous request's history as a prefix**. This is the ideal pattern for prompt caching — the cache prefix naturally grows and remains stable.

### 5. Cursor Position at End is Key
By placing `cursor_line`/`cursor_col` at the END of the JSON, the entire preceding content is a stable prefix. Moving cursor position (which happens constantly) doesn't invalidate the cache of everything before it.

---

## Key File References

| File | Location | Purpose |
|------|----------|---------|
| `plugin/llm.vim` | `/home/pdibiase/sources/vim-llm-assistant/plugin/llm.vim` | Commands, defaults, adapter loading (76 lines) |
| `autoload/llm.vim` | `/home/pdibiase/sources/vim-llm-assistant/autoload/llm.vim` | Core logic: `llm#run()`, `llm#encode()`, sessions (~830 lines) |
| `autoload/llm/adapters/aichat.vim` | `/home/pdibiase/sources/vim-llm-assistant/autoload/llm/adapters/aichat.vim` | CLI adapter: command construction, async jobs (~293 lines) |
| `autoload/llm/adapter.vim` | `/home/pdibiase/sources/vim-llm-assistant/autoload/llm/adapter.vim` | Adapter registry interface (54 lines) |
| `autoload/llm/log.vim` | `/home/pdibiase/sources/vim-llm-assistant/autoload/llm/log.vim` | Logging infrastructure (~350 lines) |
| `default-vim-role.md` | `/home/pdibiase/sources/vim-llm-assistant/default-vim-role.md` | System prompt (39KB, ~13,000 tokens) |

---

## Recommendations for Cache Implementation

### Vim-Side (Low Effort, High Impact)
1. **Ensure history prefix stability** — already done via `llm#encode()` ordering
2. **Consider signaling cache hints** — pass metadata about content boundaries to aichat (e.g., where history ends and new content begins)
3. **Use `cmd_extra` hook** — signal caching preferences via environment variables

### aichat-Side (Required for Actual Caching)
1. **Mark system prompt with `cache_control`** — role content = ~13,000 tokens, trivial to cache
2. **Mark tool definitions with `cache_control`** — static between requests
3. **Split user message into blocks** — separate the stable document/history prefix from the dynamic prompt suffix, marking the stable portion with `cache_control`
4. **Move cache breakpoints on growth** — as `llm_history` grows, move the cache breakpoint to include the latest stable prefix

### Potential Savings Per Request
With a typical session (5+ turns, role + tools + history):
- **Without caching**: Pay full price for ~30,000-50,000 input tokens each request
- **With caching**: Pay 90% less for ~25,000-45,000 cached tokens, full price only for ~5,000 new tokens
- **Estimated savings**: 70-85% reduction in input token costs per request after first request
