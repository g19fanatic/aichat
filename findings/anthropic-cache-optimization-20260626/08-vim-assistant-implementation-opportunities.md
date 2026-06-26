# Implementation Opportunities in vim-llm-assistant for Cache Optimization

**Task**: Identify how vim-llm-assistant could restructure its calls to maximize caching
**Date**: 2025-07-16
**Based on**: Findings 01 (API mechanics), 02 (optimization strategies), 04 (call analysis), 05 (token patterns)

---

## Executive Summary

vim-llm-assistant already implements **deliberate cache-friendly content ordering** via `llm#encode()` (stable→dynamic key order) and structures data with static content before dynamic content. However, the plugin currently delivers ALL context as a **single JSON text blob** that aichat treats as one content block in the API request. This fundamentally limits caching because the dynamic suffix (prompt, cursor position) invalidates the entire blob's hash — there's no way for Anthropic's API to cache just the stable prefix portion within a single content block.

The primary opportunity is to **communicate content boundaries** to aichat so it can split the single blob into multiple content blocks with strategic `cache_control` breakpoints. Secondary opportunities involve reducing content volatility and leveraging the plugin's intimate knowledge of what's static vs. dynamic.

---

## Opportunity Matrix

| # | Opportunity | Impact | Effort | Dependencies |
|---|------------|--------|--------|--------------|
| 1 | Signal cache boundaries via metadata | **HIGH** | Low | aichat must parse metadata |
| 2 | Separate prompt/cursor from context file | **HIGH** | Low | CLI flag coordination |
| 3 | Structure history as turn-delimited sections | **HIGH** | Medium | aichat must parse turns |
| 4 | Use cmd_extra hook for cache mode signaling | **MEDIUM** | Low | aichat env var support |
| 5 | Add cache-warming command | **MEDIUM** | Low | aichat max_tokens:0 support |
| 6 | Snippet system as cache stabilizer | **LOW** | Already done | None |
| 7 | Buffer content fingerprinting | **LOW** | Medium | Protocol design |
| 8 | Proper multi-turn conversation mode | **HIGHEST** | High | Major architecture change |

---

## 1. Signal Cache Boundaries via JSON Metadata

### Current Problem

The JSON payload from vim-llm-assistant looks like:
```json
{
  "llm_history": "==== Mon Jul 15 10:30:00 ====\nPrompt: ...\nResponse text...\n\n==== Mon Jul 15 10:35:00 ====\n...",
  "buffers": [{"filename": "...", "contents": "..."}],
  "active_buffer": {"filename": "...", "contents": "..."},
  "file_arguments": ["/path/to/file1"],
  "prompt": "What is this function doing?",
  "cursor_line": 45,
  "cursor_col": 12
}
```

aichat reads this and concatenates it into a single text block in the user message. The entire block gets one hash — if `prompt` or `cursor_col` changes, the ENTIRE prefix (including the 30K-token history) is treated as different content.

### Proposed Solution

Add a `_cache_hints` metadata field to the JSON that tells aichat where to place cache breakpoints:

```json
{
  "llm_history": "...",
  "buffers": [...],
  "active_buffer": {...},
  "file_arguments": [...],
  "prompt": "What is this function doing?",
  "cursor_line": 45,
  "cursor_col": 12,
  "_cache_hints": {
    "breakpoint_after": ["llm_history", "buffers"],
    "stable_fields": ["llm_history", "buffers", "file_arguments"],
    "dynamic_fields": ["prompt", "cursor_line", "cursor_col"],
    "semi_stable_fields": ["active_buffer"]
  }
}
```

### Implementation in vim-llm-assistant

**File**: `autoload/llm.vim` (in `llm#run()`, around line 560-580)

```vim
" After assembling l:data, add cache hints
let l:data._cache_hints = {
      \ 'breakpoint_after': ['llm_history', 'buffers'],
      \ 'stable_fields': ['llm_history', 'buffers', 'file_arguments'],
      \ 'dynamic_fields': ['prompt', 'cursor_line', 'cursor_col'],
      \ 'semi_stable_fields': ['active_buffer'],
      \ }
```

### What aichat Must Do

Parse `_cache_hints` and split the single user message into multiple content blocks:

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "## LLM History\n...(large stable history)...\n\n## Open Buffers\n...",
      "cache_control": {"type": "ephemeral"}
    },
    {
      "type": "text",
      "text": "## Active Buffer\n...(semi-stable)...\n\n## Prompt\nWhat is this function doing?\n\n## Cursor Position\nLine 45, Col 12"
    }
  ]
}
```

### Estimated Cache Savings

- Typical history + buffers: 20,000-50,000 tokens (cached at 90% discount)
- Dynamic suffix: ~200-500 tokens (full price)
- **Net savings per request after first**: 80-90% of input token costs

---

## 2. Separate Prompt/Cursor from Context File

### Current Problem

The `prompt` and `cursor_line`/`cursor_col` fields are embedded in the same JSON file as the stable context. This forces the entire file content to change every request.

### Proposed Solution

vim-llm-assistant already passes the prompt via `-- <prompt>` on the command line for aichat. The issue is that cursor position and other dynamic data are still in the JSON. Two approaches:

#### Approach A: Environment Variables for Dynamic Data

```vim
" In aichat.vim, modify command construction:
let l:cmd_base = ['bash', '-c', 
      \ 'AICHAT_CURSOR_LINE=' . l:data.cursor_line
      \ . ' AICHAT_CURSOR_COL=' . l:data.cursor_col
      \ . ' ' . l:cmd_extra . ...
      \ ]
```

Then remove `cursor_line` and `cursor_col` from the JSON payload. aichat would read them from environment variables and append them to the message AFTER the cache breakpoint.

#### Approach B: Separate Metadata File

```vim
" Write stable context to one file, dynamic context to another
let l:stable_json = llm#encode(l:stable_data)  " history + buffers + file_args
let l:dynamic_json = json_encode({'prompt': l:prompt, 'cursor_line': l:cursor_line, 'cursor_col': l:cursor_col, 'active_buffer': l:active_buffer})

call writefile(split(l:stable_json, "\n"), l:stable_file)
call writefile(split(l:dynamic_json, "\n"), l:dynamic_file)

" Pass both files:
" aichat --role ... --file <stable>.json --file <dynamic>.json -- <prompt>
```

### Implementation in vim-llm-assistant

**File**: `autoload/llm/adapters/aichat.vim:161-170`

Modify command construction to exclude dynamic fields from the main JSON:

```vim
" Remove from JSON before writing:
if has_key(l:json_data, 'cursor_line')
  let l:cursor_env = 'AICHAT_CURSOR_LINE=' . l:json_data.cursor_line 
        \ . ' AICHAT_CURSOR_COL=' . l:json_data.cursor_col . ' '
  " These are now passed via env, not in the JSON
endif
```

Or in `llm#run()`, simply omit them from the JSON and pass them separately.

### Impact

Without cursor_line/cursor_col in the JSON, the file content only changes when:
- History grows (new turn appended) — but prefix is still stable
- Active buffer content changes — semi-stable
- Buffers change — session-stable

The JSON file would be **identical** between requests where the user only moves their cursor, enabling full cache hits.

---

## 3. Structure History as Turn-Delimited Sections

### Current Problem

`llm_history` is raw text from the [LLM-Scratch] buffer:
```
==== Mon Jul 15 10:30:00 2025 ====
Prompt: What is this?
Here's the explanation...

==== Mon Jul 15 10:35:00 2025 ====
Prompt: Refactor it
Here's the refactored version...
```

This is treated as a single opaque text field by aichat. There's no way for aichat to know where turn boundaries are to place intermediate cache breakpoints.

### Proposed Solution

Structure history as a JSON array of turns:

```json
{
  "llm_history": [
    {
      "timestamp": "Mon Jul 15 10:30:00 2025",
      "user": "What is this?",
      "assistant": "Here's the explanation..."
    },
    {
      "timestamp": "Mon Jul 15 10:35:00 2025",
      "user": "Refactor it",
      "assistant": "Here's the refactored version..."
    }
  ],
  ...
}
```

### Implementation in vim-llm-assistant

**File**: `autoload/llm.vim` — modify how history is gathered

Instead of:
```vim
let l:data.llm_history = join(getbufline(g:llm_scratch_bufnr, 1, '$'), "\n")
```

Parse the scratch buffer into turns:
```vim
function! llm#parse_history_turns() abort
  if !exists('g:llm_scratch_bufnr') || !bufexists(g:llm_scratch_bufnr)
    return []
  endif
  
  let l:lines = getbufline(g:llm_scratch_bufnr, 1, '$')
  let l:turns = []
  let l:current_turn = {}
  let l:collecting = ''  " 'prompt' or 'response'
  let l:content = []
  
  for l:line in l:lines
    if l:line =~# '^==== .* ====$'
      " Save previous turn if exists
      if !empty(l:current_turn)
        if !empty(l:content)
          let l:current_turn[l:collecting] = join(l:content, "\n")
        endif
        call add(l:turns, l:current_turn)
      endif
      " Start new turn
      let l:timestamp = matchstr(l:line, '^==== \zs.*\ze ====$')
      let l:current_turn = {'timestamp': l:timestamp}
      let l:content = []
      let l:collecting = ''
    elseif l:line =~# '^Prompt: '
      let l:current_turn.user = l:line[8:]  " strip 'Prompt: '
      let l:collecting = 'assistant'
      let l:content = []
    else
      call add(l:content, l:line)
    endif
  endfor
  
  " Don't forget last turn
  if !empty(l:current_turn)
    if !empty(l:content)
      let l:current_turn[l:collecting] = join(l:content, "\n")
    endif
    call add(l:turns, l:current_turn)
  endif
  
  return l:turns
endfunction
```

Then in `llm#run()`:
```vim
let l:data.llm_history = llm#parse_history_turns()
```

### What aichat Can Do With This

With structured turns, aichat can:
1. **Convert to proper multi-turn messages** — each turn becomes a user + assistant message pair
2. **Place intermediate cache breakpoints** — every N turns, add a `cache_control` marker
3. **Benefit from automatic cache breakpoint movement** — in multi-turn mode, the breakpoint naturally advances as conversation grows

### Cache Benefit

This is the **highest-impact change** because it enables proper multi-turn conversation caching where:
- Turn 1-5: cached from the first request
- Turn 6 added: reads turns 1-5 from cache, writes turn 6
- Turn 7 added: reads turns 1-6 from cache, writes turn 7
- Each subsequent request pays only for the NEW turn's tokens

---

## 4. Use cmd_extra Hook for Cache Mode Signaling

### Current Capability

The adapter already supports a `g:llm_adapter_cmd_extra` hook (`aichat.vim:138-145`) that can inject environment variables and CLI flags.

### Proposed Implementation

**File**: User's vimrc or a plugin configuration file

```vim
" Signal cache preferences to aichat via environment variables
function! LLMCacheExtra(json_filename, prompt, model) abort
  let l:extras = ''
  
  " Signal to aichat that cache_control markers should be applied
  let l:extras .= 'AICHAT_CACHE_MODE=explicit '
  
  " Signal the history length for breakpoint decisions
  if exists('g:llm_scratch_bufnr') && bufexists(g:llm_scratch_bufnr)
    let l:history_lines = len(getbufline(g:llm_scratch_bufnr, 1, '$'))
    let l:extras .= 'AICHAT_HISTORY_LINES=' . l:history_lines . ' '
  endif
  
  " Signal number of open buffers for breakpoint budgeting
  let l:buf_count = len(tabpagebuflist())
  let l:extras .= 'AICHAT_BUFFER_COUNT=' . l:buf_count . ' '
  
  " Signal if this is a continuation (not first message)
  if exists('g:llm_scratch_bufnr') && bufexists(g:llm_scratch_bufnr)
        \ && len(getbufline(g:llm_scratch_bufnr, 1, '$')) > 1
    let l:extras .= 'AICHAT_IS_CONTINUATION=1 '
  else
    let l:extras .= 'AICHAT_IS_CONTINUATION=0 '
  endif
  
  return l:extras
endfunction

let g:llm_adapter_cmd_extra = {'aichat': 'LLMCacheExtra'}
```

### What aichat Should Do With These Signals

| Env Variable | Purpose | aichat Behavior |
|-------------|---------|-----------------|
| `AICHAT_CACHE_MODE=explicit` | Use explicit breakpoints only | Don't use top-level automatic caching |
| `AICHAT_HISTORY_LINES=N` | History size | If N > 100, add intermediate breakpoint |
| `AICHAT_BUFFER_COUNT=N` | Context size | Affects breakpoint budgeting |
| `AICHAT_IS_CONTINUATION=1` | Multi-turn session | Ensure cache prefix stability |

---

## 5. Add Cache-Warming Command (`:LLMWarm`)

### Concept

Send a `max_tokens: 0` request that writes the cache without generating output. This is valuable when:
- User opens a new editing session with many buffers
- User loads a new set of file arguments
- User wants to ensure first real question has cache hits

### Implementation

**File**: `autoload/llm.vim` — new function

```vim
" Warm the Anthropic cache with current context (no output generated)
function! llm#warm_cache() abort
  " Build context exactly as llm#run() would, but with a dummy prompt
  " Signal to aichat to use max_tokens: 0
  
  " Reuse llm#run() context assembly logic...
  " (Could extract context assembly into llm#build_context() helper)
  
  let l:data = s:build_context_data('', [])  " no prompt, no files
  let l:data._cache_warm = 1  " Signal to aichat
  
  let l:json_data = llm#encode(l:data)
  let l:tempfile = tempname()
  call writefile(split(l:json_data, "\n"), l:tempfile)
  
  " Fire async with a simple acknowledgement callback
  call llm#process_async(l:tempfile, '', '', {output -> echom '[LLM] Cache warmed (' . len(output) . ' bytes response)'})
endfunction
```

**File**: `plugin/llm.vim` — add command
```vim
command! LLMWarm call llm#warm_cache()
```

### aichat Implementation Required

aichat must detect `_cache_warm: true` in the JSON (or `AICHAT_CACHE_WARM=1` env var) and:
1. Set `max_tokens: 0` in the API request
2. Still apply `cache_control` breakpoints on system/tools/content
3. Return only the `usage` object (no content generation)
4. Report cache_creation_input_tokens in output

### Pre-Warming Strategy

```vim
" Auto-warm when significant context changes
autocmd BufWinEnter * call timer_start(5000, {-> llm#warm_cache()})
```

**Caution**: Only warm when sufficient tokens are available (>1024 for Sonnet). Don't warm on every trivial change — use debouncing.

---

## 6. Snippet System as Cache Stabilizer

### Already Implemented (No Changes Needed)

The snippet system (`llm#get_buffer_content()` at `llm.vim:210-250`) already provides cache stability benefits:

- **Replaces volatile full-buffer content** with specific stable line ranges
- **Reduces total token count** — smaller payloads are cheaper
- **Focused snippets change less** than full buffers (editing line 300 doesn't invalidate a snippet from lines 1-50)

### Recommended User Practice

For maximum cache efficiency, use snippets for large files:
```
:LLMSnip    " Select relevant portion via visual mode
:LLM what does this do?
```

This ensures only the targeted code section enters the context, keeping the overall payload more stable between requests.

### Potential Enhancement: Auto-Snippet Mode

```vim
" Automatically use snippet mode for buffers larger than N lines
let g:llm_auto_snippet_threshold = 200

" In llm#get_buffer_content(), if buffer > threshold and no explicit snippets:
" Only include lines within ±50 of cursor position
```

This would prevent large open files from bloating the context AND improve cache stability by limiting how much buffer content can change between requests.

---

## 7. Buffer Content Fingerprinting

### Concept

Track content hashes between requests to help aichat make cache decisions. If buffer content hasn't changed since last request, aichat knows it can place a cache breakpoint confidently.

### Implementation

**File**: `autoload/llm.vim`

```vim
" Track content fingerprints across requests
let s:last_content_fingerprints = {}

function! llm#compute_fingerprints(data) abort
  let l:fingerprints = {}
  
  " Hash the history (should only grow)
  if has_key(a:data, 'llm_history')
    let l:fingerprints.llm_history = sha256(a:data.llm_history)
  endif
  
  " Hash each buffer
  if has_key(a:data, 'buffers')
    let l:fingerprints.buffers = sha256(json_encode(a:data.buffers))
  endif
  
  " Hash active buffer
  if has_key(a:data, 'active_buffer')
    let l:fingerprints.active_buffer = sha256(json_encode(a:data.active_buffer))
  endif
  
  return l:fingerprints
endfunction

function! llm#get_stability_report(fingerprints) abort
  let l:report = {}
  
  for [l:key, l:hash] in items(a:fingerprints)
    if has_key(s:last_content_fingerprints, l:key)
      let l:report[l:key] = (l:hash ==# s:last_content_fingerprints[l:key]) ? 'stable' : 'changed'
    else
      let l:report[l:key] = 'new'
    endif
  endfor
  
  " Update stored fingerprints
  let s:last_content_fingerprints = a:fingerprints
  return l:report
endfunction
```

Then include in the JSON:
```json
{
  ...,
  "_stability": {
    "llm_history": "stable",
    "buffers": "stable",
    "active_buffer": "changed"
  }
}
```

### aichat Behavior

When aichat sees `_stability` metadata:
- Fields marked "stable" → safe to include before a cache breakpoint
- Fields marked "changed" → must go after the last breakpoint
- Fields marked "new" → no prior cache exists for this content

This enables aichat to make the **optimal cache breakpoint decision per-request** rather than using a static heuristic.

---

## 8. Proper Multi-Turn Conversation Mode (Highest Impact, Highest Effort)

### The Vision

Instead of treating the conversation as a flat text blob, restructure the plugin to maintain proper turn-by-turn conversation state that maps directly to Anthropic's API message format.

### Current Architecture
```
[LLM-Scratch] buffer (flat text) → single JSON field → single text block in API
```

### Proposed Architecture
```
[LLM-Scratch] buffer (display) ←→ internal turn array → structured JSON → multi-turn API messages
```

### Implementation Sketch

**File**: `autoload/llm.vim` — add conversation state management

```vim
" Internal state: conversation turns (separate from display buffer)
let s:conversation_turns = []

" After each LLM response, record the turn
function! llm#record_turn(prompt, response) abort
  call add(s:conversation_turns, {
        \ 'user': a:prompt,
        \ 'assistant': a:response,
        \ 'timestamp': strftime('%c'),
        \ })
endfunction

" Build context for aichat with proper turn separation
function! llm#build_multi_turn_context(prompt, files) abort
  let l:data = {}
  
  " Conversation history as structured turns
  let l:data.conversation = s:conversation_turns
  
  " Current context (buffers, files — NOT part of conversation)
  let l:data.context = {
        \ 'buffers': l:buffers_data,
        \ 'active_buffer': l:active_data,
        \ 'file_arguments': a:files,
        \ }
  
  " Current request (dynamic)
  let l:data.current_request = {
        \ 'prompt': a:prompt,
        \ 'cursor_line': line('.'),
        \ 'cursor_col': col('.'),
        \ }
  
  return l:data
endfunction
```

### What aichat Does With Structured Turns

```json
// API Request built by aichat:
{
  "system": [
    {"type": "text", "text": "role content...", "cache_control": {"type": "ephemeral"}}
  ],
  "tools": [..., "cache_control": {"type": "ephemeral"}],
  "messages": [
    // Turn 1 (from conversation array)
    {"role": "user", "content": "What is this?"},
    {"role": "assistant", "content": "It's a function that..."},
    // Turn 2
    {"role": "user", "content": "Refactor it"},
    {"role": "assistant", "content": "Here's the refactored..."},
    // Current request (new)
    {"role": "user", "content": [
      {"type": "text", "text": "Context: buffers...\n\nQuestion: Can you add tests?"}
    ]}
  ],
  "cache_control": {"type": "ephemeral"}  // Automatic mode handles the rest
}
```

### Cache Behavior With Multi-Turn

With automatic caching enabled:
- Request N: Everything up to the last message is cached
- Request N+1: Reads the entire prefix (turns 1 through N) from cache; only writes the new turn
- **Savings compound**: By turn 10, you're caching 90% of 9 previous turns' tokens

### Migration Path

1. **Phase 1**: Keep flat buffer display but maintain internal turn array in parallel
2. **Phase 2**: Pass both formats to aichat (legacy field + structured turns)
3. **Phase 3**: aichat uses structured turns for API calls, ignores flat history
4. **Phase 4**: Remove flat history from JSON payload

### Backward Compatibility

The `[LLM-Scratch]` buffer remains as a human-readable display. The structured turn data exists purely for API communication. Session save/load already captures history — it just needs to also save the structured array.

---

## 9. Content Ordering Optimization

### Current Order (Already Good)

```
llm_history → buffers → active_buffer → file_arguments → prompt → cursor_line → cursor_col
```

### Recommended Refinement

Consider whether `active_buffer` should move AFTER `file_arguments`:

```
llm_history → buffers → file_arguments → active_buffer → prompt → cursor_line → cursor_col
```

**Rationale**: `file_arguments` is explicitly attached and typically stable for an entire question sequence. `active_buffer` changes whenever the user edits. Moving the more stable content before the less stable content extends the cacheable prefix.

### Implementation

**File**: `autoload/llm.vim:37-44` — reorder `l:ordered_keys`:

```vim
let l:ordered_keys = [
      \ 'llm_history',      " 1. Large, stable prefix (earlier turns never change)
      \ 'buffers',          " 2. Stable across requests in same session
      \ 'file_arguments',   " 3. Stable per session (moved before active_buffer)
      \ 'active_buffer',    " 4. Semi-stable — changes with edits
      \ 'prompt',           " 5. Changes per request
      \ 'cursor_line',      " 6. Changes constantly (small)
      \ 'cursor_col',       " 7. Changes constantly (small)
      \ ]
```

**Impact**: Marginal improvement — moves ~0-60K tokens of file content into the stable prefix zone. Only helps if active_buffer changes between requests while file_arguments don't.

---

## 10. Cache Metrics Reporting

### Concept

vim-llm-assistant should surface cache performance to the user so they can verify caching is working and tune their workflow.

### Implementation

**File**: `autoload/llm/adapters/aichat.vim` — parse aichat output for cache metrics

If aichat reports cache usage (after implementing cache metric tracking in the Rust code):

```vim
" In s:on_job_complete(), parse metrics from output/response
function! s:extract_cache_metrics(output) abort
  " Look for cache metrics in aichat's stderr or structured output
  let l:metrics = {}
  for l:line in split(a:output, "\n")
    if l:line =~# 'cache_read_input_tokens'
      let l:metrics.cache_read = matchstr(l:line, '\d\+')
    endif
    if l:line =~# 'cache_creation_input_tokens'
      let l:metrics.cache_write = matchstr(l:line, '\d\+')
    endif
  endfor
  return l:metrics
endfunction
```

**Display**:
```vim
" After completion, show cache status
echom '[LLM] Complete! Cache: ' . l:metrics.cache_read . ' read, ' . l:metrics.cache_write . ' written'
```

---

## Implementation Priority Roadmap

### Phase 1: Quick Wins (1-2 hours each, vim-llm-assistant side)

1. **Reorder `file_arguments` before `active_buffer`** — 5-minute change in `llm.vim:37-44`
2. **Add `_cache_hints` metadata** to JSON payload — trivial addition in `llm#run()`
3. **Use cmd_extra hook** to pass `AICHAT_CACHE_MODE` and `AICHAT_IS_CONTINUATION` env vars
4. **Move cursor_line/cursor_col to env vars** — stop including them in JSON

### Phase 2: Medium Effort (aichat changes needed)

5. **Parse structured history turns** — new `llm#parse_history_turns()` function
6. **Cache-warming command** — `:LLMWarm` with `_cache_warm` signaling
7. **Buffer fingerprinting** — track stability between requests

### Phase 3: Architecture Evolution (requires both sides)

8. **Multi-turn conversation mode** — proper user/assistant turn array
9. **Split JSON into stable/dynamic files** — two `--file` arguments
10. **Cache metrics display** — surface Anthropic's usage data to user

---

## Key Insight: The Coordination Problem

The fundamental challenge is that vim-llm-assistant **knows** which content is stable vs. dynamic, but **cannot directly control** how aichat structures the API request. The implementation path requires:

1. **Protocol design**: Define how vim-llm-assistant communicates content boundaries to aichat
2. **aichat support**: aichat must parse these signals and map them to `cache_control` markers
3. **Incremental delivery**: Each phase is independently valuable; don't require the full architecture to see improvements

The single most impactful change that works with minimal aichat modification is **moving dynamic data out of the JSON file entirely** (Opportunity #2). If `prompt`, `cursor_line`, and `cursor_col` are passed via CLI args and env vars instead of in the JSON, then the JSON file content changes ONLY when actual context changes — enabling aichat's existing caching logic (which marks the last user message) to be far more effective.

---

## Appendix: Key File References

| File | Lines | Purpose |
|------|-------|---------|
| `autoload/llm.vim:32-70` | `llm#encode()` — deterministic key ordering |
| `autoload/llm.vim:525-560` | Context assembly in `llm#run()` |
| `autoload/llm.vim:601-635` | `OnLLMComplete` — how history is appended |
| `autoload/llm.vim:210-250` | Snippet system (`llm#get_buffer_content()`) |
| `autoload/llm/adapters/aichat.vim:138-145` | `cmd_extra` hook |
| `autoload/llm/adapters/aichat.vim:161-170` | Command construction |
| `plugin/llm.vim:15-20` | Default settings (model, role) |
