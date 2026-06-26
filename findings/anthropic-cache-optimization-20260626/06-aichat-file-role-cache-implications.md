# aichat's --file Flag and Role System: Cache Implications

## Executive Summary

The `--file` flag and role system in aichat create **massive static prefixes** that are ideal
prompt cache candidates. The default-vim-role.md alone is 39KB (~10,000 tokens), and tool
definitions add another 32KB (~8,000 tokens). Together, the system prompt + tools represent
~18,000+ tokens of completely static content that could be cached on every request — currently
only the system prompt is properly cached; tools are not.

---

## 1. Role System Architecture

### How Roles Are Loaded

```
CLI: aichat --role default-vim-role ...
                    │
                    ▼
config/mod.rs:876  use_role(name)
                    │
                    ▼
config/mod.rs:919  retrieve_role(name)
                    │  reads: ~/.config/aichat/roles/{name}.md
                    ▼
config/role.rs:62  Role::new(name, content)
                    │  parses YAML frontmatter + prompt text
                    ▼
                  Role { name, prompt, model_id, temperature, use_tools, ... }
```

**Key file**: `config/role.rs:62-99` — Role parsing with YAML metadata extraction

### Role File Structure (YAML Frontmatter)

```markdown
---
use_tools: code_assistant
---

# Intelligent Coding Assistant
[... 39KB of system prompt content ...]
```

The frontmatter configures:
- `model` — override default model
- `temperature` — sampling temperature
- `top_p` — nucleus sampling
- `use_tools` — tool group or comma-separated list (e.g., `code_assistant`, `all`)

### Role → System Message Flow

```
role.rs:183  build_messages(input)
                │
                ├─ Parses structured prompt with ### INPUT: / ### OUTPUT: markers
                │  into: system message + few-shot examples
                │
                ├─ Creates Message(System, role_prompt)  ← CACHE CANDIDATE
                ├─ Creates Message(User, example_input) × N
                ├─ Creates Message(Assistant, example_output) × N
                └─ Creates Message(User, input_content)  ← dynamic user input
```

**Critical insight**: The role prompt becomes the **System message** in the Anthropic API
request. This is the HIGHEST-priority cache prefix (system → tools → messages hierarchy).

### Role Sizes (Current Configuration)

| Role File | Size (bytes) | Est. Tokens | Static? | Cache Value |
|-----------|-------------|-------------|---------|-------------|
| default-vim-role.md | 39,245 | ~10,000 | ✅ 100% | VERY HIGH |
| ralph-role.md | 12,158 | ~3,000 | ✅ 100% | HIGH |
| subagent-role.md | 28,679 | ~7,200 | ✅ 100% | HIGH |
| therapist.md | 8,317 | ~2,100 | ✅ 100% | MEDIUM |
| atlassian.md | 2,525 | ~630 | ✅ 100% | LOW (below min) |

**Minimum cache threshold**: 1,024 tokens (Claude Sonnet/Opus), 2,048 (Haiku).
All major roles exceed the minimum threshold individually.

---

## 2. --file Flag Architecture

### CLI Definition

```rust
// cli.rs:56-57
/// Include files, directories, or URLs
#[clap(short = 'f', long, value_name = "FILE")]
pub file: Vec<String>,
```

Multiple `-f` flags can be specified: `-f file1 -f file2 -f context.json`

### Input Assembly Flow

```
main.rs:327  create_input(config, text, file, abort_signal)
                │
                ├─ if file.is_empty() → Input::from_str(text)
                │
                └─ else → Input::from_files_with_spinner(text, files)
                              │
                              ▼
input.rs:56   Input::from_files(config, raw_text, paths, role)
                              │
                              ├─ resolve_paths(paths) → local, remote, cmd, protocol
                              ├─ load_documents() → Vec<(kind, path, contents)>
                              │
                              ▼  INTENTIONAL CACHE-FRIENDLY ORDERING:
                              │
                              ├─ 1. Documents FIRST (static content)
                              │     Comment at line 77: "Documents first (static content — cache-friendly prefix)"
                              │
                              ├─ 2. Last reply (semi-static per session turn)
                              │     Only if %% path is used
                              │
                              └─ 3. Prompt text LAST (dynamic, changes every request)
```

**Key evidence**: `input.rs:77` has an explicit comment:
```rust
// Documents first (static content — cache-friendly prefix)
```

The developer already designed this ordering for caching!

### How vim-llm-assistant Uses --file

The actual command constructed by `aichat.vim:168`:
```bash
aichat --role default-vim-role --model <model> \
    -f /path/to/file1 -f /path/to/file2 \    # from file_arguments (stable)
    --file /tmp/json_context.json             # main context payload (mixed)
    [-- "user prompt"]                        # optional trailing prompt
```

Multiple `-f` flags appear BEFORE `--file` for the main context. In `from_files()`, all
file paths are loaded together. The order they appear in the array determines their position
in the concatenated user message text.

---

## 3. Context Payload Structure (vim-llm-assistant)

### JSON Context Assembly (`llm.vim:618-649`)

```vim
" Assemble the data dictionary in cache-optimized order:
"   stable (large) → variable (small) for maximum prefix cache hits.
"   1. llm_history    — large, stable prefix (earlier turns never change)
"   2. buffers[]      — stable across requests in same session
"   3. active_buffer  — stable most of the time
"   4. file_arguments — stable per session
"   5. prompt         — changes per request
"   6. cursor_line    — changes constantly (small)
"   7. cursor_col     — changes constantly (small)
```

### Deterministic JSON Encoding (`llm.vim:32-70`)

```vim
function! llm#encode(obj) abort
  let l:ordered_keys = [
        \ 'llm_history',
        \ 'buffers',
        \ 'active_buffer',
        \ 'file_arguments',
        \ 'prompt',
        \ 'cursor_line',
        \ 'cursor_col',
        \ ]
  " ... deterministic serialization ...
endfunction
```

This ensures the same key ordering every time, which is critical for byte-level cache
prefix matching.

---

## 4. Full API Request Structure (Cache Perspective)

When vim-llm-assistant calls aichat with default-vim-role:

```
┌─────────────────────────────────────────────────────────────────────┐
│ ANTHROPIC API REQUEST BODY                                          │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│ 1. system: [                                ← CACHED ✓             │
│      { type: "text",                         ~10,000 tokens         │
│        text: "<default-vim-role content>",   39KB role prompt        │
│        cache_control: {type: "ephemeral"} }                         │
│    ]                                                                │
│                                                                     │
│ 2. tools: [                                 ← NOT CACHED ✗          │
│      { name: "audio_from_yt", ... },         ~8,000 tokens          │
│      { name: "code_navigator", ... },        32KB JSON schemas       │
│      { name: "fs_cat", ... },                                       │
│      ... (25 tools total)                                           │
│    ]                                                                │
│                                                                     │
│ 3. messages: [                                                      │
│      { role: "user",                                                │
│        content: [                           ← PARTIAL cache via      │
│          {                                    last-msg breakpoint    │
│            type: "text",                                            │
│            text: "                                                  │
│              <-f file1 contents>             stable per session      │
│              <-f file2 contents>             stable per session      │
│              ============ FILE: /path ===                            │
│              <JSON context:>                                        │
│                llm_history: ...              ← grows monotonically   │
│                buffers: [...]               ← semi-stable            │
│                active_buffer: {...}         ← changes often          │
│                prompt: '...'               ← changes every request   │
│                cursor_line: N              ← changes constantly      │
│                cursor_col: M              ← changes constantly       │
│            ",                                                       │
│            cache_control: {type: "ephemeral"}  ← on LAST message    │
│          }                                                          │
│        ]                                                            │
│      }                                                              │
│    ]                                                                │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Token Budget Breakdown (Typical Request)

| Component | Estimated Tokens | Stability | Cached? |
|-----------|-----------------|-----------|---------|
| System (role prompt) | 10,000-15,000 | 100% static | ✅ Yes |
| Tools (function schemas) | 6,000-8,000 | 100% static | ❌ NO |
| File arguments (-f) | 0-5,000 | Per-session stable | ❌ No (mixed in user msg) |
| LLM history | 1,000-50,000+ | Prefix stable | ❌ No (mixed in user msg) |
| Buffers | 500-10,000 | Semi-stable | ❌ No (mixed in user msg) |
| Active buffer | 100-5,000 | Changes often | ❌ No |
| Prompt + cursor | 10-500 | Changes every request | ❌ No |

**Total cacheable but NOT cached**: ~6,000-8,000 tokens (tools alone)

---

## 5. Cache Hierarchy & Prefix Model

Anthropic's cache uses byte-level prefix matching:

```
Cache prefix = system + tools + messages (in order)
```

Any change at byte N invalidates everything from N onward. This means:

1. **System prompt change → everything invalidated** (model switch, role edit)
2. **Tool definition change → messages invalidated** (functions.json update)
3. **Message change → only subsequent messages invalidated**

### Current Behavior Analysis

| Scenario | Cache Behavior |
|----------|---------------|
| Same role, same tools, new prompt | System cached ✓, tools NOT cached, messages NOT cached |
| Same role, same tools, conversation turn 2 | System cached ✓, tools NOT cached, msg prefix NOT cached |
| Different role | Everything misses |
| Same role, tool list changes | System still cached ✓, tools miss, messages miss |

### Optimal Behavior (After Fix)

| Scenario | Cache Behavior |
|----------|---------------|
| Same role, same tools, new prompt | System+Tools cached ✓ (~18K tokens saved) |
| Multi-turn conversation | System+Tools+Prefix cached ✓ |
| Same session, different buffer | System+Tools+History cached ✓ |

---

## 6. The `use_tools` Directive

### How Tools Get Selected

```rust
// config/mod.rs:1654-1698
pub fn select_functions(&self, role: &Role) -> Option<Vec<FunctionDeclaration>> {
    if self.function_calling {
        if let Some(use_tools) = role.use_tools() {
            // "all" → all tools
            // "code_assistant" → lookup in mapping_tools
            // "tool1,tool2" → specific tools
        }
    }
}
```

The `mapping_tools` config maps group names to tool lists:
```yaml
# In config.yaml
mapping_tools:
  code_assistant: "fs_cat,fs_write,recursive_grep,..."
```

### Tool Stability Analysis

- Tools are defined in `functions.json` (32KB) — changes only on tool code updates
- Tool selection is determined by role's `use_tools` field — fixed per role
- **Tools are 100% static for a given role** — perfect cache candidate

### Why Tools Aren't Cached (The Bug)

In `claude.rs:294-303`:
```rust
if let Some(functions) = functions {
    body["tools"] = functions.iter().map(|v| {
        json!({
            "name": v.name,
            "description": v.description,
            "input_schema": v.parameters,
        })
    }).collect();
}
// No cache_control added to tools!
```

Compare with Bedrock (`bedrock.rs:592-619`) which correctly adds cachePoint to tools.

---

## 7. Role + File Interaction Patterns

### Pattern A: Single-Shot Command (no session)

```bash
aichat --role default-vim-role -f context.json -- "explain this code"
```

**Flow**: Role → System msg (cached) → Tools (NOT cached) → User msg [file+prompt]

**Cache benefit**: Only system (10K tokens). Missing 8K+ from tools.

### Pattern B: Session Mode (multi-turn)

```bash
aichat --role default-vim-role --session my-session -f context.json
```

**Flow**: Role → System → Tools → Session history + new input

**Cache benefit**: System (10K) + all previous turns. But tools still not cached.

### Pattern C: vim-llm-assistant Standard Call

```bash
aichat --role default-vim-role --model claude-sonnet-4-20250514 \
    -f /path/to/extra/file \
    --file /tmp/vim_context_XXXX.json \
    -- "user prompt"
```

**Flow**: Role → System → Tools → User msg [extra_files + json_context + prompt]

**Stability layers**:
1. System: 100% stable (role never changes mid-session)
2. Tools: 100% stable (same role = same tools)
3. Extra files (-f): Stable per editing session
4. JSON prefix (llm_history): Grows monotonically, prefix stable
5. Current state (buffers, prompt): Changes each request

---

## 8. Specific Cache Optimization Opportunities

### 8.1. Add cache_control to Tools (HIGH IMPACT)

**Location**: `claude.rs:294-303`
**Tokens saved**: ~6,000-8,000 per request
**Implementation**: Add `cache_control: {"type": "ephemeral"}` to last tool in array

```rust
// After building tools array:
if let Some(tools_array) = body["tools"].as_array_mut() {
    if let Some(last_tool) = tools_array.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}
```

### 8.2. Remove Invalid Top-Level cache_control (CORRECTNESS)

**Location**: `claude.rs:311`
**Issue**: `body["cache_control"]` is NOT a valid Anthropic API field — it's silently ignored
**Fix**: Remove it entirely (block-level markers are already correctly placed)

### 8.3. Consider Caching file_arguments Separately (MEDIUM IMPACT)

Currently, all `-f` file contents are concatenated into a single user message. If
file_arguments were treated as separate content blocks with their own cache_control,
they could be independently cached.

**Approach**: When building the user message from multiple files, emit each file as a
separate content block in a multi-part message array, with cache_control on the last
stable file block.

### 8.4. Role Size Awareness (DESIGN INSIGHT)

The default-vim-role.md at 39KB (~10K tokens) is a significant cache investment. Each
cache write costs 25% MORE than a regular read. However, once cached, subsequent reads
are 90% cheaper. For the vim workflow where the same role is used repeatedly:

- **Cost of first request**: 10,000 × 1.25 = 12,500 equivalent tokens (cache write)
- **Cost of subsequent requests**: 10,000 × 0.10 = 1,000 equivalent tokens (cache read)
- **Break-even**: After ~1.4 requests, caching the role pays for itself

With a 5-minute TTL (or 1-hour for Claude 4+), interactive coding sessions easily exceed
this threshold.

### 8.5. Prelude System for Auto-Role

```rust
// config/mod.rs:1614 - apply_prelude()
// cmd_prelude: "role:default-vim-role" → auto-sets role without --role flag
```

If users configure `cmd_prelude`, the role is loaded automatically, ensuring consistent
caching even without explicit `--role` flags.

---

## 9. Token Count Analysis

### Per-Request Cache Savings (After Optimization)

| Fix | Tokens Cached | Savings/Request (at $3/M input) |
|-----|--------------|-------------------------------|
| System (already done) | ~10,000 | $0.027 saved |
| + Tools | ~8,000 | $0.022 saved |
| **Total static prefix** | **~18,000** | **$0.049 saved per request** |

Over a typical coding session (50-100 requests in 5 min window):
- Current: 10K cached → saves $1.35-$2.70
- Optimized: 18K cached → saves $2.45-$4.90

### Conversation Scaling

In multi-turn conversations, the conversation prefix grows. With proper breakpoint
management:

| Turn | Cached Prefix | New Tokens | Cache Benefit |
|------|--------------|------------|--------------|
| 1 | 18K (system+tools) | ~5K (user msg) | 78% cached |
| 2 | 23K (sys+tools+turn1) | ~5K | 82% cached |
| 5 | 38K (sys+tools+turns1-4) | ~5K | 88% cached |
| 10 | 63K (prefix grows) | ~5K | 93% cached |

---

## 10. Summary: Cache Architecture Map

```
                    FULLY STATIC (cache breakpoint 1)
                    ┌─────────────────────────────────────┐
                    │  System: Role prompt (10K tokens)    │ ← cached ✓
                    │  Tools: Function schemas (8K tokens) │ ← NOT cached ✗
                    └─────────────────────────────────────┘
                                     │
                    SEMI-STATIC (cache breakpoint 2 - optional)
                    ┌─────────────────────────────────────┐
                    │  file_arguments: Extra files (-f)    │ ← in user msg, not separate
                    │  llm_history prefix: Prior turns     │ ← grows, prefix stable
                    └─────────────────────────────────────┘
                                     │
                    DYNAMIC (no caching)
                    ┌─────────────────────────────────────┐
                    │  active_buffer, prompt, cursor       │
                    └─────────────────────────────────────┘
```

**Immediate wins**:
1. Cache tools (fix one line in claude.rs) → saves 8K tokens/request
2. Remove invalid top-level cache_control → correctness fix
3. Consider tools cachePoint like Bedrock → proper implementation

**Design wins** (require architecture changes):
4. Split file content into separate cached blocks
5. Add secondary breakpoint at llm_history boundary
6. Explore "automatic" caching mode (top-level flag on request instead of per-block)
