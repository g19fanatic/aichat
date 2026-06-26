# Session Compress
**Task ID**: 15  **Scope**: Analyze compress_session and need_compress — when they fire in tool-call loops, blocking behavior, parallel-with-next-request behavior, and interaction with high-tool-count agent flows.

## Evidence Gathered

- `src/config/session.rs:320-330` — `need_compress` function:
  ```rust
  pub fn need_compress(&self, global_compress_threshold: usize) -> bool {
      if self.compressing { return false; }
      let threshold = self.compress_threshold.unwrap_or(global_compress_threshold);
      if threshold < 1 { return false; }
      self.tokens() > threshold
  }
  ```
  Uses cached `self.tokens()` (stale during tool-call chains per Task 9). Default threshold = 4000 (`mod.rs:201`).

- `src/config/mod.rs:1224-1257` — `maybe_compress_session` function:
  ```rust
  pub fn maybe_compress_session(config: GlobalConfig) {
      // ...check need_compress...
      session.set_compressing(true);
      tokio::spawn(async move {
          if let Err(err) = Config::compress_session(&config).await { warn!(...) }
          session.set_compressing(false);
      });
  }
  ```
  Uses `tokio::spawn` — compression is FULLY ASYNC and NON-BLOCKING to the caller. Returns immediately after spawning the task.

- `src/config/mod.rs:1258-1292` — `compress_session` implementation:
  ```rust
  pub async fn compress_session(config: &GlobalConfig) -> Result<()> {
      let prompt = config.read().summarize_prompt.clone()
          .unwrap_or_else(|| SUMMARIZE_PROMPT.into());
      let input = Input::from_str(config, &prompt, None);
      let summary = input.fetch_chat_text().await?;  // ← FULL LLM CALL
      let summary_prompt = config.read().summary_prompt.clone()...;
      session.compress(format!("{summary_prompt}{summary}"));
      Ok(())
  }
  ```
  Makes a full LLM call via `fetch_chat_text()` = `client.chat_completions(self.clone())`.  
  `SUMMARIZE_PROMPT` = "Summarize the discussion briefly in 200 words or less..." (`mod.rs:71-72`).  
  `SUMMARY_PROMPT` = "This is a summary of the chat history as a recap: " (`mod.rs:73`).

- `src/repl/mod.rs:756` — `maybe_compress_session` call site:
  ```rust
  // In repl/mod.rs ask() function, the else branch (no more tool_results):
  Config::maybe_autoname_session(config.clone());
  Config::maybe_compress_session(config.clone());
  Ok(())
  ```
  Only called in the REPL `ask()` function's **terminal branch** (when `tool_results.is_empty()` — i.e., after the entire tool-call chain completes). NOT called during intermediate rounds.

- `src/repl/mod.rs:732-734` — Compression guard at top of `ask()`:
  ```rust
  while config.read().is_compressing_session() {
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
  }
  ```
  The NEXT REPL turn polls every 100ms waiting for compression to complete. This is a busy-wait polling loop that **blocks the NEXT turn's start** if compression is still running.

- `src/repl/mod.rs:546` — Manual `.compress session` command:
  ```rust
  abortable_run_with_spinner(Config::compress_session(config), "Compressing", ...)
  ```
  This is the synchronous (awaited) path — blocks until compression completes. User-triggered only.

- `src/config/input.rs:226-230` — `fetch_chat_text` used by compression:
  ```rust
  pub async fn fetch_chat_text(&self) -> Result<String> {
      let client = self.create_client()?;
      let text = client.chat_completions(self.clone()).await?.text;
      let text = strip_think_tag(&text).to_string();
      Ok(text)
  }
  ```
  Uses `chat_completions` (non-streaming), meaning `build_client()` (new ReqwestClient per Task 12) + full HTTP round-trip.

- `src/config/session.rs:340-358` — `Session::compress` mutates session state:
  ```rust
  pub fn compress(&mut self, mut prompt: String) {
      // optionally prepend system prompt
      self.compressed_messages.append(&mut self.messages);  // MOVE all messages
      self.messages.push(Message::new(System, Text(prompt))); // replace with summary
      self.dirty = true;
      self.update_tokens();
  }
  ```
  After compression: `self.messages` contains only 1 system message (the summary); ALL prior messages moved to `self.compressed_messages` (never shown to LLM again). This is DESTRUCTIVE — once compressed, history is irreversibly collapsed.

- `src/config/mod.rs:201` — Default threshold:
  ```rust
  compress_threshold: 4000,
  ```
  4000 tokens is the default. With the stale token cache (Task 9 finding), `need_compress` won't fire during a tool-call chain because tokens are only updated on `add_message()` at chain end.

- `src/config/session.rs:32` — `compress_threshold` field:
  `compress_threshold: Option<usize>` — per-session override. Global default used if None.

## Findings

1. **`maybe_compress_session()` fires ONLY in REPL path** (CONFIRMED, `repl/mod.rs:756`): Called exclusively in the `ask()` function's terminal branch — after all tool results are consumed and no new tool calls remain. NOT called in `start_directive` (non-REPL) cmd-line agent path. For headless/scripted use: **compress is never triggered**.

2. **Compression is fully async/non-blocking at trigger point** (CONFIRMED, `mod.rs:1246`): `tokio::spawn(async move {...})` — the spawning call returns immediately. The current REPL turn completes instantly; compression runs in background.

3. **Compression BLOCKS the NEXT REPL turn** (CONFIRMED, `repl/mod.rs:732-734`): At the start of every `ask()` call, there's a 100ms polling sleep loop: `while is_compressing_session() { sleep(100ms) }`. If compression is still running when the user submits the next prompt, the user sees a 0-100ms (or more) delay. For slow LLM responses during compression, could block for seconds.

4. **Compression makes a full LLM API call** (CONFIRMED, `mod.rs:1274`): `input.fetch_chat_text().await?` → `client.chat_completions(self.clone())` — complete HTTP round-trip for summarization. Cost: 200-2000ms typical LLM latency. This is the dominant cost of compression, not the in-memory work.

5. **`need_compress` uses STALE token count** (CONFIRMED, cross-ref Task 9, `session.rs:328`): During multi-round tool-call chains, `self.tokens()` is NOT updated between intermediate rounds. Tokens are only updated on `add_message()` at chain end. Result: if a long tool-call chain crosses the 4000-token threshold MID-CHAIN, compression won't trigger until the NEXT user turn in REPL mode. This means the threshold is effectively a "turn-start threshold" not a "per-round threshold."

6. **High-tool-count agent flows interact safely** (CONFIRMED): The `compressing` boolean flag prevents concurrent compression (`session.rs:321`). Even if a very long agent chain finishes when threshold is exceeded, at most ONE compression task runs. The `is_compressing()` guard on subsequent REPL inputs is the only performance interaction.

7. **`Session::compress` is destructively in-place** (CONFIRMED, `session.rs:346-356`): After compression, `self.messages` is replaced with ONE system message (the LLM summary). All prior messages are moved to `compressed_messages` (never sent to LLM again). This is O(M) one-shot work — moves all message Vec storage.

8. **`fetch_chat_text` inherits all per-request overhead** (CONFIRMED, `input.rs:226`): The compression LLM call goes through `create_client()` → `init_client()` (linear scan, `input.rs:224`) + `build_client()` (new ReqwestClient per Task 12) + full message build + HTTP. No connection reuse optimization applies here either.

9. **`compress_threshold: 0` disables compression** (CONFIRMED, `session.rs:325`): `if threshold < 1 { return false; }` — setting threshold to 0 fully disables compression.

10. **Manual `.compress session` command is synchronous** (CONFIRMED, `repl/mod.rs:543-550`): `abortable_run_with_spinner(Config::compress_session(...), ...).await?` — fully awaited with spinner. Blocks until complete. Different from auto-compress path.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Replace 100ms polling loop (`is_compressing` busy-wait) with `tokio::sync::watch` channel | low | S | low | CONFIRMED — `repl/mod.rs:732-734`: busy-polls every 100ms; a watch channel would add zero overhead when not compressing and wake immediately when done |
| 2 | Cache `ReqwestClient` in compression path (same as Task 12 fix for all requests) | med | M | low | CONFIRMED — `input.rs:226-229`: fetch_chat_text calls `create_client()` → `build_client()` each time; same fix as global connection reuse |
| 3 | Add `compress_threshold: 0` to agent's per-session config by default for non-REPL use | low | S | low | CONFIRMED — compression never fires in non-REPL path anyway; documenting this as a user-facing config note |
| 4 | Make compression trigger threshold use LIVE token count (via `guard_max_input_tokens` result) | med | M | med | THEORY — would require either updating `self.tokens()` after each round or passing live count to `need_compress`; fixes silent under-compression for high-tool-count chains |
| 5 | Overlap compression LLM call with user-input wait (current design already does this via spawn) | N/A | N/A | N/A | CONFIRMED — already implemented; design is correct |
| 6 | Use streaming for compression summarization to reduce TTFB wait on next input | low | S | low | LIKELY — `fetch_chat_text` uses non-streaming `chat_completions`; the 100ms poll is the bottleneck; streaming wouldn't help if next input arrives quickly |

## Open Questions / Needs Verification

1. **What is the exact timing of the `is_compressing` polling delay?** If the user submits a second prompt within ~100ms of the compress trigger, they experience the polling loop. With typical LLM latency for a 4000-token summarization (1-3 seconds), the next input would likely poll 10-30 times. Total delay = actual compression latency (1-3s), not a fixed 100ms.

2. **Does `compressed_messages` affect `build_messages()` output?** Need to verify whether compressed_messages are ever re-included in the prompt. `Session::build_messages()` should only use `self.messages` (not `compressed_messages`), meaning compressed history is permanently excluded from future prompts — which is the intended behavior.

3. **Tool-call chain token growth vs. compress threshold**: Since `self.tokens()` is stale during a multi-round tool-call chain, and tokens accumulate quadratically (Task 7), is it possible to significantly exceed the 4000 threshold mid-chain without compression firing? Answer: YES — for a 10-round chain with 3 tools at 2KB each, the live token count could reach 30,000+ without triggering compression (because `self.tokens()` is only updated at chain end via `add_message`). The next REPL turn would then find tokens >> threshold and immediately trigger compression.

4. **Does `Input::from_str(config, &prompt, None)` for compression use the session's message history as context?** If the summarization request ALSO goes through session build_messages, it would include the entire accumulated session — making the LLM call expensive for large sessions. Needs code verification.

## Hot-Path Classification
**Per-round-trip**: no — does not fire per round.  
**Per-session (REPL only)**: YES — fires at most once after a full tool-call chain completes (when threshold exceeded).  
**Cold-path for `start_directive`**: CONFIRMED — never fires in non-REPL agent execution.

**Summary**: Session compression is an intentional "eventually consistent" background optimization that is correctly fire-and-forget for the auto path. The main performance concern is the 100ms busy-wait polling that could delay the next REPL input by up to the full compression LLM latency (1-3s). The feature does NOT impact the tool-call chain hot path at all. It's relevant only for REPL users with long multi-turn sessions.
