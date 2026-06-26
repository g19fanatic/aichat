# LLM Prefix Caching Research

## Overview

All three major LLM providers (Anthropic, OpenAI, Google) implement caching mechanisms that reward **stable prefixes** — content at the beginning of a request that remains identical across multiple API calls. This research documents how each provider's caching works, what conditions must be met for cache hits, and how message content ordering affects cache efficiency.

**Key Universal Insight**: All providers cache from the **start** of the prompt forward. Dynamic content (user questions) should be placed at the **end** of the message to maximize the stable prefix that can be cached.

---

## (a) Per-Provider Cache Mechanism

### 1. Anthropic Claude — Prompt Caching

**Mechanism**: Prefix-based caching with explicit or automatic breakpoints.

**Two modes**:
1. **Automatic caching**: Add a single `cache_control` field at the top level of the request. The system automatically places the cache breakpoint on the last cacheable block and moves it forward as conversations grow.
2. **Explicit cache breakpoints**: Place `cache_control` directly on individual content blocks for fine-grained control over exactly what gets cached.

**Cache hierarchy**: Content is cached in this order:
```
tools → system → messages
```
Each level builds upon the previous. Changes at any level invalidate that level and all subsequent levels.

**How it works**:
1. System checks if a prompt prefix (up to a specified cache breakpoint) is already cached from a recent query
2. If found, uses the cached version (reducing processing time and costs)
3. If not found, processes the full prompt and caches the prefix once the response begins

**Pricing**:
- Cache writes: 125% of base input token price (5-min TTL) or 200% (1-hour TTL)
- Cache reads: **10%** of base input token price (90% savings!)
- Regular uncached tokens: 100% base price

**Cache lifetime**:
- Default: 5 minutes (refreshed on each use at no additional cost)
- Extended: 1 hour (at additional cost)

**Minimum cacheable tokens** (varies by model):
- 512 tokens: Claude Fable 5, Mythos 5
- 1,024 tokens: Claude Opus 4.8, Sonnet 4.5/4.6, Opus 4/4.1, Sonnet 4
- 2,048 tokens: Claude Mythos Preview, Opus 4.7
- 4,096 tokens: Claude Opus 4.5/4.6, Haiku 4.5

**Lookback window**: System checks up to 20 blocks backward from the breakpoint to find prior cache entries.

**Source**: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching

---

### 2. OpenAI — Prefix Caching (Automatic)

**Mechanism**: Fully automatic prefix-based caching. No code changes required.

**How it works**:
1. **Cache Routing**: Requests are routed to a machine based on a hash of the initial prefix (typically the first 256 tokens). The `prompt_cache_key` parameter can influence routing.
2. **Cache Lookup**: System checks if the initial portion (prefix) of the prompt exists in the cache on the selected machine.
3. **Cache Hit**: If a matching prefix is found, uses the cached result (significantly decreased latency and reduced costs).
4. **Cache Miss**: If no match found, processes the full prompt, caching the prefix afterward.

**Pricing**: 
- **No additional fees** for caching
- Cache hits reduce latency by up to 80% and input token costs by up to 90%
- No charge for cache writes

**Cache lifetime**:
- In-memory: 5-10 minutes of inactivity, up to 1 hour max
- Extended: up to 24 hours (for supported models like GPT-5.5, GPT-5.1, GPT-4.1)

**Minimum cacheable tokens**: 1,024 tokens

**Models**: All recent models (gpt-4o and newer)

**What can be cached**:
- Complete messages array (system, user, assistant interactions)
- Images (as links or base64-encoded data)
- Tool definitions
- Structured output schemas (serve as prefix to system message)

**Source**: https://platform.openai.com/docs/guides/prompt-caching

---

### 3. Google Gemini — Context Caching (Implicit + Explicit)

**Mechanism**: Two distinct caching systems.

#### Implicit Caching (Automatic)
- **Enabled by default** on Gemini 2.5 and newer models
- Automatic prefix matching — no code changes needed
- Cost savings are passed on automatically when requests hit caches
- No guaranteed cost savings (opportunistic)

**Minimum tokens for implicit caching**:
- 4,096 tokens: Gemini 3.5 Flash, Gemini 3.1 Pro Preview
- 2,048 tokens: Gemini 2.5 Flash, Gemini 2.5 Pro

**How to increase implicit cache hits**:
- Put large and common contents at the **beginning** of the prompt
- Send requests with similar prefixes in a short amount of time

#### Explicit Caching (Manual)
- Create a named `CachedContent` object with a TTL
- Reference the cache by name in subsequent `GenerateContent` requests
- **Guaranteed cost savings** when using cached content
- The cached content functions as a **prefix to the prompt**

**Key quote from docs**: *"The model doesn't make any distinction between cached tokens and regular input tokens. Cached content is a prefix to the prompt."*

**TTL**: Configurable (default: 1 hour). Can be updated after creation.

**Billing for explicit caching**:
- Cache token count: billed at a reduced rate when included in subsequent prompts
- Storage duration: billed based on TTL duration
- Non-cached input tokens and output tokens: billed normally

**Source**: https://ai.google.dev/gemini-api/docs/caching

---

## (b) What Must Be Identical for Cache Hits

### Anthropic Claude

For a cache hit, the following must be **100% identical**:

1. **All text and images** up to and including the block marked with `cache_control`
2. **Tool definitions** — any change to names, descriptions, or parameters invalidates the entire cache
3. **System messages** — any modification invalidates system and message caches
4. **Message content** — must be byte-for-byte identical up to the breakpoint
5. **Image detail parameter** — must be set identically (affects tokenization)
6. **tool_choice parameter** — changes invalidate message cache
7. **Thinking parameters** — changes to extended thinking settings affect message cache
8. **Web search toggle** — enabling/disabling modifies system prompt
9. **Citations toggle** — enabling/disabling modifies system prompt
10. **JSON key ordering** — must be stable (some languages randomize key order)

**Cache invalidation cascade**: Changes flow downward through the hierarchy:
- Tool definition change → invalidates tools + system + messages cache
- System message change → invalidates system + messages cache
- Message change → invalidates messages cache only

### OpenAI

For a cache hit:

1. **Exact prefix match** — the initial portion of the prompt must be identical
2. **Messages array** — complete messages must match (system, user, assistant)
3. **Images** — must be identical including detail parameter
4. **Tools** — tool definitions must be identical between requests
5. **Structured outputs** — schema must be identical (serves as prefix to system message)
6. **prompt_cache_key** — if used, must be consistent for same prefix patterns

**Critical constraint**: *"Cache hits are only possible for exact prefix matches within a prompt."*

### Google Gemini

#### Implicit Caching:
1. **Similar prefix** — content at the beginning of the prompt must be similar/identical
2. **Temporal proximity** — requests with similar prefixes should be sent within a short time window
3. **Token minimum** — must meet the minimum token threshold for the model

#### Explicit Caching:
1. **Cache reference** — must reference the correct `cachedContent` name
2. **Model match** — must use the same model the cache was created for
3. **Content is fixed** — the cached content cannot be modified (only TTL can be updated)
4. **Cached content is prefix** — it functions as the beginning of the prompt; new content is appended after it

---

## (c) How User Message Content Positioning Affects Caching

### The Core Principle (Universal Across All Providers)

All three providers implement **prefix-based caching** — content is matched and cached from the **start** of the prompt forward. This means:

```
┌─────────────────────────────────────────────────────────┐
│ BEGINNING of prompt/message (cached) ──────────────────→│
│                                                         │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ STATIC content = CACHE HIT potential               │ │
│ │ (system prompt, tools, file contents, context)     │ │
│ └─────────────────────────────────────────────────────┘ │
│                                                         │
│ ┌─────────────────────────────────────────────────────┐ │
│ │ DYNAMIC content = forces cache MISS on everything  │ │
│ │ (user question, timestamps, per-request context)   │ │
│ └─────────────────────────────────────────────────────┘ │
│                                                         │
│ ←────────────────────────────── END of prompt/message │
└─────────────────────────────────────────────────────────┘
```

### Impact on Each Provider

#### Anthropic Claude

From the docs: *"Place cached content at the prompt's beginning for best performance."*

And critically: *"Place the breakpoint on the last block that stays identical across requests. For a prompt with a static prefix and a varying suffix (timestamps, per-request context, the incoming message), that is the end of the prefix, not the varying block."*

**Common mistake documented by Anthropic**: If you place `cache_control` on a block that changes every request (like a timestamp or dynamic user message), the prefix hash changes every time, and the lookback never finds a prior cache entry. You pay for a fresh cache write on every request and never get a read.

**Impact of wrong ordering**: If the user prompt (dynamic) comes FIRST in the content, followed by static file contents:
- The first block changes every request
- The prefix hash at any breakpoint includes the dynamic content
- Result: **cache MISS on every single request**
- Cost: Full input token pricing every time

**Impact of correct ordering**: If file contents (static) come FIRST, followed by the user prompt (dynamic) at the end:
- The static file content forms a stable prefix
- Cache breakpoint can be placed after the static content
- Result: **cache HIT on repeated requests with same files**
- Cost: 10% of input token price for cached portion (90% savings)

#### OpenAI

From the docs: *"Structure prompts with static or repeated content at the beginning and dynamic, user-specific content at the end."*

**Routing mechanism**: OpenAI hashes the first ~256 tokens to route requests to machines with the relevant cache. If the first 256 tokens change every request (because the user prompt is first), requests get routed to different machines every time — making cache hits impossible even if the bulk of the content is identical.

**Impact of wrong ordering**: Dynamic prompt at the start:
- Hash of first 256 tokens changes → routed to different machine
- Even if routed correctly, prefix doesn't match → cache miss
- Result: Full processing and full cost every time

**Impact of correct ordering**: Static files at the start:
- Hash of first 256 tokens is stable → routed to same machine
- Prefix matches up to where the static content ends → cache hit
- Result: Up to 80% latency reduction and 90% cost reduction

#### Google Gemini

From the docs: *"Try putting large and common contents at the beginning of your prompt"*

And: *"Cached content is a prefix to the prompt."*

**Implicit caching**: Works best when:
- Large, common content is at the beginning
- Requests with similar prefixes arrive in a short time window

**Explicit caching**: The cached content **is** the prefix. The user's new query is appended **after** the cached content. This architecture inherently assumes static content first, dynamic content last.

**Impact of wrong ordering**: With dynamic content first, implicit caching cannot match prefixes between requests.

**Impact of correct ordering**: Large static files form the common prefix → implicit caching kicks in automatically.

---

## Summary: Implications for aichat

### Current Behavior (Suboptimal)
```
User message content = [prompt_text] + [file_A_contents] + [file_B_contents]
                        ↑ DYNAMIC        ↑ STATIC          ↑ STATIC
```

Every request has a different beginning (the prompt changes), so:
- **Anthropic**: Cache prefix hash always differs → 0% cache hits
- **OpenAI**: First 256 tokens change → routed to different machines → 0% cache hits
- **Google**: No common prefix → implicit caching fails → 0% cache hits

### Proposed Behavior (Optimal)
```
User message content = [file_A_contents] + [file_B_contents] + [prompt_text]
                        ↑ STATIC          ↑ STATIC            ↑ DYNAMIC
```

The beginning is stable (same files across calls), so:
- **Anthropic**: Stable prefix → cache breakpoint after files → 90% input cost savings on cached portion
- **OpenAI**: Stable first 256 tokens → same machine routing → prefix caching → up to 90% cost savings
- **Google**: Common prefix → implicit caching works → automatic cost savings

### Quantified Impact Example

Given: 2 files totaling 50,000 tokens + a 50-token user prompt, making 10 requests with same files:

| Metric | Current (prompt first) | Proposed (files first) |
|--------|----------------------|----------------------|
| Cacheable prefix | 0 tokens | 50,000 tokens |
| Cache hit rate | 0% | ~90% (9 of 10 requests) |
| **Anthropic cost** (Sonnet) | 10 × 50,050 × $3/MTok = **$1.50** | 1 write + 9 reads = ~**$0.19** |
| **OpenAI cost** (GPT-4o) | 10 × 50,050 × $2.50/MTok = **$1.25** | 1 miss + 9 hits = ~**$0.15** |
| Latency (Anthropic) | Full prefill each time | ~80% reduction on cache hits |
| Latency (OpenAI) | Full prefill each time | ~80% reduction on cache hits |

### Provider-Specific Recommendations for aichat

1. **For Anthropic**: After reordering content, aichat could optionally add `cache_control` breakpoints after the file content blocks to explicitly mark the static prefix boundary. This would guarantee caching behavior.

2. **For OpenAI**: Simply reordering content is sufficient — caching is fully automatic and requires no API changes. The stable prefix will naturally be cached.

3. **For Google Gemini**: For implicit caching, reordering is sufficient. For explicit caching, aichat could create a `CachedContent` object containing the file contents and reference it across requests (more complex, but provides guaranteed savings and longer TTL).

---

## References

1. Anthropic Prompt Caching Documentation: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching
2. OpenAI Prompt Caching Documentation: https://platform.openai.com/docs/guides/prompt-caching
3. Google Gemini Context Caching Documentation: https://ai.google.dev/gemini-api/docs/caching
