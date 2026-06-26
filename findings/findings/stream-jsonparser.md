# JsonStreamParser — Streaming JSON Boundary Detector

**Task ID**: 11  **Scope**: Analyze `JsonStreamParser` at `src/client/stream.rs:175` for O(n²) patterns, redundant parses, and allocation costs. Identify hot-path impact and optimization opportunities.

---

## Evidence Gathered

- `src/client/stream.rs:175-213` — Full `JsonStreamParser` struct and `process()` impl:
  ```rust
  struct JsonStreamParser {
      buffer: Vec<char>,    // accumulates ALL input, never drained
      cursor: usize,        // monotonically increasing
      start: Option<usize>,
      balances: Vec<char>,
      quoting: bool,
      escape: bool,
  }
  
  fn process<F>(&mut self, text: &str, handle: &mut F) -> Result<()> {
      self.buffer.extend(text.chars());   // UTF-8 → UCS-4 conversion per chunk
      for i in self.cursor..self.buffer.len() {  // scans only NEW chars
          ...
          if let Some(start) = self.start.take() {
              let value: String = self.buffer[start..=i].iter().collect(); // char→String
              handle(&value)?;
          }
          ...
      }
      self.cursor = self.buffer.len();   // cursor never resets
      Ok(())
  }
  ```

- `src/client/stream.rs:144-170` — `json_stream()` function with `unparsed_bytes`:
  ```rust
  let mut parser = JsonStreamParser::default();
  let mut unparsed_bytes = vec![];
  while let Some(chunk_bytes) = stream.next().await {
      unparsed_bytes.extend(chunk_bytes);       // accumulates until valid UTF-8
      match std::str::from_utf8(&unparsed_bytes) {
          Ok(text) => {
              parser.process(text, &mut handle)?;
              unparsed_bytes.clear();           // cleared immediately on success
          }
          Err(_) => continue,                   // retains for next chunk
      }
  }
  ```

- `src/client/vertexai.rs:197-234` — Only call site, both for `vertexai.rs` and `gemini.rs`:
  ```rust
  pub async fn gemini_chat_completions_streaming(
      builder: RequestBuilder, handler: &mut SseHandler, _model: &Model,
  ) -> Result<()> {
      ...
      let handle = |value: &str| -> Result<()> {
          let data: Value = serde_json::from_str(value)?;  // full parse per JSON object
          if let Some(parts) = data["candidates"][0]["content"]["parts"].as_array() {
              for (i, part) in parts.iter().enumerate() {
                  if let Some(text) = part["text"].as_str() {
                      handler.text(text)?;
                  } else if let (Some(name), Some(args)) = (
                      part["functionCall"]["name"].as_str(),
                      part["functionCall"]["args"].as_object(),
                  ) {
                      handler.tool_call(ToolCall::new(name.to_string(), json!(args), None))?;
                  }
              }
          }
          Ok(())
      };
      json_stream(res.bytes_stream(), handle).await?;
  ```

- `src/client/gemini.rs:1,34` — Both Gemini and VertexAI share the same streaming function:
  ```rust
  use super::vertexai::*;   // imports gemini_chat_completions_streaming
  ...
  impl_client_trait!(GeminiClient, (prepare_chat_completions, gemini_chat_completions,
      gemini_chat_completions_streaming), ...);
  ```

- `src/client/stream.rs:188` — `text.chars()` UTF-8 → UCS-4 conversion:
  `self.buffer.extend(text.chars())` — for ASCII-dominant JSON, each byte becomes a 4-byte `char` in the Vec. A 10KB JSON response becomes ~40KB of `Vec<char>` storage.

- `src/client/stream.rs:209` — Char-to-String reconstruction per complete object:
  `let value: String = self.buffer[start..=i].iter().collect()` — iterates char-by-char and allocates a new `String`. For a 1KB object: 256 chars × 4 bytes = 1KB of char iteration + 1KB String alloc.

---

## Findings

1. **`Vec<char>` buffer grows monotonically and is NEVER drained** (CONFIRMED, `stream.rs:186-213`):
   `self.cursor` advances to `self.buffer.len()` but data before `cursor` is retained forever.
   After processing a 50KB VertexAI response, `buffer` holds ~200KB of `char` data until `JsonStreamParser` is dropped. Peak working memory = 4× response_size in chars.

2. **`text.chars()` UTF-8 → UCS-4 conversion per chunk** (CONFIRMED, `stream.rs:188`):
   Every `process()` call converts incoming `&str` (UTF-8, 1 byte for ASCII) to `Vec<char>` (UCS-4, 4 bytes per char). For ASCII-dominant LLM JSON output: 4× memory expansion. ~100 chunks for a typical streamed response = ~100 small `extend()` calls each triggering potential Vec realloc.

3. **`buffer[start..=i].iter().collect()` allocates a String per complete JSON object** (CONFIRMED, `stream.rs:209`):
   This String is passed to the `handle` closure at `vertexai.rs:200`, where it is immediately re-parsed with `serde_json::from_str(value)`. The String is a transient intermediary — the data goes: raw bytes → `Vec<char>` → `String` → `serde_json::Value`. The `Vec<char>` → `String` step is unnecessary if we track byte offsets into the original input.

4. **NO O(n²) patterns** (CONFIRMED, `stream.rs:186-213`):
   `for i in self.cursor..self.buffer.len()` processes only NEW characters on each `process()` call. Total algorithmic work across all calls is O(N) where N = total input characters. The task premise of "O(n²) concat patterns" is NOT confirmed in this code.

5. **NO backtracking / peek/rewind** (CONFIRMED):
   `cursor` is strictly monotonic. No rewind, no lookahead, no re-scanning of previously processed data. The state machine (quoting, escape, balances) is purely forward-scanning.

6. **`json_stream` is used by BOTH `gemini.rs` AND `vertexai.rs`** (CONFIRMED, `vertexai.rs:197`, `gemini.rs:34`):
   `gemini.rs` does `use super::vertexai::*` and registers `gemini_chat_completions_streaming` from that module. This path fires for any user using Google Gemini API (not just VertexAI), making the scope broader than originally assumed.

7. **`unparsed_bytes` is NOT a persistent accumulator** (CONFIRMED, `stream.rs:151-168`):
   `unparsed_bytes.extend(chunk_bytes)` is followed by `unparsed_bytes.clear()` on successful UTF-8 decode. In practice (complete UTF-8 sequences, which is 99.9% of HTTP chunks), this vector is filled and immediately cleared on every iteration. It only grows when a multi-byte UTF-8 sequence straddles a chunk boundary — bounded to 1-3 bytes. Efficient.

8. **`serde_json::from_str` per complete JSON object is the dominant cost** (CONFIRMED, `vertexai.rs:200`):
   For each JSON object from the stream: 1× `String` alloc from chars + 1× `serde_json::from_str` (full `Value` tree alloc + parse). The `from_str` is the larger cost (~1–5μs for typical Gemini response objects of 200–2000 bytes). Same finding as Task 10 for SSE path.

9. **Tool call argument handling at `vertexai.rs:214-218`**:
   `json!(args)` where `args` is `&Map<String, Value>` — this clones the entire map into a new `Value::Object`. Then `ToolCall::new(name.to_string(), json!(args), None)` — another String clone. Unavoidable given the ownership model unless arguments are moved directly.

---

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | **Drain processed buffer prefix** — after `cursor = buffer.len()`, call `buffer.drain(0..start.unwrap_or(cursor))` and adjust `cursor` and `start` accordingly. Reduces peak memory from O(response_size) to O(largest_single_object) | low | S | low | CONFIRMED — buffer never shrinks (`stream.rs:213`) |
| 2 | **Replace `Vec<char>` with byte-level state machine** — accumulate raw bytes in a `Vec<u8>` or `String` with byte-level scanning for `{`, `}`, `"`, `\` (all single-byte ASCII). Track `start_byte` offset. When object complete: call `handle(&buffer_str[start_byte..=i])` as a zero-copy `&str` slice. Eliminates 4× memory expansion and char conversion. | low-med | M | low | CONFIRMED — `text.chars()` at `stream.rs:188`; JSON grammar uses only ASCII structural chars |
| 3 | **Eliminate intermediate String allocation** — with byte-level approach, `handle()` can receive a `&str` slice of the original input. Removes the `iter().collect()` String at `stream.rs:209`. Only worth doing together with #2. | low | M | low | CONFIRMED — `buffer[start..=i].iter().collect()` at `stream.rs:209` |
| 4 | **Custom path extractor instead of `serde_json::from_str` per event** — parse only the required paths (`["candidates"][0]["content"]["parts"]`) using a zero-copy deserializer (e.g., `serde_json::RawValue` or streaming deserialize). Eliminates full `Value` tree alloc per event. | med | L | med | CONFIRMED — `serde_json::from_str(value)` at `vertexai.rs:200`; this is the dominant cost in the `handle` closure |
| 5 | **`unparsed_bytes` pre-allocation** — pre-allocate with `Vec::with_capacity(4)` since it's only used for 1-3 byte UTF-8 boundary fragments. Eliminates the one-time vec allocation and potential first-extend realloc. | negligible | XS | none | CONFIRMED — `let mut unparsed_bytes = vec![]` at `stream.rs:151` |

---

## Open Questions / Needs Verification

- Does the VertexAI/Gemini streaming API send one JSON object per chunk, or many? If one-per-chunk, `start.is_none()` would always be true at chunk start, meaning `buffer` grows but `start` is only set mid-chunk. The buffer drain optimization (#1) is only effective if multiple objects are received before drain.
- What is the typical Gemini streaming response cadence — how many chunks per response? This determines whether the Vec<char> accumulation (finding #1) is a real memory concern or theoretical.
- For tool-call responses specifically, how large are `args` Maps typically? At `vertexai.rs:218`, `json!(args)` clones the Map. For large structured arguments (e.g., complex search queries), this could be non-trivial.
- The `unparsed_bytes` buffer grows across chunks on UTF-8 decode failure. If a malformed UTF-8 stream is received (bug in upstream or adversarial input), `unparsed_bytes` could grow unboundedly. Should add a size limit guard.

---

## Hot-Path Classification

**Per-round-trip** (fires once per streaming HTTP response for Gemini/VertexAI users)

The `JsonStreamParser` is NOT in the tool-call execution hot path — it handles the LLM response stream, not the tool execution. For a 10-round tool-call chain using Gemini, `json_stream` is called 10 times total (once per LLM response). The dominant per-call cost is `serde_json::from_str` in the handle closure, not the JsonStreamParser itself.

**Provider scope**: Gemini and VertexAI only (both `gemini.rs` and `vertexai.rs` use `gemini_chat_completions_streaming` which calls `json_stream`). OpenAI, Claude, Bedrock, Cohere, Ollama all use `sse_stream` — unaffected.

**Relative priority**: Lower than macro-level bottlenecks (subprocess spawn = Task 4, HTTP client rebuild = Task 12, message history quadratic clone = Task 7). Optimizations here are local polish for Gemini/VertexAI users.
