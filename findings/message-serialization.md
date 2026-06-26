# Message Serialization Per Round-Trip

**Task ID**: 8  **Scope**: Per-round-trip message serialization pipeline for all providers — redundant String materializations, message content transformations (patch_messages, extract_system_message, merge_prompt), and function declaration re-serialization.

## Evidence Gathered

- `src/config/input.rs:233-247` — `prepare_completion_data()` is called per HTTP round-trip:
  ```rust
  pub fn prepare_completion_data(&self, model: &Model, stream: bool) -> Result<ChatCompletionsData> {
      let mut messages = self.build_messages()?;       // 1. Vec<Message> rebuild
      patch_messages(&mut messages, model);             // 2. in-place mutation
      model.guard_max_input_tokens(&messages)?;        // 3. token count check
      let functions = self.config.read().select_functions(self.role()); // 4. FunctionDeclaration vec
      Ok(ChatCompletionsData { messages, ... })
  }
  ```

- `src/client/message.rs:220-238` — `patch_messages()` performs O(N) Vec::insert/remove operations:
  ```rust
  pub fn patch_messages(messages: &mut Vec<Message>, model: &Model) {
      if let Some(prefix) = model.system_prompt_prefix() {
          // Vec::insert(0, ...) shifts ALL elements O(M) cost
          messages.insert(0, Message { role: MessageRole::System, ... });
      }
      if model.no_system_message() && messages[0].role.is_system() {
          let system_message = messages.remove(0);  // O(M) shift
          if let (Some(message), system) = (messages.get_mut(0), system_message.content) {
              message.merge_system(system);
          }
      }
  }
  ```

- `src/client/message.rs:200-209` — `extract_system_message()` for Claude/Gemini is O(N) Vec::remove:
  ```rust
  pub fn extract_system_message(messages: &mut Vec<Message>) -> Option<String> {
      if messages[0].role.is_system() {
          let system_message = messages.remove(0);  // O(M) element shift every round
          return Some(system_message.content.to_text());
      }
      None
  }
  ```
  Called at start of `claude_build_chat_completions_body` and `gemini_build_chat_completions_body` every round.

- `src/client/openai.rs:256,267` — `arguments.to_string()` and `output.to_string()` per tool result:
  ```rust
  // For EVERY accumulated tool_result (from ALL rounds) every round-trip:
  "arguments": tool_result.call.arguments.to_string(),  // line 256: Value -> JSON String
  "content": tool_result.output.to_string(),            // line 267: Value -> JSON String
  ```
  Also at lines 283 and 290 for sequence path. At R=10, N=3 tools: 30 `.to_string()` calls per round-trip just for history replay.

- `src/client/claude.rs:241` — Claude also serializes output: `"content": tool_result.output.to_string()`

- `src/client/vertexai.rs` (gemini_build_chat_completions_body) — double traversal for ToolCalls:
  ```rust
  let model_parts: Vec<Value> = tool_results.iter().map(|tool_result| {
      json!({ "functionCall": { ... } })
  }).collect();       // First pass: immutable borrow
  let function_parts: Vec<Value> = tool_results.into_iter().map(|tool_result| {
      json!({ "functionResponse": { ... } })
  }).collect();       // Second pass: consuming
  ```
  Two Vec allocations (model_parts + function_parts) instead of one combined loop.

- `src/client/common.rs:138-172` — `patch_request_data()` called every request, reads `env::var()` unconditionally:
  ```rust
  fn patch_request_data(&self, request_data: &mut RequestData) {
      if let Some(patch) = self.model().patch() { request_data.apply_patch(patch.clone()); }
      let patch_map = std::env::var(get_env_name(&format!(
          "patch_{}_{}", self.model().client_name(), model_type.api_name()
      ))).ok()...     // env::var() + potential regex compilation per request
  }
  ```

- `src/config/mod.rs:1652-1680` — `select_functions()` builds TWO HashSets per round-trip:
  ```rust
  pub fn select_functions(&self, role: &Role) -> Option<Vec<FunctionDeclaration>> {
      let declaration_names: HashSet<String> = self.functions.declarations().iter()
          .map(|v| v.name.to_string()).collect();   // HashSet of K name Strings
      let mut tool_names: HashSet<String> = Default::default();
      // ... filtering logic ...
      functions = self.functions.declarations().iter()
          .filter(|v| tool_names.contains(&v.name))
          .cloned()                 // Deep clone of FunctionDeclaration (includes Value parameters)
          .collect();
  ```
  K function names → K String allocs for HashSet + K FunctionDeclaration deep clones per round-trip.

- `src/client/openai.rs:315-322` — Function declarations re-serialized every round-trip:
  ```rust
  if let Some(functions) = functions {
      body["tools"] = functions.iter().map(|v| json!({
          "type": "function",
          "function": v,    // serde_json serialization of FunctionDeclaration per function
      })).collect();
  }
  ```
  K `json!()` macro calls = K `serde_json::to_value()` equivalents per round-trip.

- `src/config/input.rs:260` — `tool_calls.clone()` in `build_messages()`:
  ```rust
  if let Some(tool_calls) = &self.tool_calls {
      messages.push(Message::new(
          MessageRole::Assistant,
          MessageContent::ToolCalls(tool_calls.clone()),  // Deep clone of ALL accumulated results
      ))
  }
  ```
  Confirmed from Task 7 as O(R²×N×size) cost — this is the worst serialization-related allocation.

- `src/client/common.rs:249-260` — `into_builder()` calls `builder.json(&body)` exactly ONCE:
  ```rust
  pub fn into_builder(self, client: &ReqwestClient) -> RequestBuilder {
      let RequestData { url, headers, body } = self;
      let mut builder = client.post(url);
      for (key, value) in headers { builder = builder.header(key, value); }
      builder = builder.json(&body);   // ONE serde_json serialization pass to bytes
      builder
  }
  ```
  No double-serialization in the outbound path. The `Value` → HTTP bytes is exactly once.

## Findings

1. **No double JSON-to-bytes serialization** (CONFIRMED): The pipeline is `Vec<Message>` → `Value` (via `json!()` macro in build_chat_completions_body) → HTTP bytes (via `builder.json(&body)` once in `into_builder`). There is no String → JSON → String round-trip. The concern about "redundant serialization" from a bytes perspective is NOT confirmed.

2. **`tool_result.arguments.to_string()` and `output.to_string()` are O(R×N) per round** (CONFIRMED): OpenAI at `openai.rs:256,267,283,290` and Claude at `claude.rs:241` call `.to_string()` on the accumulated `ToolResult` collection. At round R, there are R×N total tool_results, so each round-trip does R×N serializations of tool arguments/outputs to JSON strings. These strings are protocol-mandated (OpenAI requires arguments as a string, not object), so this is unavoidable at the API format level — but the data being re-serialized grows quadratically.

3. **`extract_system_message` is O(M) Vec::remove every round for Claude/Gemini** (CONFIRMED): Called unconditionally in `claude_build_chat_completions_body` and `gemini_build_chat_completions_body`. For M=60 messages (20 chat chains × 3 messages), this shifts 59 elements per round-trip. For OpenAI, this overhead does NOT exist — OpenAI keeps system message in the array.

4. **`patch_messages()` has conditional O(M) Vec::insert/remove** (CONFIRMED): Only fires for models with `system_prompt_prefix()` or `no_system_message()` — likely rare. Cost is O(M) element shift. Normal models incur ZERO cost from this.

5. **`select_functions()` builds 2 HashSets + K FunctionDeclaration clones per round** (CONFIRMED, `mod.rs:1652`): The `declaration_names: HashSet<String>` and `tool_names: HashSet<String>` are built fresh every round. The final `.cloned()` on `FunctionDeclaration` deep-copies K function definitions including their `parameters: Value` (a full serde_json tree). For K=50 functions, this is 100 String allocs + 50 Value tree clones per round.

6. **`FunctionDeclaration` re-serialized via `json!()` macro per function per round** (CONFIRMED): OpenAI at `openai.rs:315`, Claude at `claude.rs:280`, Gemini at `vertexai.rs` — every client calls `json!(v)` or `json!({...v...})` on each FunctionDeclaration, triggering K serde_json serializations per round. This is O(K) redundant work since functions never change.

7. **Gemini double-traversal of ToolResults** (CONFIRMED, `vertexai.rs:~360-400`): Two separate Vec allocations (`model_parts` from `iter()`, `function_parts` from `into_iter()`) instead of one combined pass. Medium alloc overhead for tool-heavy agents.

8. **`patch_request_data` reads `env::var()` unconditionally every request** (CONFIRMED, `common.rs:142-148`): `std::env::var(get_env_name(...))` fires even when no patch is configured. This adds 1 syscall per HTTP request.

9. **No caching of `Value` body across rounds** (CONFIRMED by inspection): The entire body (`{model, messages, tools, ...}`) is reconstructed from scratch every round-trip. No incremental update pattern. The only stable part (function declarations) is the most expensive to re-serialize.

10. **`merge_prompt` allocates new String per call** (CONFIRMED, `message.rs:130-142`): Called during role system prompt injection. Creates a new String via the replace_fn lambda. Called once per `prepare_completion_data`, not per message. Low overhead.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | **Cache serialized `tools` Value** — serialize function declarations once after `select_functions`, cache as `Option<Arc<Value>>` keyed on function set; reuse across rounds | med | M | low | CONFIRMED: K serde_json serializations per round, K≈50 common in agents |
| 2 | **Cache `select_functions` result** — `FunctionDeclaration` set is identical every round in agent loop; cache the `Vec<FunctionDeclaration>` in Input instead of rebuilding from HashSet each time | med | S | low | CONFIRMED: `mod.rs:1652` rebuilds 2 HashSets + K clones per round |
| 3 | **Single-pass Gemini tool_results** — combine `model_parts` and `function_parts` into one `flat_map` pass returning both `Value`s per `tool_result` | low | S | low | CONFIRMED: double Vec alloc in `gemini_build_chat_completions_body` |
| 4 | **Cache `env::var` patch lookup** — move `patch_request_data` env::var() read to initialization, store as `Option<ApiPatch>` | low | S | low | CONFIRMED: `common.rs:142` fires every request |
| 5 | **Arc<[ToolResult]> for tool_calls** — change `MessageContentToolCalls.tool_results` from `Vec<ToolResult>` to `Arc<Vec<ToolResult>>` — clone is O(1) pointer copy instead of O(R×N×size) deep copy | high | M | med | CONFIRMED: `input.rs:260` deep-clones ALL accumulated results each build_messages call; synergizes with Task 7 findings |
| 6 | **Claude/Gemini system message optimization** — don't pass system message as first Vec element; store separately in `ChatCompletionsData` to avoid `extract_system_message` Vec::remove(0) every round | med | M | med | CONFIRMED: `message.rs:200-209` O(M) shift per round for Claude/Gemini |
| 7 | **Pre-serialize `FunctionDeclaration` to `Value`** during `Functions::init` — store as `Vec<Value>` alongside the struct so serialization is cold-path once | med | M | low | CONFIRMED: K `json!()` calls per round-trip per client |

## Open Questions / Needs Verification

- Does `select_functions` actually fire every round in agent loops? In theory `role.use_tools()` is stable across rounds — if `prepare_completion_data` is refactored to hoist functions selection above the tool-call loop, it would be a zero-cost win. Needs verification of call site at `main.rs` agent recursion path.
- What is the typical K (number of function declarations) for real-world agents? K=5-10: alloc cost is negligible. K=50-100: the HashSet + clone overhead becomes meaningful. Need a benchmark with a large agent.
- Is `patch_request_data`'s `env::var` check actually hot? It fires per request but it's a HashMap lookup in the kernel table — may be faster than the allocations above. Not a top priority.
- For OpenAI/Claude: the `tool_result.arguments.to_string()` and `output.to_string()` are protocol-required. The question is whether there's a way to cache the string representation once when the tool result is first received (at `eval_tool_calls` time) and reuse it. This would convert O(R×N) serializations to O(N) per chain.
- Gemini uses `tool_result.output` directly (as a `Value`, not `.to_string()`) in `functionResponse.content` — this is more efficient than OpenAI/Claude which stringify. Verify this is actually accepted by the Gemini API.

## Hot-Path Classification

**Per-round-trip** (hot path in tool-call chains):
- `tool_result.arguments.to_string()` × N×R (grows with rounds)
- `extract_system_message` Vec::remove (Claude/Gemini only)
- `select_functions` HashSet × 2 + FunctionDeclaration::clone × K
- `FunctionDeclaration` json!() × K per client
- `patch_request_data` env::var()

**Per-chain (once at chain end)**:
- `build_messages` tool_calls.clone() (deep, but happens every round via Input::clone)

**Cold-path**:
- `patch_messages` Vec::insert/remove (only for rare model-specific configs)
- `merge_prompt` String alloc (once per prepare)
- `into_builder` json() bytes serialization (efficient, not redundant)
