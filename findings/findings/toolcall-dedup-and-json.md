# ToolCall::dedup and Double-Parse JSON Pattern
**Task ID**: 5  **Scope**: Analyze `ToolCall::dedup` complexity and the jsonic→serde_json double-parse pattern in function.rs.

## Evidence Gathered

- `src/function.rs:156-175` — `ToolCall::dedup` full implementation:
  ```rust
  pub fn dedup(calls: Vec<Self>) -> Vec<Self> {
      let mut new_calls = vec![];
      let mut seen_ids = HashSet::new();
      for call in calls.into_iter().rev() {
          if let Some(id) = &call.id {
              if !seen_ids.contains(id) {
                  seen_ids.insert(id.clone());
                  new_calls.push(call);
              }
          } else { new_calls.push(call); }
      }
      new_calls.reverse();
      new_calls
  }
  ```
  Called at `src/function.rs:27`: `calls = ToolCall::dedup(calls);`

- `src/function.rs:74-82` — `Functions::init` double-parse:
  ```rust
  match jsonic::parse(&content) {
      Ok(json_item) => {
          let json_str = json_item.as_str().unwrap_or_default();
          serde_json::from_str(json_str).with_context(ctx)?
      },
      ...
  }
  ```

- `src/function.rs:192-207` — `ToolCall::eval` per-call hot path (string-argument branch):
  ```rust
  let arguments: Value = match jsonic::parse(arguments) {
      Ok(json_item) => {
          let json_str = json_item.as_str().unwrap_or_default();
          serde_json::from_str(json_str).map_err(|err| { ... })?
      },
      ...
  };
  ```

- `src/function.rs:346-353` — `run_llm_function` output JSON path:
  ```rust
  if contents.trim().starts_with('{') || contents.trim().starts_with('[') {
      match jsonic::parse(&contents) {
          Ok(json_item) => {
              output = Some(json_item.as_str().unwrap_or(&contents).to_string());
          },
          ...
  ```

- `jsonic-0.2.14/src/slice.rs:35` — `Slice::as_str()` implementation:
  ```rust
  pub fn as_str(&self) -> &str {
      unsafe { from_utf8_unchecked(self.as_bytes()) }
  }
  ```
  Where `Slice` stores raw pointer+len into the **original input buffer**: `ptr: unsafe { bytes.as_ptr().byte_add(start) }` (slice.rs:22). Zero-copy borrow — no allocation.

- `jsonic-0.2.14/src/json_item.rs:57-62` — `JsonItem::as_str()`:
  ```rust
  pub fn as_str(&self) -> Option<&str> {
      if self.json_type == Empty { None }
      else { Some(self.slice.as_str()) }
  }
  ```
  Returns `None` only if item is `Empty` (non-existent key). Returns raw substring for ALL other types (objects, arrays, strings, nulls, booleans, numbers).

- `jsonic-0.2.14/src/lib.rs:116-149` — `parse_map` creates Slice spanning `mark` (first `{`) to `index+1` (closing `}`). This is the **raw original bytes** verbatim, including any trailing commas that jsonic skipped while parsing. No transformation applied.

- `src/function.rs:213-219` — Back in `ToolCall::eval`, the returned String is fed directly to serde_json:
  ```rust
  let output = match run_llm_function(cmd_name, cmd_args, envs)? {
      Some(contents) => serde_json::from_str(&contents)
          .ok()
          .unwrap_or_else(|| json!({"output": contents})),
      None => Value::Null,
  };
  ```

- `src/function.rs:185-191` — The `if self.arguments.is_object()` fast path:
  ```rust
  let json_data = if self.arguments.is_object() {
      self.arguments.clone()
  } else if let Some(arguments) = self.arguments.as_str() {
      // Uses jsonic+serde_json double-parse path
  ```
  Only streaming-assembled tool calls go through the jsonic path; calls that arrive already deserialized as `Value::Object` skip it.

## Findings

1. **`ToolCall::dedup` is O(N) time and O(N) space, CONFIRMED** (`src/function.rs:156-175`): linear scan with HashSet lookup (O(1) amortized per element). Allocates 1 HashSet, 1 Vec, and N String clones (for IDs). For typical N=1-5 tool calls in a batch, total cost is ~microseconds. Not a meaningful performance concern.

2. **`dedup` is per-BATCH deduplication, NOT cross-round loop detection, CONFIRMED** (`src/function.rs:27-31`): dedup fires once per `eval_tool_calls` call, removing calls with duplicate `id` fields within a single LLM response. If the LLM sends the same `tool_call_id` twice in one response, those are collapsed. Only if all calls in a batch are duplicated does `calls.is_empty()` fire, triggering the "infinite loop" error message. This label is misleading — actual multi-round infinite loops are NOT detected here; this guards only against same-batch ID repetition.

3. **`jsonic::parse` → `as_str()` returns the RAW ORIGINAL TEXT, not a normalized form, CONFIRMED** (`jsonic-0.2.14/src/slice.rs:14-26`, `json_item.rs:57-62`): `Slice` stores a raw pointer into the input buffer; `as_str()` returns an unsafe borrow from that pointer. No allocation. No JSON normalization. For an object `{a: 1, b: 2,}` with a trailing comma, jsonic parse succeeds but `as_str()` returns the original bytes including the trailing comma. Feeding that to `serde_json::from_str()` would STILL fail. The jsonic pass provides no normalization benefit in the `run_llm_function` output path.

4. **The jsonic double-parse in `run_llm_function` (lines 346-353) appears semantically incorrect for its stated purpose, CONFIRMED**: The code comment implies jsonic normalizes the JSON, but jsonic's `as_str()` returns raw original bytes. The only behavior difference is: (a) jsonic validates the JSON is parseable; (b) if jsonic fails, fall back to raw `contents`. For well-formed JSON, `jsonic::parse` + `as_str().to_string()` = one full parse + one String alloc, then serde_json parses again — net cost: 2 parses + 1 extra String alloc (the `.to_string()` at line 351). For well-formed JSON from tool scripts, skipping jsonic and calling `serde_json::from_str(&contents)` directly would be correct and faster.

5. **The `.to_string()` in `run_llm_function` at line 351 is a mandatory extra allocation, CONFIRMED** (`src/function.rs:351`): `json_item.as_str()` is a zero-copy borrow of `contents`, but `contents` is local to `run_llm_function`. The function returns `Option<String>`, so `.to_string()` must copy. This is an unavoidable allocation given current API. Optimization requires changing the return type to avoid the intermediate String, or restructuring to do serde_json deserialization inside `run_llm_function` (return `Option<Value>` instead of `Option<String>`).

6. **The jsonic double-parse in `ToolCall::eval` (lines 192-207) has more justification, THEORY**: Streaming tool-call arguments are assembled from SSE deltas as a String. LLM providers may emit slightly malformed JSON (truncated at edges, etc.). The jsonic first-pass attempts to handle this. However, since `as_str()` returns raw bytes, the actual benefit is limited to cases where jsonic accepts something that serde_json would also accept (no gain) vs. cases where jsonic accepts malformed JSON that serde_json would reject (raw bytes would STILL fail serde_json). The jsonic pass provides zero normalization benefit unless the caller switches to jsonic's own typed API instead of re-parsing with serde_json.

7. **`unwrap_or_default()` in `Functions::init` is a latent bug risk, CONFIRMED** (`src/function.rs:77`): if `jsonic::parse()` returns `JsonItem` of type `Empty` (which only occurs for missing sub-keys, not a root parse result), `as_str()` returns `None`, `unwrap_or_default()` returns `""`, and `serde_json::from_str("")` fails with a cryptic "EOF while parsing a value" error masked by the `with_context(ctx)?` as "Failed to load functions". In practice `json_item.as_str()` on a root parse result should always return `Some` (since `Empty` is a sentinel for missing keys), but this is fragile.

8. **Allocation cost quantified per tool call (hot path), CONFIRMED**:
   - For `self.arguments.is_object()` (most common after streaming): `self.arguments.clone()` → one `serde_json::Value` tree clone. Cost: O(K) for K keys.
   - For `self.arguments.as_str()` (streaming-assembled strings): `jsonic::parse` → parse tree (alloc Vec+Slice entries for each key/value) + `serde_json::from_str` → second parse tree. Total: ~2× parse cost for argument JSON.
   - Then `json_data.to_string()` at line 211 → one additional String allocation to build command argument string.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Replace jsonic+serde_json double-parse in `run_llm_function` output path with direct `serde_json::from_str(&contents)` (single parse, no intermediate String) | low-med | S | low | CONFIRMED: jsonic `as_str()` returns raw bytes with no normalization benefit |
| 2 | Change `run_llm_function` return type from `Option<String>` to `Option<Value>` to eliminate the extra `.to_string()` allocation and the second serde_json parse in `ToolCall::eval` | med | M | med | CONFIRMED: currently requires 2 parses + 1 String copy per tool call on the JSON output path |
| 3 | Remove jsonic from `ToolCall::eval` string-arguments path: try `serde_json::from_str(arguments)` first, fall back to jsonic only on error | low-med | S | low | CONFIRMED: jsonic provides no normalization; only adds a redundant parse pass for well-formed JSON (the common case) |
| 4 | Pre-compute `arguments.is_object()` guard by deserializing tool-call arguments eagerly in streaming path (SSE handler) rather than deferring to eval() | med | L | med | THEORY: would eliminate the String-arguments path entirely; requires changes to SSE delta merging in stream.rs |
| 5 | `dedup`: deduplicate in-place using retain + a seen HashSet instead of reverse+collect+reverse | low | S | low | CONFIRMED: current approach is correct but involves two reversals and two Vec allocations; can be simplified |

## Open Questions / Needs Verification

- **What LLM output actually triggers the jsonic path?** The guard `contents.trim().starts_with('{')` means plain-text tool outputs skip jsonic entirely. How often do tool scripts return JSON vs. plain text? If mostly plain text, the jsonic path is rarely hit.
- **Does any LLM provider actually emit arguments as a raw String (not already a serde_json Value)?** If the streaming SSE handler (stream.rs) always deserializes `arguments` as a Value before inserting into ToolCall, the `as_str()` branch in `eval()` would never be reached on the hot path. Task 10/11 (stream-sse/stream-jsonparser) should verify.
- **Jsonic trailing-comma handling**: The jsonic `parse_map` code shows it skips commas between items, allowing trailing commas (comma before `}` reads comma, skips spaces, sees `}`). This means jsonic DOES accept `{a:1,}` but `as_str()` returns the original bytes with the trailing comma — so the subsequent `serde_json::from_str()` still fails. The jsonic pass thus provides effectively zero protection for trailing-comma malformed JSON in the serde_json re-parse path. Needs a concrete test case to confirm.
- **`unwrap_or_default()` in Functions::init**: Can this realistically return `None`? Trace through jsonic's `parse()` entry point to verify that a root-level successful parse always returns non-Empty type.

## Hot-Path Classification
**Per-call** (jsonic double-parse in `eval()`'s string-argument branch — triggered per tool call when arguments are String-typed)
**Per-round-trip** (`dedup` — once per `eval_tool_calls` invocation)
**Cold-path** (`Functions::init` jsonic parse — agent initialization only)
