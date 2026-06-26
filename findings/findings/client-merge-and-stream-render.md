# Client Merge and Stream Render

**Task ID**: 13  **Scope**: Audit `markdown_stream` / `raw_stream` in `src/render/stream.rs` — per-frame terminal repaint cost, markdown re-render strategy, spinner interaction, and incremental rendering efficiency.

## Evidence Gathered

### render/stream.rs — streaming render pipeline
- `src/render/stream.rs:152-167` — `gather_events()` batches up to 50ms of `SseEvent::Text(String)` items into a `Vec<String>`, then calls `texts.join("")` to produce ONE joined String. Triple-alloc pattern: N owned Strings → Vec<String> → joined String per batch.
  ```rust
  SseEvent::Text(v) => texts.push(v),  // N Strings in Vec
  ...
  events.push(SseEvent::Text(texts.join("")))  // N+1th String
  ```
- `src/render/stream.rs:121` — when text contains `\n`, creates full concatenation of buffer+text just to find the split point, immediately discarded:
  ```rust
  let text = format!("{buffer}{text}");   // alloc: buffer + text concatenated
  let (head, tail) = split_line_tail(&text);
  ```
- `src/render/stream.rs:129` — when text has no `\n`, replaces buffer by full copy each batch:
  ```rust
  buffer = format!("{buffer}{text}");   // alloc: grows buffer by copy every batch
  ```
  Should be `buffer.push_str(&text)` — zero-alloc in-place append.
- `src/render/stream.rs:86` — `cursor::position()` ioctl called per batch (up to 3 retries), inside `markdown_stream_inner`. This is ~1 syscall per 50ms interval.
- `src/render/stream.rs:104` — `queue!(terminal::Clear(FromCursorDown))` is queued (no flush) per batch. Followed by `writer.flush()` at line 145 — 1 write syscall per batch.
- `src/render/stream.rs:49-52` — `raw_stream` flushes on EVERY token, zero batching:
  ```rust
  SseEvent::Text(text) => {
      print!("{text}");
      stdout().flush()?;   // 1 flush per token — no batching
  }
  ```

### render/markdown.rs — markdown renderer
- `src/render/markdown.rs:67-73` — `render()` processes ONLY the text passed to it — NOT all accumulated prior output. NOT O(n²):
  ```rust
  pub fn render(&mut self, text: &str) -> String {
      text.split('\n')
          .map(|line| self.render_line_mut(line))
          .collect::<Vec<String>>()
          .join("\n")
  }
  ```
  Called with `head` (completed lines in current batch only), not total response. ✅ Incremental.
- `src/render/markdown.rs:95` — `check_line()` calls `self.code_syntax.clone()` on EVERY line rendered:
  ```rust
  let mut code_syntax = self.code_syntax.clone();
  ```
  `SyntaxReference` contains a `String` name field + index — this is a heap allocation per rendered line. For a 100-line response, 100 `SyntaxReference` clones.
- `src/render/markdown.rs:142-145` — `highlight_line()` creates a new `HighlightLines` struct on every call (even for plain text):
  ```rust
  let mut highlighter = HighlightLines::new(syntax, theme);
  if let Ok(ranges) = highlighter.highlight_line(trimmed_line, &self.syntax_set) {
  ```
  One `HighlightLines` alloc (+ internal parse-state initialization) per line rendered, per batch.
- `src/render/markdown.rs:67-73` — `render()` allocates `Vec<String>` of N line strings, then `join("\n")` creates N+1th String:
  ```rust
  .collect::<Vec<String>>()
  .join("\n")
  ```

### spinner.rs — spinner interaction
- `src/utils/spinner.rs:73-76` — `Spinner::set_message()` contains a **blocking** `std::thread::sleep(10ms)`:
  ```rust
  pub fn set_message(&self, message: String) -> Result<()> {
      self.0.send(SpinnerEvent::SetMessage(message))?;
      std::thread::sleep(Duration::from_millis(10));   // BLOCKING in async context
      Ok(())
  }
  ```
- `src/utils/spinner.rs:78-81` — `Spinner::stop()` also contains a **blocking** `std::thread::sleep(10ms)`:
  ```rust
  pub fn stop(&self) {
      let _ = self.0.send(SpinnerEvent::Stop);
      std::thread::sleep(Duration::from_millis(10));   // BLOCKING in async context
  }
  ```
- `src/utils/spinner.rs:84-90` — `Spinner::create()` calls `spinner.set_message()`, triggering the 10ms blocking sleep BEFORE the spinner consumer task is even spawned:
  ```rust
  pub fn create(message: &str) -> (Self, UnboundedReceiver<SpinnerEvent>) {
      let (tx, spinner_rx) = mpsc::unbounded_channel();
      let spinner = Spinner(tx);
      let _ = spinner.set_message(message.to_string());  // ← 10ms sleep here, no consumer yet
      (spinner, spinner_rx)
  }
  ```
  In `spawn_spinner()`, `tokio::spawn` is called AFTER `Spinner::create()` returns — the sleep fires before the consumer task exists. The sleep was presumably meant to let the consumer process the initial message, but it is **completely ineffective**: the event sits unprocessed in the MPSC buffer during the sleep.
- `src/utils/spinner.rs:93-99` — `spawn_spinner()` calls `Spinner::create()` (10ms sleep) then `tokio::spawn` — the consumer task starts only AFTER the sleep:
  ```rust
  pub fn spawn_spinner(message: &str) -> Spinner {
      let (spinner, mut spinner_rx) = Spinner::create(message);  // ← 10ms sleep here
      tokio::spawn(async move { ... });                           // ← task spawned after sleep
      spinner
  }
  ```
  Total overhead: 10ms blocking at create + 10ms at stop = **20ms blocking per streaming request**.
- `src/utils/spinner.rs:105` — spinner tick interval = 50ms. Spinner task uses `tokio::spawn` (does NOT block render). Per tick: `stdout().flush()` via `step()`.
- `src/utils/spinner.rs:122-126` — `abortable_run_with_spinner()` (non-stream path) calls `Spinner::create()` → 10ms blocking sleep per non-streaming HTTP call.

### render/mod.rs — render_stream entry point
- `src/render/mod.rs:14-16` — double config read-lock acquisition at render start:
  ```rust
  if *IS_STDOUT_TERMINAL && config.read().highlight {
      let render_options = config.read().render_options()?;
  ```
  Two separate `config.read()` RwLock acquisitions where one would suffice.

## Findings

### Finding 1 — CONFIRMED: `render.render()` is NOT O(n²) — rendering IS incremental
`render()` at `markdown.rs:67-73` processes ONLY the lines passed in the current call (the `head` portion of the current batch's text). The `buffer` in `stream.rs` accumulates ONLY the current incomplete line (text after the last `\n`). Completed lines are rendered once via `render()` and flushed; they never re-enter the render path. For a 1000-token response with 50 lines: ~50 `render_line_mut` calls total across all batches. **NOT O(n²)**. The key open question from Task 10 is answered: incremental rendering is efficient at the line level.

However, `check_line()` clones `SyntaxReference` on every line, and `highlight_line()` creates a new `HighlightLines` parser per call (CONFIRMED). These are fixed-cost allocs per line but compound for long responses.

### Finding 2 — CONFIRMED: Blocking `std::thread::sleep(10ms)` in async context (Spinner)
`Spinner::set_message()` and `Spinner::stop()` both call `std::thread::sleep(10ms)` inside `spinner.rs:73,80`. These are called from:
- `spawn_spinner()` → `Spinner::create()` → `set_message` → 10ms sleep **before** `tokio::spawn` (the consumer doesn't exist yet — the sleep is entirely useless)
- `spinner.stop()` called from `markdown_stream_inner` and `raw_stream` (async fns) — 10ms blocking sleep on every first-text event and stream completion

For `abortable_run_with_spinner` (non-stream path, e.g., tool call chain), `Spinner::create()` is called at `spinner.rs:122` → 10ms blocking sleep at the START of every non-streaming HTTP request.

**Total cost per request**: 20ms of blocking (10ms create + 10ms stop) for streaming path; 10ms blocking for non-streaming path. This stalls the tokio thread, potentially delaying other async tasks. For a 10-round tool-call chain using non-streaming = **100ms of unnecessary blocking** in the async executor.

### Finding 3 — CONFIRMED: `buffer = format!("{buffer}{text}")` O(n) alloc per batch
`stream.rs:121` creates `format!("{buffer}{text}")` for the newline case (just to find the split point, then discards the allocation). `stream.rs:129` replaces the entire buffer with a new allocation for the no-newline case. Both should use `buffer.push_str(&text)` for zero-copy in-place append. For a 1000-token response batched at 50ms: ~20 batches × buffer_size copies.

### Finding 4 — CONFIRMED: `gather_events` triple-alloc per 50ms batch
`stream.rs:162-168`: N `String` tokens arrive from MPSC (each already an owned String from `SseHandler::text()`), pushed into `Vec<String>`, then `.join("")` allocates a new joined String. Three String lifetimes coexist: the N individual Strings + the Vec + the final joined String. For high-throughput streaming (many tokens per 50ms): N can be 10-30, so 10-30 redundant allocs per batch that could be eliminated with a pre-allocated buffer.

### Finding 5 — CONFIRMED: `check_line()` clones `SyntaxReference` per line
`markdown.rs:95`: `let mut code_syntax = self.code_syntax.clone()` executes on EVERY `render_line` / `render_line_mut` call. `SyntaxReference` contains a heap-allocated String name. For a 100-line code block, this is 100 redundant `SyntaxReference` clones even when code_syntax doesn't change between lines. The fix: pass `code_syntax` as a reference or inline the check_line logic to avoid cloning when the state hasn't changed.

### Finding 6 — CONFIRMED: `HighlightLines::new(syntax, theme)` per line (when theme is set)
`markdown.rs:143`: A new `HighlightLines` struct is created for every `highlight_line()` call. `HighlightLines` is a syntect incremental parser — initialization involves setting up parse state. For themed output with a 50-line code block: 50 `HighlightLines` constructions. However, note that syntect's `HighlightLines::highlight_line()` is STATEFUL (processes one line at a time maintaining stack state). Because streaming passes one line per `render_line` call, a stateful `HighlightLines` instance CANNOT be reused across separate `render_line` calls without tracking per-code-block state. **The alloc is architecturally required for correctness** unless `MarkdownRender` stores `HighlightLines` state per active code block.

### Finding 7 — CONFIRMED: `render()` Vec collect → join per multi-line batch
`markdown.rs:67-73`: `text.split('\n').map(...).collect::<Vec<String>>().join("\n")` — for a batch with N completed lines: N String allocs + Vec<String> + final joined String. Optimization: use `String::with_capacity` + loop + `push_str` to avoid Vec<String> intermediate:
```rust
pub fn render(&mut self, text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut first = true;
    for line in text.split('\n') {
        if !first { out.push('\n'); }
        out.push_str(&self.render_line_mut(line));
        first = false;
    }
    out
}
```

### Finding 8 — CONFIRMED: `raw_stream` per-token flush (no batching)
`stream.rs:49-52`: One `stdout().flush()` per `SseEvent::Text` token. For a 1000-token response: ~333-1000 flush syscalls vs. ~20 with 50ms batching. This is only in the non-TTY path (piped output), but represents 100× more syscalls than `markdown_stream`.

### Finding 9 — CONFIRMED: Double `config.read()` RwLock in `render_stream`
`mod.rs:14-16`: Two consecutive `config.read()` calls where one would suffice. The `highlight` flag and `render_options` can be extracted in a single lock scope. Very minor overhead (lock acquisition is fast for read-mostly locks), but worth noting as a code quality issue.

### Finding 10 — CONFIRMED: `gather_events` is NOT truly 50ms per batch for slow models
`stream.rs:152-156`: The `tokio::select!` drains the MPSC until EITHER 50ms elapses OR the channel is empty (the `while let Some(...) = rx.recv().await` arm drains ALL available events before yielding). For very slow models (>50ms between tokens), each batch contains exactly 1 token. The 50ms timeout only provides batching for high-throughput streaming (sub-50ms token intervals). This is correct behavior but means `gather_events` doesn't guarantee batching in practice.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Remove `std::thread::sleep(10ms)` from `Spinner::set_message` and `Spinner::stop`; replace with `tokio::task::yield_now()` or simply remove (the sleep fires before the consumer task exists) | high | S | low | CONFIRMED — `spinner.rs:73,80` — 10-20ms blocking per request, fires before consumer task starts |
| 2 | Replace `buffer = format!("{buffer}{text}")` with `buffer.push_str(&text)` at `stream.rs:129`; refactor the newline branch at `stream.rs:121` to do `buffer.push_str(&text)` then `split_line_tail(&buffer)` | med | S | low | CONFIRMED — `stream.rs:121,129` — per-batch String copy of growing buffer |
| 3 | Eliminate `gather_events` triple-alloc: instead of `Vec<String>` + `join`, use a single `String::with_capacity` pre-allocated buffer, `push_str` each event, avoid `join` entirely | med | S | low | CONFIRMED — `stream.rs:162-168` — N+1 String allocs per 50ms batch |
| 4 | Replace `raw_stream` per-token `flush()` with buffered flush: accumulate text, call `flush()` only on `SseEvent::Done` (or at 50ms intervals for non-TTY streaming) | med | S | low | CONFIRMED — `stream.rs:49-52` — ~333-1000 flush syscalls per 1000-token response |
| 5 | Cache `SyntaxReference` clone in `check_line()` — pass current `code_syntax` as `Option<&SyntaxReference>` instead of cloning; update only on fence line transitions | low | M | low | CONFIRMED — `markdown.rs:95` — 1 String-containing clone per rendered line |
| 6 | Refactor `render()` to use a pre-allocated String + `push_str` instead of `collect::<Vec<String>>().join("\n")` | low | S | low | CONFIRMED — `markdown.rs:67-73` — N+1 String allocs per multi-line batch |
| 7 | Store `HighlightLines` in `MarkdownRender` as `Option<HighlightLines>` field (initialized at `CodeBegin`, reset at `CodeEnd`) and reuse across lines in the same code block | low | M | med | CONFIRMED need — `markdown.rs:143` — new HighlightLines per line; med-risk because syntect state must survive across `render_line` calls |
| 8 | Consolidate double `config.read()` in `render_stream` into one lock scope | negligible | S | low | CONFIRMED — `mod.rs:14-16` |

## Open Questions / Needs Verification

1. **`HighlightLines` initialization cost**: How much does `HighlightLines::new(syntax, theme)` actually cost in practice? If syntect's initialization is cheap (just copying references), optimization #7 is low-priority. Needs micro-benchmark.
2. **Spinner blocking sleep severity**: The 10ms blocking sleep calls `std::thread::sleep` inside tokio. In multi-threaded tokio runtime, this stalls ONE worker thread (not all). If the runtime has ≥2 threads, other tasks can still run. Impact depends on runtime configuration (`worker_threads`). Needs profiling under actual tool-call chains.
3. **`gather_events` token arrival pattern**: For typical LLM inference (30-100 tokens/sec), do most batches contain 1 token (slow models) or 5-15 tokens (fast models)? The batching overhead is only significant if batches contain multiple tokens. Needs real-world profiling.
4. **Does crossterm's `queue!` buffer internally?**: Calls to `queue!(writer, ...)` are buffered in the writer's internal buffer (not flushed until `writer.flush()`). The per-batch cost is therefore dominated by the single `flush()` call, not the number of `queue!` calls. This is likely already optimal for terminal output.
5. **`raw_stream` usage in practice**: Is `raw_stream` commonly used (non-TTY)? If aichat is typically run interactively (TTY), then `raw_stream`'s flush issue may be low-priority. In CI/pipeline/tool-chaining context it matters more.
6. **Spinner suppression in non-interactive contexts**: `IS_STDOUT_TERMINAL` guards `run_abortable_spinner` (`spinner.rs:154`) but NOT the blocking `Spinner::create()` sleep at `abortable_run_with_spinner`. The 10ms blocking sleep fires even when `IS_STDOUT_TERMINAL` is false. Verify whether `Spinner::create()` should early-exit when not a terminal.

## Hot-Path Classification

**Per-batch (50ms)**: `gather_events` allocs, `format!("{buffer}{text}")`, `cursor::position()` ioctl, `terminal::Clear` queue, `render_line` call, `writer.flush()`

**Per-line (per newline in batch)**: `render.render()` call, `check_line()` SyntaxReference clone, `HighlightLines::new()`, `render_line_mut()` call

**Per-request (once at start + stop)**: Spinner `std::thread::sleep(10ms)` × 2, `enable_raw_mode()`, `disable_raw_mode()`, double `config.read()`

**Per-token (hot, raw_stream only)**: `stdout().flush()` — 1 flush per SSE text event in non-TTY mode

**Hot-path priority** (highest to lowest for markdown_stream / streaming path):
1. Spinner 10ms blocking sleep × 2 = 20ms fixed overhead per request — highest wall-clock impact
2. `buffer = format!(...)` copy per batch — cumulative for long responses
3. `gather_events` triple-alloc — cumulative for high-throughput responses
4. `check_line` SyntaxReference clone per line — low absolute cost but per-line
5. `render()` Vec collect → join — minor
6. `HighlightLines::new` per line — minor (likely cheap init)
7. `cursor::position()` ioctl per batch — minimal
