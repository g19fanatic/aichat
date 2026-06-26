# SSE Stream Pipeline: Allocation Analysis

**Task ID**: 10  **Scope**: Per-chunk allocation cost, dual storage, String reallocations in SseHandler + provider streaming handlers + render pipeline

## Evidence Gathered

### SseHandler structure and text/done methods
- `src/client/stream.rs:11-14` — `SseHandler` holds:
  ```rust
  buffer: String,
  tool_calls: Vec<ToolCall>,
  ```
  `buffer` accumulates ALL text via `push_str` across the entire response lifetime.

- `src/client/stream.rs:28-45` — `SseHandler::text()`:
  ```rust
  self.buffer.push_str(text);          // O(1) amortized append
  self.sender.send(SseEvent::Text(text.to_string()))  // NEW String alloc per chunk
  ```
  **CONFIRMED**: Every SSE text chunk allocates a new `String` via `text.to_string()` even though `buffer` already has the content. This is a per-chunk allocation for the sole purpose of MPSC transmission.

- `src/client/stream.rs:72-76` — `SseHandler::take()`:
  ```rust
  let Self { buffer, tool_calls, .. } = self;
  (buffer, tool_calls)
  ```
  Moves `buffer` out — no clone here. Final result is the entire response text in one String.

### SseMmessage construction in sse_stream
- `src/client/stream.rs:100-104` — `sse_stream` moves event/data Strings:
  ```rust
  let message = SseMmessage {
      event: message.event,   // moves String from reqwest-eventsource
      data: message.data,     // moves String from reqwest-eventsource
  };
  ```
  No new allocation at this layer — moves ownership from the HTTP layer.

### OpenAI streaming handler — per-SSE-event work
- `src/client/openai.rs:125` — Full Value parse per SSE event:
  ```rust
  let data: Value = serde_json::from_str(&message.data)?;
  ```
  Every SSE event (text AND tool-call chunks) deserializes the FULL delta JSON into a heap-allocated `Value` tree. A 1000-token response ≈ 333 SSE events → 333 `Value` tree allocations.

- `src/client/openai.rs:153-155` — `format!` per tool-call SSE chunk:
  ```rust
  let maybe_call_id = format!("{}/{}", id.unwrap_or_default(), index.unwrap_or_default());
  ```
  Allocates a new String on EVERY tool-call SSE chunk to check call transitions. In a typical N-tool call with K argument chunks each, this is N×K new Strings.

- `src/client/openai.rs:181` — `name.to_string()` when function name starts with prefix:
  ```rust
  function_name = name.to_string();
  ```
  Re-allocates `function_name` when the streaming delta for `name` arrives (first chunk only).

- `src/client/openai.rs:188` — Efficient argument accumulation:
  ```rust
  function_arguments.push_str(arguments);
  ```
  O(1) amortized — correct pattern.

- `src/client/openai.rs:116` — One parse per tool-call at end of streaming:
  ```rust
  let arguments: Value = function_arguments.parse().with_context(|| ...)?;
  ```
  Single parse of accumulated arguments at call completion — NOT per chunk. Efficient.

### Claude streaming handler
- `src/client/claude.rs:85` — Same pattern, same cost:
  ```rust
  let data: Value = serde_json::from_str(&message.data)?;
  ```
  Claude confirms the `serde_json::from_str` per-event pattern is universal across providers.

- `src/client/claude.rs:124` — Argument accumulation in Claude:
  ```rust
  function_arguments.push_str(partial_json);
  ```
  Same efficient push_str pattern as OpenAI.

### gather_events — 50ms batching and allocation
- `src/render/stream.rs:152-167` — `gather_events()` batches events in a 50ms window:
  ```rust
  let mut texts = vec![];
  tokio::select! {
      _ = async {
          while let Some(reply_event) = rx.recv().await {
              match reply_event {
                  SseEvent::Text(v) => texts.push(v),  // collects N Strings into Vec
                  ...
              }
          }
      } => {}
      _ = tokio::time::sleep(Duration::from_millis(50)) => {}
  };
  if !texts.is_empty() {
      events.push(SseEvent::Text(texts.join("")))  // NEW merged String per batch
  }
  ```
  **CONFIRMED**: The render pipeline re-allocates. Per 50ms batch: 1× `Vec<String>` alloc, then `texts.join("")` creates ONE new merged String. N separate Strings (from `handler.text()`'s `to_string()`) are merged into a single new String and then discarded.

- `src/render/stream.rs:123,129` — `format!` in `markdown_stream_inner`:
  ```rust
  let text = format!("{buffer}{text}");  // line 123: newline case, new String
  buffer = format!("{buffer}{text}");    // line 129: no-newline case, new String
  ```
  **CONFIRMED**: Two O(n) String reallocations per render batch. `buffer` grows over the response. A 10KB response → average buffer size 5KB → ~5KB alloc per batch → O(response_size) total wasted allocation work.

### Dual-storage confirmed
- `src/client/stream.rs:13` + `src/client/stream.rs:36` — text bytes stored in BOTH:
  1. `self.buffer` (complete accumulation for `handler.take()` result)
  2. `SseEvent::Text(text.to_string())` → MPSC channel → render pipeline
  Peak memory = 2× response_text_size simultaneously.

### Tool call argument type after streaming
- `src/client/openai.rs:116,166` — `function_arguments.parse()` returns `Value::Object`
- Cross-reference with `src/function.rs:185` — `self.arguments.is_object()` fast path
  **CONFIRMED**: For SSE-streaming assembled tool calls, `arguments` is always `Value::Object` (parsed at end of streaming). The `jsonic` path in `ToolCall::eval` (function.rs:193) is NEVER taken for SSE calls. Task 5's fast-path claim is verified.

### raw_stream — no batching, one flush per chunk
- `src/render/stream.rs:49-52`:
  ```rust
  SseEvent::Text(text) => {
      print!("{text}");
      stdout().flush()?;  // syscall per chunk!
  }
  ```
  **CONFIRMED**: `raw_stream` has no 50ms batching. One `flush()` syscall per SSE chunk — significantly worse than `markdown_stream` (1 flush per 50ms batch). Used in non-terminal (pipe/redirect) scenarios.

## Findings

1. **Per-chunk `to_string()` allocation in `SseHandler::text()`** (CONFIRMED, `stream.rs:36`): Every SSE text chunk allocates a new owned `String` via `text.to_string()`. For a 1000-token response (~333 SSE chunks): 333 heap allocations. These allocations exist solely to traverse the MPSC channel and are discarded after render. Mitigation: use `Bytes` or a pre-allocated MPSC message pool, or use `SseEvent::Text(Box<str>)` (slightly smaller allocation).

2. **Per-event `serde_json::from_str` allocates full `Value` tree** (CONFIRMED, `openai.rs:125`, `claude.rs:85`): Every SSE event — including tiny 2-3 char text deltas — parses the entire delta JSON into a heap-allocated `Value`. Provider-universal. Per-chunk cost: ~100-500 bytes of `Value` tree allocation + deallocation. Total for 333 events: ~33-165KB of transient allocations.

3. **Dual-storage: text bytes live in 2 places simultaneously** (CONFIRMED, `stream.rs:13,36`): `buffer` accumulates the complete response. MPSC channel holds the same bytes as `SseEvent::Text` items awaiting render. Peak memory = 2× response_text_size. For a 10KB response = 20KB overhead. Not critical by itself, but indicates architecture could use a single-writer path.

4. **`gather_events` does `texts.join("")` per 50ms batch** (CONFIRMED, `render/stream.rs:162`): Collects N individual `to_string()` allocations into `Vec<String>`, then allocates a NEW merged String via `join("")`. Triple allocation pattern: N individual Strings → Vec → joined String → all N individual Strings dropped.

5. **`format!("{buffer}{text}")` is O(n) per render batch** (CONFIRMED, `render/stream.rs:123,129`): Two code paths both use `format!()` to create new Strings from `buffer + text`. `buffer` is the growing partial current line. As response grows: 1KB + 1KB → 2KB alloc, 2KB + 1KB → 3KB alloc, etc. Replaceable with `buffer.push_str(&text)` — zero-alloc append. This is an O(response²) total allocation pattern for the current line buffer.

6. **`maybe_call_id = format!(...)` per tool-call SSE chunk** (CONFIRMED, `openai.rs:155`): Allocates a String on EVERY tool-call SSE event to detect call boundaries. For 3 tools × 50 chunks each = 150 small String allocations per round-trip. Low absolute cost but unnecessary.

7. **`raw_stream` has per-chunk syscall** (CONFIRMED, `render/stream.rs:49-52`): `stdout().flush()` per SSE chunk in non-TTY mode. For pipe output (batch/scripting use), this is a syscall per token. Should batch and flush at `SseEvent::Done`.

8. **Tool-call argument accumulation is efficient** (CONFIRMED, `openai.rs:188`, `claude.rs:124`): `function_arguments.push_str(arguments)` — standard O(1) amortized String growth. Arguments are only parsed once at tool call completion. This is a GOOD pattern, not a hotspot.

9. **jsonic NOT invoked for SSE-streaming tool calls** (CONFIRMED cross-reference with `function.rs:185`): Since `function_arguments.parse()` returns `Value::Object`, `arguments.is_object()` is true in `ToolCall::eval`. The jsonic path only fires for String-type arguments, which requires the non-streaming case. SSE path bypasses jsonic entirely.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Replace `format!("{buffer}{text}")` with `buffer.push_str(&text)` in `markdown_stream_inner` (stream.rs:123,129) | med | S | low | CONFIRMED — eliminates O(buffer_len) allocation per render batch |
| 2 | Eliminate `texts.join("")` in `gather_events`: instead pass the `Vec<String>` directly or use `buffer.extend(texts.drain(..))` to avoid the joined String allocation | med | S | low | CONFIRMED — eliminates one extra alloc per 50ms batch |
| 3 | Batch `raw_stream` flushes: only flush on `SseEvent::Done` (or periodically), not every chunk | med | S | low | CONFIRMED — eliminates N flush syscalls in pipe/batch mode |
| 4 | Replace `text.to_string()` in `SseHandler::text()` with `Bytes::from(text.as_bytes().to_vec())` or use an MPSC message type holding `Box<str>` — OR consolidate SseHandler buffer+channel into a single write path | low | M | low | CONFIRMED — per-chunk String alloc; ~333 allocs for 1000-token response |
| 5 | Replace `serde_json::from_str(&message.data)` for text-only SSE events with a targeted pattern that extracts `content` field without full `Value` deserialization (e.g., simd_json or custom extractor) | high | L | high | CONFIRMED — full Value tree per event; all providers affected; ~333 allocs per response |
| 6 | Replace `format!("{}/{}", id, index)` with a stack-based comparison (compare id String + index u64 separately without allocating) | low | S | low | CONFIRMED — per tool-call SSE chunk String alloc |
| 7 | For `SseHandler`: use `String::with_capacity(estimated_response_size)` to pre-allocate `buffer` and avoid reallocs during streaming | low | S | low | LIKELY — reduces buffer growth reallocs for long responses |

## Open Questions / Needs Verification

- **`gather_events` 50ms window**: Does the window fire per-token (i.e., each token arrives before 50ms elapses) or does it batch multiple tokens? If the API returns tokens slowly, `gather_events` may fire per-token anyway, negating the batching benefit. Runtime measurement needed.
- **`serde_json::from_str` cost vs network latency**: Is the per-event parse cost dominated by network RTT? Need benchmarks. The parse IS overhead but may be << 1ms per event.
- **`buffer.push_str` fix in `markdown_stream_inner`**: Does the markdown renderer (`render.render_line()`) require the entire current line in `buffer` each call? If so, `push_str` is correct. If renderer has internal state, architecture changes might be needed.
- **`raw_stream` path usage frequency**: How often is aichat run in non-terminal (pipe) mode during agent workflows? If agents always run with terminal, `raw_stream` syscall cost is irrelevant.
- **Double-storage mitigation**: Could `SseHandler` be restructured to use a single MPSC channel for both rendering and final result extraction? This would eliminate the `buffer` duplication but requires a different consumer architecture.

## Hot-Path Classification

**Per-call (per SSE event)**: `serde_json::from_str(&message.data)` (Value tree), `text.to_string()` in `handler.text()`, `format!("{}/{}", ...)` for tool-call chunks

**Per-round-trip (per 50ms batch)**: `texts.join("")` in `gather_events`, `format!("{buffer}{text}")` in `markdown_stream_inner`, `writer.flush()` in markdown_stream, `cursor::position()` ioctl

**Per-round-trip (per streaming call)**: `SseHandler::new()` (buffer + vec alloc), `enable_raw_mode()`/`disable_raw_mode()` syscalls

**Cold-path**: `SseHandler::take()` (move, no alloc), `ToolCall::new()` with `Value::Object` arguments (one alloc at tool-call close)
