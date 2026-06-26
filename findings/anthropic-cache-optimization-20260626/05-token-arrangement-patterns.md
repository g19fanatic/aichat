# Token Arrangement Patterns for Cache Hits

**Task**: Research specific token arrangement patterns for cache hits
**Date**: 2025-07-16
**Sources**:
- Anthropic Official Docs: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
- Anthropic Cookbook: https://platform.claude.com/cookbook/misc-prompt-caching
- Tool Use with Prompt Caching: https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/tool-use-with-prompt-caching

---

## Executive Summary

Cache hits require **byte-for-byte identical content** from the start of the request up to the cache breakpoint. The fundamental strategy is to arrange content from **most stable to most dynamic**, placing cache breakpoints at the boundary between static and changing content. This document provides specific, implementable patterns for each cacheable section type.

---

## 1. The Prefix Model (Foundation Concept)

### How the Cache Actually Works

The cache is a **prefix cache** — it stores computed KV (key-value) representations for a contiguous sequence of tokens from the beginning of the processed input. The "processed input" follows this strict order:

```
┌─────────────────────────────────────────────────────────────┐
│ CACHE PREFIX (everything before last breakpoint)            │
│                                                             │
│  1. tools[]         ← First in prefix                      │
│  2. system[]        ← Second in prefix                     │
│  3. messages[]      ← Third in prefix                      │
│     ├── user(1)                                            │
│     ├── assistant(1)                                       │
│     ├── user(2)                                            │
│     ├── ...                                                │
│     └── last_message_before_breakpoint ◀ BREAKPOINT        │
│                                                             │
├─────────────────────────────────────────────────────────────┤
│ UNCACHED SUFFIX (everything after last breakpoint)          │
│                                                             │
│     └── current_user_message (dynamic)                     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### The Golden Rule

> Place `cache_control` on the **last block whose entire prefix is identical across the requests you want to share a cache**.

If you place the breakpoint on content that changes every request:
- A fresh cache **write** occurs every request (1.25x cost)
- **No** cache reads ever occur
- The lookback mechanism cannot help — it only finds entries that **prior requests wrote at their own breakpoints**

---

## 2. System Prompt Caching Patterns

### Pattern 2a: Simple System Prompt (Single Block)

The most basic caching pattern — cache your entire system prompt as a single block:

```json
{
  "system": [
    {
      "type": "text",
      "text": "You are an expert software engineer with deep knowledge...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

**Requirements**:
- System prompt MUST be in **array format** (not plain string) to attach `cache_control`
- Must meet minimum token threshold (1,024 for Sonnet 4.5/4.6)
- Content must be byte-for-byte identical across requests

### Pattern 2b: Multi-Block System Prompt (Segmented by Change Frequency)

When parts of your system prompt change at different rates:

```json
{
  "system": [
    {
      "type": "text",
      "text": "PERMANENT INSTRUCTIONS: You are a coding assistant...\n\nPERMANENT RULES: Always provide...",
      "cache_control": {"type": "ephemeral", "ttl": "1h"}
    },
    {
      "type": "text",
      "text": "SESSION CONTEXT: The user is working on project X...\nCurrent file: src/main.rs\nRecent changes: ...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

**Key principle**: Longer TTL must come BEFORE shorter TTL in the prefix.

### Pattern 2c: System Prompt + Injected Static Files

For coding assistants that inject role files, skills, or reference documents:

```json
{
  "system": [
    {
      "type": "text",
      "text": "# Role Definition\nYou are a code assistant...\n\n# Skills\n@skill-1 content...\n@skill-2 content...\n\n# Static Reference\nProject architecture: ...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

**Critical for aichat/vim-llm-assistant**: The `--role` and `--file` flags inject large static content (10K+ tokens) that is identical across many requests. This is the **primary caching opportunity**.

### Pattern 2d: System Prompt with Dynamic Preamble REMOVED

**Anti-pattern** (DO NOT DO):
```json
{
  "system": [
    {
      "type": "text",
      "text": "Current time: 2025-07-16T10:30:00Z\nYou are a helpful assistant..."
    }
  ]
}
```

**Correct pattern**: Move timestamps, request IDs, or any per-request data OUT of the system prompt. Place them in the user message instead:

```json
{
  "system": [
    {
      "type": "text",
      "text": "You are a helpful assistant...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [
    {
      "role": "user",
      "content": "Current time: 2025-07-16T10:30:00Z\n\nMy question is..."
    }
  ]
}
```

---

## 3. Tool Definition Caching Patterns

### Pattern 3a: Basic Tool Array Caching

Place `cache_control` on the **last tool** in the array:

```json
{
  "tools": [
    {"name": "tool_a", "description": "...", "input_schema": {...}},
    {"name": "tool_b", "description": "...", "input_schema": {...}},
    {
      "name": "tool_c",
      "description": "...",
      "input_schema": {...},
      "cache_control": {"type": "ephemeral"}
    }
  ]
}
```

This caches ALL tool definitions (the entire tools prefix). Since tools are first in the hierarchy, cached tools remain valid even when system/messages change.

### Pattern 3b: Deterministic Tool Ordering (CRITICAL)

```json
{
  "tools": [
    // ALWAYS in alphabetical order by name (or any fixed, deterministic order)
    {"name": "code_navigator", ...},
    {"name": "fetch_url", ...},
    {"name": "fs_cat", ...},
    {"name": "fs_write", ...},
    {"name": "safe_script_executor", ..., "cache_control": {"type": "ephemeral"}}
  ]
}
```

**Critical requirements for cache stability**:
1. **Fixed order**: Sort tools alphabetically or by any deterministic scheme
2. **Deterministic JSON schemas**: Sort all JSON object keys when serializing `input_schema`
3. **No dynamic tools**: Don't add/remove tools between requests (use `defer_loading` instead)
4. **Consistent descriptions**: Don't dynamically modify tool descriptions

**Languages with hash-map randomization** (Go, Swift, some Ruby versions): Sort keys explicitly before serialization.

### Pattern 3c: Deferred Tool Loading (Dynamic Without Breaking Cache)

For large tool sets where you don't want all definitions in the prefix:

```json
{
  "tools": [
    {"name": "core_tool_1", ...},
    {"name": "core_tool_2", ..., "cache_control": {"type": "ephemeral"}},
    {
      "name": "optional_tool_set",
      "type": "tool_search",
      "defer_loading": true,
      "description": "Search for additional tools"
    }
  ]
}
```

Deferred tools discovered via `tool_search` appear as `tool_reference` blocks inline in conversation history — they do NOT break the prefix cache. The prefix hash remains stable.

### Pattern 3d: Tools + System Combined (Maximum Cache Stability)

```json
{
  "tools": [
    {"name": "tool_1", ...},
    {"name": "tool_2", ..., "cache_control": {"type": "ephemeral"}}
  ],
  "system": [
    {
      "type": "text",
      "text": "Your large system prompt...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

This uses 2 of 4 available breakpoints:
- Breakpoint 1: After tools → tools cached independently
- Breakpoint 2: After system → system cached (includes tools prefix)

If only messages change, BOTH cache entries hit. If system changes but tools don't, at least the tools cache still hits.

---

## 4. Few-Shot Example Caching Patterns

### Pattern 4a: Examples in System Prompt

The most cache-friendly placement for few-shot examples:

```json
{
  "system": [
    {
      "type": "text",
      "text": "You are a code reviewer. Here are examples of good reviews:\n\nExample 1:\nInput: def foo(x): return x+1\nReview: Function name 'foo' is not descriptive...\n\nExample 2:\nInput: class UserManager...\nReview: Consider using dependency injection...\n\n[...20+ examples...]\n\nNow review the following code:",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [
    {"role": "user", "content": "[actual code to review]"}
  ]
}
```

**Key insight from Anthropic docs**: "With prompt caching you can get even better performance by including 20+ diverse examples of high quality answers." — The cost becomes viable when examples are cached (90% cheaper on subsequent requests).

### Pattern 4b: Examples as Early User Messages (Multi-Turn Format)

For examples that benefit from the multi-turn format:

```json
{
  "system": [{"type": "text", "text": "You are a code reviewer.", "cache_control": {"type": "ephemeral"}}],
  "messages": [
    {"role": "user", "content": "Review this: def foo(x): return x+1"},
    {"role": "assistant", "content": "Function name 'foo' is not descriptive. Consider 'increment'..."},
    {"role": "user", "content": "Review this: class UserManager..."},
    {"role": "assistant", "content": "Consider using dependency injection..."},
    // ... more examples ...
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Review this: [LAST EXAMPLE]",
          "cache_control": {"type": "ephemeral"}
        }
      ]
    },
    {"role": "assistant", "content": "[LAST EXAMPLE RESPONSE]"},
    {"role": "user", "content": "[ACTUAL USER INPUT - dynamic, not cached]"}
  ]
}
```

**Advantage**: The model sees examples in the same format as the actual task, improving performance.
**Requirement**: Earlier messages must be **byte-for-byte identical** — never edit them.

### Pattern 4c: Reference Document + Examples Combo

For document QA with examples:

```json
{
  "system": [
    {
      "type": "text",
      "text": "You answer questions about the following codebase:\n\n[FULL CODEBASE CONTENT HERE - 50K+ tokens]",
      "cache_control": {"type": "ephemeral", "ttl": "1h"}
    },
    {
      "type": "text",
      "text": "Example Q&A:\nQ: How does authentication work?\nA: The auth module uses JWT tokens stored in...\n\nQ: What's the database schema?\nA: The main tables are users, posts, and comments...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [
    {"role": "user", "content": "[USER'S ACTUAL QUESTION]"}
  ]
}
```

**Uses 2 breakpoints**: The codebase (rarely changes, 1h TTL) and examples (may change occasionally, 5-min TTL).

---

## 5. Conversation Prefix Caching Patterns

### Pattern 5a: Automatic Mode (Recommended for Multi-Turn)

The simplest and most robust pattern for growing conversations:

```json
{
  "cache_control": {"type": "ephemeral"},
  "system": "You are a helpful assistant.",
  "messages": [
    {"role": "user", "content": "First question"},
    {"role": "assistant", "content": "First answer"},
    {"role": "user", "content": "Second question"},
    {"role": "assistant", "content": "Second answer"},
    {"role": "user", "content": "Current question"}
  ]
}
```

The system automatically:
1. Places breakpoint on the last cacheable block (current user message)
2. On next request, reads everything up to the prior breakpoint from cache
3. Only processes the new delta (assistant response + new user message)

### Pattern 5b: Pinned System + Automatic Conversation

Combines explicit system caching with automatic conversation caching:

```json
{
  "cache_control": {"type": "ephemeral"},
  "system": [
    {
      "type": "text",
      "text": "Large system prompt with role, skills, tools context...",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [...]
}
```

**Uses 2 of 4 breakpoints** (1 explicit + 1 automatic).

**Why this matters**: Even if the conversation grows beyond the 20-block lookback window, the system prompt's explicit cache entry is always found (it never moves).

### Pattern 5c: Append-Only History (Critical Rule)

```
Request N:
  messages = [msg1, msg2, ..., msgN-1, NEW_USER_MSG]
                ↑ These must be BYTE-FOR-BYTE IDENTICAL to Request N-1

Request N+1:
  messages = [msg1, msg2, ..., msgN-1, msgN, assistant_response, NEW_USER_MSG]
                                        ↑ msgN = exact copy of what was sent last time
                                               ↑ assistant_response = verbatim from API response
```

**Rules for cache stability**:
1. Never edit earlier messages (no typo fixes, no reformatting)
2. Echo assistant responses VERBATIM (copy the exact content[] array from the response)
3. Echo tool_use and tool_result blocks VERBATIM
4. Never reorder messages
5. Never remove messages from the middle

### Pattern 5d: Long Conversation with Secondary Breakpoints

When conversations grow by 20+ blocks between turns (e.g., large tool results):

```json
{
  "system": [
    {"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}
  ],
  "messages": [
    // ... many earlier messages (blocks 1-50) ...
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Middle of conversation...",
          "cache_control": {"type": "ephemeral"}
        }
      ]
    },
    // ... blocks 52-75 (could be large tool results) ...
    {
      "role": "user",
      "content": [
        {
          "type": "text",
          "text": "Current question",
          "cache_control": {"type": "ephemeral"}
        }
      ]
    }
  ]
}
```

**Strategy**: Place a "stepping stone" breakpoint every ~15-18 blocks to ensure the lookback window always finds a prior write.

---

## 6. Multi-Breakpoint Budget Strategy

### The 4-Breakpoint Budget

You have exactly **4 cache breakpoints** per request. Use them wisely:

| Strategy | BP1 | BP2 | BP3 | BP4 |
|----------|-----|-----|-----|-----|
| **Simple** | Last tool | System prompt | — (auto) | — |
| **Full control** | Last tool | System prompt | Mid-conversation anchor | Last user msg |
| **Mixed TTL** | System (1h) | Context (5m) | — (auto) | — |
| **Long conversation** | System | Conversation midpoint | Near-end anchor | Last msg |

### Choosing Breakpoint Positions

Decision tree for where to place breakpoints:

```
1. Do you have tools? → BP on last tool (always first)
2. Do you have a system prompt >1K tokens? → BP on last system block
3. Is your conversation >20 blocks? → BP at a "stepping stone" position
4. Are you using automatic caching? → It takes one slot automatically
5. Remaining slots: Use for mid-conversation anchors
```

---

## 7. Content Block Arrangement Within Sections

### System Prompt Block Ordering

Within the `system` array, arrange blocks from most stable to least stable:

```json
{
  "system": [
    {
      "type": "text",
      "text": "CORE ROLE (never changes):\nYou are an expert...",
      "cache_control": {"type": "ephemeral", "ttl": "1h"}
    },
    {
      "type": "text",
      "text": "INJECTED FILES (changes per session):\n[file contents]..."
    },
    {
      "type": "text",
      "text": "SESSION CONTEXT (changes per conversation):\nProject: X\nBranch: main",
      "cache_control": {"type": "ephemeral"}
    }
  ]
}
```

### Message Content Block Ordering

Within a user message, place static content first:

```json
{
  "role": "user",
  "content": [
    {
      "type": "text",
      "text": "[Large static context like a file being discussed]"
    },
    {
      "type": "text",
      "text": "[The actual user question about that file]"
    }
  ]
}
```

**Note**: You can only put `cache_control` on content blocks within messages, not on the message envelope itself.

---

## 8. Specific Pattern Templates for Use Cases

### Template A: Coding Assistant (aichat/vim-llm-assistant)

```json
{
  "tools": [
    // 20+ tool definitions (~5K-10K tokens), deterministically ordered
    {"name": "code_navigator", ...},
    {"name": "fs_cat", ...},
    {"name": "fs_write", ...},
    {"name": "patch", ...},
    {"name": "safe_script_executor", ..., "cache_control": {"type": "ephemeral"}}
  ],
  "system": [
    {
      "type": "text",
      "text": "# Role Definition (from --role flag)\n[10K+ tokens of role instructions]\n\n# Skills (from --file flags)\n[5K+ tokens of loaded skills]\n\n# Project Context\n[Static project documentation]",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "cache_control": {"type": "ephemeral"},
  "messages": [
    // Growing conversation — automatic caching handles this
    {"role": "user", "content": "..."},
    {"role": "assistant", "content": "..."},
    {"role": "user", "content": "[current question]"}
  ]
}
```

**Expected savings**: With ~15K tokens of tools + 15K tokens of system prompt = 30K tokens cached at $0.30/MTok (read) instead of $3/MTok (uncached) = **90% savings on 30K tokens per request**.

### Template B: Document QA

```json
{
  "system": [
    {
      "type": "text",
      "text": "[Full document content - potentially 100K+ tokens]\n\nAnswer questions about the above document.",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "cache_control": {"type": "ephemeral"},
  "messages": [
    {"role": "user", "content": "What does section 3 say about...?"}
  ]
}
```

### Template C: Agentic Loop with Tool Use

```json
{
  "tools": [
    // Fixed tool set
    {..., "cache_control": {"type": "ephemeral"}}
  ],
  "system": [
    {
      "type": "text",
      "text": "You are an agent that...",
      "cache_control": {"type": "ephemeral"}}
  ],
  "cache_control": {"type": "ephemeral"},
  "messages": [
    // Each iteration adds:
    //   assistant: tool_use block
    //   user: tool_result block
    // Automatic caching handles the growing history
    {"role": "user", "content": "Initial task"},
    {"role": "assistant", "content": [{"type": "tool_use", ...}]},
    {"role": "user", "content": [{"type": "tool_result", ...}]},
    {"role": "assistant", "content": [{"type": "tool_use", ...}]},
    {"role": "user", "content": [{"type": "tool_result", ...}]},
    // ... continues growing
  ]
}
```

**Server tools note**: When using server tools (web search, code execution), the API automatically places a 5-min cache breakpoint on server tool results within the same request's agentic loop.

### Template D: Few-Shot Classifier

```json
{
  "system": [
    {
      "type": "text",
      "text": "Classify the following text into categories: [positive, negative, neutral]\n\nExamples:\n\nText: 'I love this product'\nCategory: positive\n\nText: 'It broke after one day'\nCategory: negative\n\n[... 20+ examples ...]\n\nText: 'It works as described'\nCategory: neutral\n\nNow classify the following:",
      "cache_control": {"type": "ephemeral"}
    }
  ],
  "messages": [
    {"role": "user", "content": "[text to classify]"}
  ]
}
```

---

## 9. Token Arrangement Anti-Patterns

### Anti-Pattern 1: Timestamp in System Prompt
```json
// ❌ WRONG — cache misses every request
{"system": [{"type": "text", "text": "Time: 2025-07-16T10:30:00Z\nYou are..."}]}

// ✅ CORRECT — timestamp moved to user message
{"system": [{"type": "text", "text": "You are...", "cache_control": {...}}],
 "messages": [{"role": "user", "content": "Time: 10:30. My question: ..."}]}
```

### Anti-Pattern 2: Dynamic Tool Descriptions
```json
// ❌ WRONG — tool description includes dynamic data
{"tools": [{"name": "search", "description": "Search. Last used: 5 min ago..."}]}

// ✅ CORRECT — static descriptions only
{"tools": [{"name": "search", "description": "Search for information."}]}
```

### Anti-Pattern 3: Non-Deterministic JSON Key Ordering
```json
// ❌ WRONG — keys may serialize in different order each time
{"input_schema": {"required": ["x"], "type": "object", "properties": {"x": {...}}}}

// ✅ CORRECT — always sorted keys
{"input_schema": {"properties": {"x": {...}}, "required": ["x"], "type": "object"}}
```

### Anti-Pattern 4: Editing Earlier Messages
```json
// ❌ WRONG — modifying user message from turn 1
messages[0] = {"role": "user", "content": "Fixed typo: What is Rust?"}

// ✅ CORRECT — never modify, only append
messages.push({"role": "user", "content": "New question..."})
```

### Anti-Pattern 5: Breakpoint on Dynamic Content
```json
// ❌ WRONG — breakpoint on content that changes every request
{"messages": [
  {"role": "user", "content": [
    {"type": "text", "text": "Large static context..."},
    {"type": "text", "text": "Dynamic question...", "cache_control": {"type": "ephemeral"}}
  ]}
]}
// The entire prefix INCLUDING the dynamic question is hashed.
// Since the question changes, the hash changes, and NO cache hit occurs.

// ✅ CORRECT — breakpoint on static content only
{"messages": [
  {"role": "user", "content": [
    {"type": "text", "text": "Large static context...", "cache_control": {"type": "ephemeral"}},
    {"type": "text", "text": "Dynamic question..."}
  ]}
]}
```

### Anti-Pattern 6: Toggling Features Mid-Conversation
```json
// ❌ WRONG — toggling web_search between requests invalidates system+messages cache
Request 1: {"tools": [..., {"type": "web_search_20250305"}]}
Request 2: {"tools": [...]}  // web search removed → cache invalidated
```

---

## 10. Advanced: Prefix Hash Mechanics

### How the Hash Works

The cache key is a **cumulative hash** of the token sequence from position 0 up to the breakpoint:

```
Hash = H(token[0], token[1], ..., token[breakpoint_position])
```

This means:
- Changing ANY token before or at the breakpoint changes the hash
- **Character-level sensitivity**: Even a single space difference = different hash
- **Includes tokenization**: If a model update changes tokenization, caches from the old tokenizer are invalid

### Practical Implications for Token Arrangement

1. **Concatenation order matters**: `"A\nB"` ≠ `"B\nA"` — even if semantically equivalent
2. **Whitespace matters**: `"Hello "` ≠ `"Hello"` — trailing space = different hash
3. **Encoding matters**: Ensure consistent UTF-8 encoding across requests
4. **Line endings**: `\n` ≠ `\r\n` — standardize line endings

### JSON Serialization Consistency Checklist

For tool definitions and structured content:
- [ ] Sort all JSON object keys alphabetically
- [ ] Use consistent number formatting (no trailing zeros, consistent precision)
- [ ] Use consistent string escaping (same escape sequences)
- [ ] No trailing commas
- [ ] Consistent indentation (or use compact format)
- [ ] Use a deterministic JSON serializer (avoid language defaults that randomize)

---

## 11. Token Count Budgeting for Cache Efficiency

### Minimum Thresholds by Model

| Model | Min Tokens | Typical Content to Hit Threshold |
|-------|-----------|----------------------------------|
| Claude Sonnet 4.5/4.6 | 1,024 | ~4KB of text or 3-4 tool definitions |
| Claude Opus 4.7 | 2,048 | ~8KB of text or 6-8 tool definitions |
| Claude Opus 4.5/4.6 | 4,096 | ~16KB of text or 12+ tool definitions |
| Claude Haiku 4.5 | 4,096 | ~16KB of text |

### Reaching the Threshold

If your cached prefix is just below the minimum:
- **Expand system prompt**: Add more detailed instructions or context
- **Add more examples**: Include additional few-shot examples
- **Include documentation**: Embed relevant API docs or coding standards
- **Combine sections**: Merge tools + system into a larger cacheable unit

The cost of expanding content to reach the threshold is tiny compared to the 90% savings on cache reads for frequently-used prompts.

---

## 12. Arrangement Patterns Summary Matrix

| Content Type | Where to Place | Cache Strategy | TTL Choice |
|-------------|---------------|----------------|------------|
| Tool definitions | `tools[]` (first) | BP on last tool | 5-min (rarely changes) |
| Core role instructions | `system[0]` | Explicit BP | 1-hour |
| Injected reference docs | `system[1]` | Explicit BP | 5-min (per session) |
| Few-shot examples | `system` or early messages | Explicit BP | 5-min |
| Conversation history | `messages[]` | Automatic caching | 5-min |
| Current user input | Last in `messages[]` | NOT cached (after BP) | N/A |
| Timestamps/request IDs | User message (after BP) | NOT cached | N/A |
| Tool results | `messages[]` content | Automatic (grows) | 5-min |

---

## 13. Key Takeaways for Implementation

1. **The simplest effective setup**: Top-level `cache_control` + explicit BP on system prompt = 2 breakpoints covering most scenarios.

2. **For coding assistants specifically**: Tools (deterministic, BP on last) + System prompt with role+skills (BP) + automatic conversation caching = maximum cache efficiency with minimal code changes.

3. **The 20-block rule**: Monitor conversation growth. If tool results or large assistant responses cause >20 blocks per turn, add a mid-conversation anchor breakpoint.

4. **Byte-for-byte stability is non-negotiable**: Invest in deterministic serialization, append-only history, and strict separation of static vs. dynamic content.

5. **Cache writes are cheap investments**: At 1.25x base cost, a cache write pays for itself after just ~2 cache reads (at 0.1x base cost). For any content used 3+ times, caching is always profitable.

6. **Use automatic caching as the default**: Only add explicit breakpoints when you need mixed TTLs, long-conversation anchors, or independent system/tool caching.
