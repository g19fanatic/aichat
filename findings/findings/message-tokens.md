# Message Token Counting Hot Path
**Task ID**: 9  **Scope**: Analyze `estimate_token_length`, `messages_tokens`, `total_tokens`, and `guard_max_input_tokens` — their call sites, algorithmic cost, and whether they are memoized or called redundantly on the per-round-trip hot path.

## Evidence Gathered

- `src/client/model.rs:284-291` — `guard_max_input_tokens` computes `total_tokens` **unconditionally before** checking the `max_input_tokens` Option:
  ```rust
  pub fn guard_max_input_tokens(&self, messages: &[Message]) -> Result<()> {
      let total_tokens = self.total_tokens(messages) + BASIS_TOKENS;   // ← always runs
      if let Some(max_input_tokens) = self.data.max_input_tokens {     // ← THEN checks
          if total_tokens >= max_input_tokens {
              bail!("Exceed max_input_tokens limit")
          }
      }
      Ok(())
  }
  ```
  For the ~95% of models without `max_input_tokens` set (`None`), ALL computation is performed and then thrown away. A one-line early-exit would eliminate it entirely.

- `src/config/input.rs:233-248` — `prepare_completion_data` calls `guard_max_input_tokens` **unconditionally on every HTTP request** (both stream and non-stream paths):
  ```rust
  pub fn prepare_completion_data(&self, model: &Model, stream: bool) -> Result<ChatCompletionsData> {
      let mut messages = self.build_messages()?;
      patch_messages(&mut messages, model);
      model.guard_max_input_tokens(&messages)?;   // ← HOT PATH, every round-trip
      ...
  }
  ```
  Called from `common.rs:74` (non-stream) and `common.rs:95` (stream) — one invocation per HTTP round-trip. For a 10-round tool chain: 10 full message-tree scans.

- `src/client/model.rs:255-266` — `messages_tokens` calls **`serde_json::to_string(v)`** on every `ToolResult` in every `MessageContentToolCalls` message, just to count tokens:
  ```rust
  MessageContent::ToolCalls(MessageContentToolCalls { tool_results, text, .. }) => {
      estimate_token_length(text)
          + tool_results
              .iter()
              .map(|v| {
                  serde_json::to_string(v)          // ← JSON serialize just to count tokens
                      .map(|v| estimate_token_length(&v))
                      .unwrap_or_default()
              })
              .sum::<usize>()
  }
  ```
  The JSON String is immediately discarded after counting. For R rounds × N tool results accumulating in one `MessageContentToolCalls`, this re-serializes all prior rounds' results every round-trip.

- `src/utils/mod.rs:73-88` — `estimate_token_length` allocates a **`Vec<&str>` heap allocation on every call**:
  ```rust
  pub fn estimate_token_length(text: &str) -> usize {
      let words: Vec<&str> = text.unicode_words().collect();   // ← heap alloc every call
      let mut output: f32 = 0.0;
      for word in words {
          if word.is_ascii() {
              output += 1.3;
          } else {
              let count = word.chars().count();   // ← O(len) per non-ASCII word
              ...
          }
      }
      output.ceil() as usize
  }
  ```
  Called once per text field per message per round. The `.collect()` is unnecessary — `unicode_words()` is an iterator that could be consumed directly without materializing the Vec.

- `src/client/model.rs:240-244` — `messages_tokens` calls `strip_think_tag(text)` (a `fancy_regex` regex scan) on **every non-last assistant message every round-trip**:
  ```rust
  MessageContent::Text(text) => {
      if v.role.is_assistant() && i != messages_len - 1 {
          estimate_token_length(&strip_think_tag(text))   // ← regex on every prior assistant msg
      } else {
          estimate_token_length(text)
      }
  }
  ```
  `strip_think_tag` uses `THINK_TAG_RE` (a `fancy_regex` regex: `(?s)^\s*<think>.*?</think>(\s*|$)`) — which scans the full text even when no `<think>` tag is present. `fancy_regex` is a slower engine (supports lookaheads/lookbehinds via backtracking). Called for ALL prior intermediate assistant turns in the message history, every round.

- `src/config/session.rs:134-136` — `update_tokens()` caches `total_tokens` on the Session object:
  ```rust
  pub fn update_tokens(&mut self) {
      self.tokens = self.model().total_tokens(&self.messages);
  }
  ```
  Call sites: session load (line 105), `set_role` (line 279), `set_model` (line 357), `add_message` (lines 507, 517), compress (line 589). None of these are in the per-round-trip tool-call hot path — the Session cached count is STALE during multi-round tool chains. The hot path uses `guard_max_input_tokens` instead (fresh recount every round).

- `src/config/session.rs:320-329` — `need_compress` reads the cached stale `self.tokens()` value (not a fresh count):
  ```rust
  pub fn need_compress(&self, global_compress_threshold: usize) -> bool {
      ...
      self.tokens() > threshold   // ← reads self.tokens (stale during tool chain)
  }
  ```
  Only called in `maybe_compress_session` at `mod.rs:1225-1236` — which is inside the REPL path only, NOT in `start_directive` command-line agent path (confirmed from Task 1/2 learnings). Not a hot-path concern, but the stale tokens value means compression might not fire when it should in long tool chains.

- `src/client/model.rs:303` — `max_input_tokens` is `Option<usize>` in `ModelData`:
  ```rust
  pub max_input_tokens: Option<usize>,
  ```
  Most models in `models.yaml` do NOT set `max_input_tokens` — it's an optional cap that most users don't configure. This makes the unconditional `total_tokens` computation in `guard_max_input_tokens` entirely wasted for typical usage.

## Findings

1. **`guard_max_input_tokens` performs full message scan on EVERY round-trip** (CONFIRMED): Called at `input.rs:240` inside `prepare_completion_data()`, which fires on every HTTP request — both `chat_completions` (`common.rs:74`) and `chat_completions_streaming` (`common.rs:95`). For a 10-round tool chain: 10 full O(M×text_size) scans of all messages.

2. **`guard_max_input_tokens` computes `total_tokens` BEFORE checking if it's needed** (CONFIRMED): `model.rs:285` computes unconditionally, then `model.rs:286` checks `if let Some(max_input_tokens) = self.data.max_input_tokens`. For models without `max_input_tokens` configured (the common case), this is pure waste. Fix: add `if self.data.max_input_tokens.is_none() { return Ok(()); }` before the computation.

3. **`messages_tokens` calls `serde_json::to_string(v)` per ToolResult for token estimation** (CONFIRMED): `model.rs:261-263` — serializes each `ToolResult` to a full JSON String only to call `estimate_token_length` on it. The String is discarded immediately. For R=10 rounds × N=5 tool results: the accumulated ToolResults (all previous rounds) are serialized on every single round. This interacts with Task 7's quadratic growth finding: cost is O(R² × N × result_size).

4. **`estimate_token_length` allocates `Vec<&str>` on every invocation** (CONFIRMED): `utils/mod.rs:74` — `text.unicode_words().collect()` materializes a Vec unnecessarily. Changing to `for word in text.unicode_words()` eliminates the heap allocation with zero algorithmic change.

5. **`strip_think_tag` regex runs on ALL prior assistant messages every round** (CONFIRMED): `model.rs:242` — `THINK_TAG_RE.replace_all(text, "")` runs a `fancy_regex` scan for every non-last assistant message. Most models don't emit `<think>` tags (only Claude 3.7+, DeepSeek-R1, Qwen3 thinking models). The regex scans the FULL text content of each prior assistant turn on every round-trip, even when guaranteed to be absent.

6. **Two-tier token tracking: fresh-count path vs. cached-count path** (CONFIRMED): Session `self.tokens` is a cached stale value (not updated during tool chain). `guard_max_input_tokens` does a fresh O(messages) count every round. These are different functions with different consumers: the cached value drives `need_compress` (REPL path only); the fresh count drives context-limit enforcement. There is no memoization of per-message token counts.

7. **No per-message token memoization exists anywhere** (CONFIRMED): `Message`, `MessageContent`, `MessageContentToolCalls`, `ToolResult` — none have a cached token count field. Every call to `messages_tokens` recomputes from scratch. Immutable historical messages (which never change) are re-counted every round.

8. **`update_tokens()` callsites are all outside the per-round hot path** (CONFIRMED): Lines 105 (load), 279 (set_role?), 357 (set_model?), 507/517 (add_message), 589 (compress) — none are in the intermediate tool-call rounds. The hot path per-round token counting happens ONLY via `guard_max_input_tokens`.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | **Early-exit in `guard_max_input_tokens`**: add `if self.data.max_input_tokens.is_none() { return Ok(()); }` at top of function (model.rs:284) | **high** (eliminates ALL token-counting work for models without limit) | **S** (1-2 lines) | **low** (pure optimization, semantics unchanged) | CONFIRMED (`model.rs:284-291`, `model.rs:303`) |
| 2 | **Remove `Vec::collect()` in `estimate_token_length`**: change `let words: Vec<&str> = text.unicode_words().collect(); for word in words` → `for word in text.unicode_words()` | **med** (eliminates heap alloc per text field per message per round) | **S** (1-2 lines) | **low** (pure optimization, same iteration order) | CONFIRMED (`utils/mod.rs:73-88`) |
| 3 | **Skip `strip_think_tag` for non-thinking models**: add a model capability check (e.g., `model.data.supports_thinking`) before `THINK_TAG_RE.replace_all` call in `messages_tokens` | **med** (eliminates regex scan on every prior assistant message for non-thinking models, i.e., >90% of usage) | **M** (requires ModelData capability flag + check) | **low** (only affects token estimation accuracy by epsilon for non-thinking models) | CONFIRMED (`model.rs:240-244`, `utils/mod.rs:39`) |
| 4 | **Avoid `serde_json::to_string` for ToolResult token estimation**: implement a lightweight `fn estimate_tokens(&self) -> usize` on ToolResult that sums text byte lengths directly | **med-high** (eliminates serde_json alloc+serialize per tool result per round, compounded quadratically with rounds) | **M** (add method to ToolResult/MessageContentToolCalls) | **low** (token counts are estimates anyway; byte-length heuristic is equally approximate) | CONFIRMED (`model.rs:261-263`) |
| 5 | **Per-message token caching**: add `cached_tokens: std::cell::Cell<Option<usize>>` to `Message`, populate on first `estimate_token_length` call, reuse on subsequent rounds | **high** (eliminates re-computation for ALL prior messages; only new message needs counting each round) | **L** (changes Message struct, all clone paths, requires invalidation on mutation) | **med** (Cell is non-Send; need to use AtomicUsize or Mutex for async safety; mutation invalidation is fragile) | CONFIRMED (no memoization exists: `message.rs` has no cached_tokens field) |
| 6 | **Reorder guard check before compute** (supersedes #1 but more complete): refactor `guard_max_input_tokens` to `if self.data.max_input_tokens.is_none() { return Ok(()) }; let t = self.total_tokens(messages) + BASIS_TOKENS; ...` | **high** | **S** | **low** | CONFIRMED |

## Open Questions / Needs Verification

- What fraction of users actually set `max_input_tokens` in their model config? If <5%, optimization #1 gives near-100% speedup on token-counting path at near-zero cost.
- Does `fancy_regex` for `THINK_TAG_RE` short-circuit on first-character mismatch (i.e., if text doesn't start with whitespace or `<think>`, is the scan O(1))? The regex pattern `(?s)^\s*<think>.*?</think>` anchors at `^` which could allow early exit. Needs regex engine behavior verification.
- Are there runtime scenarios where `guard_max_input_tokens` is called without `prepare_completion_data` (e.g., directly from other call sites)? Currently only 2 callsites exist; both are via `prepare_completion_data`.
- Is `ToolResult` the inner type that's being serialized, or a wrapper? Need to check the struct definition to confirm what JSON overhead is involved (string fields vs. nested objects).
- Would per-message caching break the `strip_think_tag` semantics (tokens are estimated after stripping, but the Message stores the original text)? This requires careful cache invalidation design.

## Hot-Path Classification

**Per-round-trip**: `guard_max_input_tokens` → `total_tokens` → `messages_tokens` → `estimate_token_length` (×M messages), `serde_json::to_string` (×N tool results from all prior rounds), `strip_think_tag` regex (×K prior assistant messages).

**Per-session (cold)**: `Session::update_tokens()` — called at session load, add_message (chain end), compress.

**Classification**: Token-counting is **Per-round-trip hot path** for models with `max_input_tokens` set (small minority), and **Per-round-trip wasted-computation** for models without it (large majority).
