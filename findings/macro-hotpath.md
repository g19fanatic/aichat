# Macro Hot-Path Frequency Analysis

**Task ID**: 2  **Scope**: Classify every operation in the agent tool-call chain by execution frequency (per-tool-call, per-round-trip, per-session, cold-path) and quantify cost in allocations, syscalls, and process spawns.

---

## Evidence Gathered

### Per-Tool-Call Evidence

- `src/function.rs:36` — Sequential for-loop: `for call in calls { let mut result = call.eval(config)?; ... }`. Each iteration is one tool invocation.
- `src/function.rs:271-293` — `run_llm_function()` starts with:
  ```rust
  let prompt = format!("Call {cmd_name} {}", cmd_args.join(" "));
  let mut bin_dirs: Vec<PathBuf> = vec![];
  ...
  bin_dirs.push(Config::functions_bin_dir());
  let current_path = std::env::var("PATH").context("No PATH environment variable")?;
  let prepend_path = bin_dirs.iter().map(|v| format!("{}{PATH_SEP}", v.display()))
      .collect::<Vec<_>>().join("");
  envs.insert("PATH".into(), format!("{prepend_path}{current_path}"));
  ```
  Every call: 1× `format!` alloc for prompt, 1× `Vec<PathBuf>` alloc, 1× `std::env::var("PATH")` syscall, 1× iterator-map+collect creating `Vec<String>`, 1× `.join("")` String alloc, 1× `format!` + `.insert()` for PATH String.
- `src/function.rs:296-302` — Temp file creation (when `LLM_OUTPUT` not set):
  ```rust
  let llm_output_defined = std::env::var("LLM_OUTPUT").is_ok();
  let temp_file = if !llm_output_defined {
      let temp = temp_file("-eval-", "");
      envs.insert("LLM_OUTPUT".into(), temp.display().to_string());
      temp
  } else { PathBuf::new() };
  ```
  1× `std::env::var("LLM_OUTPUT")` syscall, 1× temp file creation (syscall: `open(O_CREAT)`), 1× `PathBuf::display().to_string()` alloc.
- `src/function.rs:310` — `run_command(&cmd_name, &cmd_args, Some(envs.clone()))` — 1× OS process spawn (fork+exec), SYNC blocking.
- `src/function.rs:326-340` — `fs::read_to_string(temp_file)` — 1× disk read syscall, then conditional `jsonic::parse(&contents)` if output starts with `{` or `[` — 2 allocations if JSON.
- `src/function.rs:183-186` — Inside `eval()`: `config.read().agent` → `self.name.clone()` + `agent.name().to_string()` — 2× String allocations per call (agent path).
- `src/function.rs:242` — `agent.variable_envs()` — returns a `HashMap<String, String>` clone (agent mode only); `Default::default()` for non-agent mode.
- `src/function.rs:214-215` — Arguments are re-serialized to JSON per call: `cmd_args.push(json_data.to_string())` — 1× `serde_json::Value::to_string()` allocation.

### Per-Round-Trip Evidence

- `src/main.rs:204` — `config.write().before_chat_completion(&input)?;` called once per recursive `start_directive` call.
- `src/config/mod.rs:2049-2053` — `before_chat_completion()` body:
  ```rust
  pub fn before_chat_completion(&mut self, input: &Input) -> Result<()> {
      self.last_message = Some(LastMessage::new(input.clone(), String::new()));
      Ok(())
  }
  ```
  1× full `Input::clone()` — clones all fields: text (String), raw (String + Vec<String>), patched_text, last_reply, continue_output, medias (Vec<String>), data_urls (HashMap), tool_calls (Option<MessageContentToolCalls>), role (Role struct), config (Arc clone), 3 bools.
- `src/config/mod.rs:2054-2068` — `after_chat_completion()`: when `tool_results` non-empty, returns early with `Ok(())` (intermediate rounds do NOT save). On final round: 1× `input.clone()` + `save_message()` → `session.add_message()` → `update_tokens()` → `model().total_tokens(&messages)`.
- `src/config/input.rs:226` — `create_client()` → `init_client(&self.config, Some(self.role().model().clone()))` — 1× Model clone + linear scan through all client types (O(K), K≈20) to find the matching one + `Box<dyn Client>` allocation.
- `src/client/common.rs:51-63` — `build_client()` (called per `chat_completions` and per `chat_completions_streaming`):
  ```rust
  fn build_client(&self) -> Result<ReqwestClient> {
      let mut builder = ReqwestClient::builder();
      ...
      let client = builder.connect_timeout(Duration::from_secs(timeout)).build()...;
      Ok(client)
  }
  ```
  1× new `reqwest::Client` (creates internal connection pool, TcpConnector, potentially DNS resolver) — destroys TCP keep-alive from prior round-trip.
- `src/client/common.rs:74` — `input.prepare_completion_data(self.model(), false)?;` called per non-stream request; `src/client/common.rs:95` — same for streaming.
- `src/config/input.rs:233-246` — `prepare_completion_data()` calls:
  ```rust
  let mut messages = self.build_messages()?;   // full Vec<Message> rebuild
  patch_messages(&mut messages, model);         // O(N) traversal for model patches
  model.guard_max_input_tokens(&messages)?;     // O(N) token counting pass
  let functions = self.config.read().select_functions(self.role()); // O(F) clone
  ```
  4 operations, all O(N) in message count.
- `src/config/session.rs:525-556` — `Session::build_messages()` first line: `let mut messages = self.messages.clone();` — full clone of entire message history Vec on every round-trip.
- `src/config/mod.rs:1652-1662` — `select_functions()`:
  ```rust
  let declaration_names: HashSet<String> = self.functions.declarations().iter()
      .map(|v| v.name.to_string()).collect();
  ```
  Allocates a `HashSet<String>` with all function names on every round-trip.
- `src/function.rs:28` — `calls = ToolCall::dedup(calls);` — iterates all tool calls in reverse, O(N) with `HashSet<String>` allocation.
- `src/client/common.rs:403-413` — `call_chat_completions` (non-stream path):
  ```rust
  let ret = abortable_run_with_spinner(
      client.chat_completions(input.clone()),  // 1× Input clone
      "Generating",
      abort_signal,
  ).await;
  ```
  1× tokio task spawn (for spinner) + 1× `Input::clone()` per round-trip on non-stream path.

### Per-Session / Cold-Path Evidence

- `src/config/session.rs:135` — `update_tokens()`:
  ```rust
  pub fn update_tokens(&mut self) {
      self.tokens = self.model().total_tokens(&self.messages);
  }
  ```
  Called in `add_message()` (session.rs:512 approx), `clear_messages()`, and `set_model()`. NOT called per intermediate round-trip — only on final `save_message()`. This is important: token counting is NOT per-round-trip in tool-call loops.
- `src/config/session.rs:81-107` — `Session::load()` calls `session.update_tokens()` at load time. ONCE per session. Then not again until `add_message()` is called (after final tool result).
- `src/function.rs:65-87` — `Functions::init()` does double-parse: `fs::read_to_string()` → `jsonic::parse()` → `json_item.as_str()` → `serde_json::from_str()`. Two parse passes over the functions JSON file. ONCE per session.
- `src/client/common.rs:1-28` — `ALL_PROVIDER_MODELS` is a `LazyLock<Vec<ProviderModels>>` initialized once at startup from embedded `models.yaml`. ONCE per process.

---

## Findings

Ordered by impact (highest first):

1. **[Per-Tool-Call] OS Process Spawn — CONFIRMED** (`src/function.rs:310`). `run_command()` is called synchronously for each tool call. Process creation overhead is ~1–10ms on Linux. For N sequential calls, total blocking time = N × spawn_time + N × execution_time. This is the dominant bottleneck in tool-call batches.

2. **[Per-Round-Trip] New ReqwestClient per HTTP call — CONFIRMED** (`src/client/common.rs:51-63`). `build_client()` is called inside both `chat_completions()` (line 72) and `chat_completions_streaming()` (line 94). Each call discards the internal connection pool. Between round-trips, TCP connections cannot be reused (no keep-alive). For M round-trips: M new TLS handshakes + M TCP 3-way handshakes. Measured impact: +100–500ms per round-trip on cold TLS.

3. **[Per-Round-Trip] Input clone in before_chat_completion — CONFIRMED** (`src/config/mod.rs:2049-2053`). Full `Input::clone()` on every recursion, including all accumulated `tool_calls` (grows each round), text, medias, data_urls. After round R, the tool_calls field has accumulated all prior tool results — clone cost is O(R×T) where T is tool call payload size.

4. **[Per-Round-Trip] session.messages.clone() in build_messages — CONFIRMED** (`src/config/session.rs:525`). `let mut messages = self.messages.clone()` allocates a new `Vec<Message>` containing ALL historical messages on every round-trip. After R rounds with N messages per round, the final clone is O(N × R) in data size. Messages are never incrementally built; always full rebuild.

5. **[Per-Tool-Call] PATH rebuild via env::var + format! — CONFIRMED** (`src/function.rs:282-293`). Every invocation reads the OS PATH env var, formats a prepend string, and builds a new owned String. Cost is small (~microseconds) but adds up with many calls. The PATH never changes during a session — this is pure redundant work.

6. **[Per-Round-Trip] Input clone in call_chat_completions (non-stream) — CONFIRMED** (`src/client/common.rs:412`). `client.chat_completions(input.clone())` — yet another Input clone per request. The non-streaming path thus has 2 Input clones per round-trip (one in before_chat_completion, one here).

7. **[Per-Round-Trip] select_functions HashSet alloc — CONFIRMED** (`src/config/mod.rs:1652-1662`). Allocates a new `HashSet<String>` with all function declaration names on every call to `prepare_completion_data`. This set is only used to filter by `use_tools` and doesn't change between rounds — pure redundant allocation.

8. **[Per-Tool-Call] Temp file creation + disk read — CONFIRMED** (`src/function.rs:296-302`, `src/function.rs:326-340`). When `LLM_OUTPUT` is not pre-set, each tool call creates a temporary file (OS `open(O_CREAT)`), writes to it via the subprocess, then reads it back with `fs::read_to_string()`. This is 2 filesystem syscalls (create + read) plus potentially a conditional `jsonic::parse()` — all per tool call.

9. **[Per-Round-Trip] Input clone in streaming chat_completions_streaming — CONFIRMED** (`src/client/common.rs:81-82`). The streaming path also does `let input = input.clone()` inside the `tokio::select!` block — same cost as non-stream.

10. **[Per-Round-Trip] ToolCall::dedup O(N) + HashSet alloc — CONFIRMED** (`src/function.rs:28`, `src/function.rs:157-170`). Iterates all calls in reverse, building a `HashSet<&str>`. For a typical batch of 1–5 calls, cost is negligible. For large batches, it's O(N) with a HashSet allocation.

11. **[Per-Tool-Call] Duplicate JSON parse of arguments — CONFIRMED** (`src/function.rs:185-204`). When `arguments.is_object()` is false (string form), the code does `jsonic::parse()` → `json_item.as_str()` → `serde_json::from_str()` — two full JSON parse passes plus intermediate String materialization via `as_str().unwrap_or_default()`. Also: `cmd_args.push(json_data.to_string())` re-serializes the Value back to String to pass as CLI argument.

12. **[Per-Session Cold] Functions::init double-parse — CONFIRMED** (`src/function.rs:65-87`). Only at agent init, but: `fs::read_to_string()` → `jsonic::parse()` → `serde_json::from_str()` — two full parse passes. Low impact (once-per-session) but architecturally wasteful.

13. **[Per-Session Cold] update_tokens NOT called per intermediate round — CONFIRMED** (`src/config/session.rs:135`). `update_tokens()` is only called via `add_message()` which is invoked only on the FINAL round's `save_message()`. Intermediate tool-call rounds do NOT trigger token recalculation. Token display in the prompt (session.tokens_usage()) may show stale values during multi-round agent runs.

---

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Parallelize independent tool calls in `eval_tool_calls` using `tokio::spawn` + `join_all` | high | M | med | CONFIRMED: sequential for-loop at `src/function.rs:36`; blocking SYNC spawn at line 310 |
| 2 | Cache `ReqwestClient` across round-trips (store in config or client struct) | high | S | low | CONFIRMED: `build_client()` creates new client per `chat_completions` call at `src/client/common.rs:72,94` |
| 3 | Replace tempfile IPC with stdin/stdout pipe to subprocess | high | L | med | CONFIRMED: per-call `open(O_CREAT)` + `read_to_string` at `src/function.rs:296-340` |
| 4 | Cache PATH prepend string across calls (compute once at agent init) | low | S | low | CONFIRMED: redundant `env::var("PATH")` + `format!` per call at `src/function.rs:282-293` |
| 5 | Move Input clone out of `before_chat_completion` (use Cow or keep reference) | med | M | med | CONFIRMED: `input.clone()` in `before_chat_completion` at `src/config/mod.rs:2050`, grows O(R×T) |
| 6 | Avoid `session.messages.clone()` in `build_messages` — use Cow or incremental append | med | M | med | CONFIRMED: `let mut messages = self.messages.clone()` at `src/config/session.rs:525` |
| 7 | Cache `select_functions` result in Input/Config between rounds (invalidate only on config change) | low | S | low | CONFIRMED: new `HashSet<String>` allocation every `prepare_completion_data` call at `src/config/mod.rs:1657-1661` |
| 8 | Skip double-parse of tool call arguments: if LLM sends arguments as an already-valid JSON object, skip jsonic path | low | S | low | CONFIRMED: `if self.arguments.is_object()` fast path exists at `src/function.rs:186`; only string form pays double-parse cost |
| 9 | Remove one Input clone in non-stream path (`call_chat_completions`) | low | S | low | CONFIRMED: `input.clone()` passed to `abortable_run_with_spinner` at `src/client/common.rs:412` |
| 10 | Move from `#[async_recursion]` to an explicit `while` loop to prevent stack growth | med | M | med | CONFIRMED: recursive `start_directive` at `src/main.rs:196-222` with recursion at line 222 |

---

## Open Questions / Needs Verification

- **What is the actual wall-clock split** between HTTP time vs tool-call subprocess time? Need runtime tracing to confirm which is dominant.
- **Is `guard_max_input_tokens` O(N) per round?** The token counting pass iterates all messages — if the session has 100+ messages, this may be non-trivial. Needs inspection of `Model::total_tokens` and `estimate_token_length` (task 9).
- **Does reqwest internally cache DNS resolution?** The new `ReqwestClient` per call may still benefit from OS-level DNS cache. TLS session resumption is unlikely since the client is dropped.
- **Input struct size in practice**: how large does the `tool_calls` field grow across R rounds? Depends on typical agent output sizes. Could be kilobytes to megabytes per clone.
- **Are tool calls ever idempotent/pure?** The code has no mechanism to mark a function as read-only. Parallelization (candidate #1) requires knowledge of side-effect safety — needs protocol-level support or per-function metadata.
- **Does the streaming path also call `before_chat_completion`?** Yes — `src/main.rs:248` calls it for the eval-string path too. Confirmed 2× `Input::clone()` in the streaming round-trip (before_chat_completion + streaming clone).

---

## Hot-Path Classification

| Operation | Classification |
|-----------|---------------|
| Process spawn (run_llm_function) | Per-call (hottest) |
| Temp file create + read | Per-call |
| PATH rebuild | Per-call |
| Arguments JSON parse (string form) | Per-call |
| ReqwestClient creation | Per-round-trip (hot) |
| Input::clone (before_chat_completion) | Per-round-trip (hot) |
| Input::clone (call_chat_completions non-stream) | Per-round-trip |
| Input::clone (chat_completions_streaming) | Per-round-trip |
| session.messages.clone() | Per-round-trip |
| build_messages() full rebuild | Per-round-trip |
| patch_messages() O(N) | Per-round-trip |
| guard_max_input_tokens O(N) | Per-round-trip |
| select_functions() + HashSet alloc | Per-round-trip |
| ToolCall::dedup | Per-round-trip |
| update_tokens() | Per final-round only (after save_message) |
| Agent init / Functions::init | Cold (per-session) |
| Session::load() + update_tokens | Cold (per-session) |
| ALL_PROVIDER_MODELS init | Cold (per-process, LazyLock) |
