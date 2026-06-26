# Macro Overview: Full Agent Tool-Call Chain

**Task ID**: 1  **Scope**: Map the complete agent tool-call chain from user input to tool-result merge, including all sync/async boundaries, loop-back points, and per-hop file:line anchors.

---

## Evidence Gathered

### Entry Point Chain

- `src/main.rs:183-193` — `run()` calls `Config::use_agent()` for agent mode, or falls to `start_directive()` for direct invocation:
  ```rust
  let ret = Config::use_agent(&config, agent, session, abort_signal.clone()).await;
  ```

- `src/config/mod.rs:1488` — `Config::use_agent()` calls `Agent::init()` then delegates to the REPL loop or a `start_directive()` after agent is installed in config:
  ```rust
  let agent = Agent::init(config, agent_name, abort_signal).await?;
  ```

- `src/main.rs:195-233` — `start_directive()` is the actual multi-turn loop, implemented via **async recursion** using `#[async_recursion::async_recursion]`:
  ```rust
  #[async_recursion::async_recursion]
  async fn start_directive(config: &GlobalConfig, input: Input, code_mode: bool, abort_signal: AbortSignal) -> Result<()> {
      let client = input.create_client()?;                   // ← allocates Box<dyn Client> each call
      let extract_code = !*IS_STDOUT_TERMINAL && code_mode;
      config.write().before_chat_completion(&input)?;        // ← writes config lock, clones input
      let (output, tool_results) = if !input.stream() || extract_code {
          call_chat_completions(&input, true, extract_code, client.as_ref(), abort_signal.clone()).await?
      } else {
          call_chat_completions_streaming(&input, client.as_ref(), abort_signal.clone()).await?
      };
      config.write().after_chat_completion(&input, &output, &tool_results)?;   // ← clones, writes to file/session
      if !tool_results.is_empty() {
          start_directive(config,
              input.merge_tool_results(output, tool_results),   // ← builds new Input with merged results
              code_mode, abort_signal).await?;                  // ← RECURSIVE CALL = loop-back point
      }
      config.write().exit_session()?;
  }
  ```

### HTTP + Streaming Path

- `src/client/common.rs:406-440` — `call_chat_completions()` (non-streaming path):
  ```rust
  let ret = abortable_run_with_spinner(
      client.chat_completions(input.clone()),  // ← clones entire Input
      "Generating",
      abort_signal,
  ).await;
  // ... on success:
  Ok((text, eval_tool_calls(client.global_config(), tool_calls)?))  // ← SYNC call inside async
  ```

- `src/client/common.rs:441-471` — `call_chat_completions_streaming()` (SSE path):
  ```rust
  let (send_ret, render_ret) = tokio::join!(
      client.chat_completions_streaming(input, &mut handler),
      render_stream(rx, client.global_config(), abort_signal.clone()),
  );
  // ... on success:
  Ok((text, eval_tool_calls(client.global_config(), tool_calls)?))  // ← SYNC call inside async
  ```

- `src/client/common.rs:68-79` — `chat_completions()` default impl:
  ```rust
  let client = self.build_client()?;               // ← builds NEW ReqwestClient on every call
  let data = input.prepare_completion_data(self.model(), false)?;
  self.chat_completions_inner(&client, data).await
  ```

### Tool Execution Path

- `src/function.rs:22-47` — `eval_tool_calls()` — **SYNCHRONOUS** executor:
  ```rust
  pub fn eval_tool_calls(config: &GlobalConfig, mut calls: Vec<ToolCall>) -> Result<Vec<ToolResult>> {
      calls = ToolCall::dedup(calls);
      for call in calls {                              // ← sequential loop, no parallelism
          let mut result = call.eval(config)?;         // ← each call is sync-blocking
          ...
          output.push(ToolResult::new(call, result));
      }
      Ok(output)
  }
  ```

- `src/function.rs:271+` — `run_llm_function()` — per-call subprocess spawn (SYNC):
  ```rust
  let current_path = std::env::var("PATH").context("No PATH environment variable")?;
  let prepend_path = bin_dirs.iter().map(|v| format!("{}{PATH_SEP}", v.display())).collect::<Vec<_>>().join("");
  envs.insert("PATH".into(), format!("{prepend_path}{current_path}"));  // ← PATH rebuilt each call
  let temp = temp_file("-eval-", "");                                      // ← temp file per call (when LLM_OUTPUT not set)
  let exit_code = run_command(&cmd_name, &cmd_args, Some(envs.clone()))?; // ← subprocess spawn (blocks)
  let contents = fs::read_to_string(temp_file)?;                          // ← disk read back
  ```

### Message Building / Merge Path

- `src/config/input.rs:212-222` — `Input::merge_tool_results()`:
  ```rust
  pub fn merge_tool_results(mut self, output: String, tool_results: Vec<ToolResult>) -> Self {
      match self.tool_calls.as_mut() {
          Some(exist_tool_results) => { exist_tool_results.merge(tool_results, output); }
          None => self.tool_calls = Some(MessageContentToolCalls::new(tool_results, output)),
      }
      self   // ← returns modified Input (moves, no clone here)
  }
  ```

- `src/config/input.rs:252-264` — `Input::build_messages()` — rebuilt from scratch each round-trip:
  ```rust
  pub fn build_messages(&self) -> Result<Vec<Message>> {
      let mut messages = if let Some(session) = self.session(&self.config.read().session) {
          session.build_messages(self)      // ← full Vec<Message> allocation
      } else {
          self.role().build_messages(self)
      };
      if let Some(tool_calls) = &self.tool_calls {
          messages.push(Message::new(MessageRole::Assistant, MessageContent::ToolCalls(tool_calls.clone())))
      }
      Ok(messages)
  }
  ```

- `src/config/mod.rs:2049-2073` — `before_chat_completion()` and `after_chat_completion()`:
  ```rust
  pub fn before_chat_completion(&mut self, input: &Input) -> Result<()> {
      self.last_message = Some(LastMessage::new(input.clone(), String::new()));  // ← clones entire Input
      Ok(())
  }
  pub fn after_chat_completion(&mut self, input: &Input, output: &str, tool_results: &[ToolResult]) -> Result<()> {
      if !tool_results.is_empty() { return Ok(()); }    // ← short-circuits for tool-result rounds
      self.last_message = Some(LastMessage::new(input.clone(), output.to_string()));  // ← clones Input
      if !self.dry_run { self.save_message(input, output)?; }   // ← may write to disk/session
      Ok(())
  }
  ```

### Client Allocation

- `src/client/macros.rs:75-83` — `init_client()` (macro-generated): iterates through all registered client configs to find matching type, clones config, allocates new `Box<dyn Client>`:
  ```rust
  pub fn init_client(config: &GlobalConfig, model: Option<Model>) -> Result<Box<dyn Client>> {
      let model = model.unwrap_or_else(|| config.read().model.clone());
      None $(.or_else(|| $client::init(config, &model)))+   // ← linear scan through all client types
      .ok_or_else(|| ...)
  }
  ```

- `src/config/input.rs:226-228` — `Input::create_client()` calls `init_client()` per round-trip:
  ```rust
  pub fn create_client(&self) -> Result<Box<dyn Client>> {
      init_client(&self.config, Some(self.role().model().clone()))
  }
  ```

### Compress Integration

- `src/config/mod.rs:1224` — `Config::maybe_compress_session()` is called from the REPL path. In `start_directive` path it appears NOT called automatically, meaning session compression in non-REPL use would need verification.

---

## Findings

1. **The loop-back mechanism is async recursion, not an explicit `while` loop** (CONFIRMED — `src/main.rs:195`). Each recursion level represents one LLM round-trip. Stack depth equals number of tool-call rounds.

2. **`eval_tool_calls` is fully synchronous and blocks the tokio async executor thread** (CONFIRMED — `src/function.rs:22`, called from async context at `src/client/common.rs:435,466`). With N tool calls taking K ms each: total latency = N × K (sequential). This is the primary hot-path bottleneck.

3. **A new `Box<dyn Client>` is allocated per round-trip** (CONFIRMED — `src/config/input.rs:226`, `src/client/macros.rs:75`). This includes a config scan and model clone on each recursion.

4. **A new `ReqwestClient` is built per HTTP call** (CONFIRMED — `src/client/common.rs:74`): `let client = self.build_client()?;`. This means no HTTP connection reuse across round-trips: no persistent TCP connection pool for multi-turn agent flows.

5. **`before_chat_completion()` clones the entire `Input` struct** every round-trip (CONFIRMED — `src/config/mod.rs:2050`). After 10 tool-call rounds with a large message history, each `Input` clone includes all accumulated text and tool results.

6. **`input.build_messages()` is called inside `prepare_completion_data()`** on every round-trip, rebuilding `Vec<Message>` from scratch (CONFIRMED — `src/config/input.rs:241,252`). The messages include the full accumulated history.

7. **`after_chat_completion()` short-circuits (skips saving) when tool_results is non-empty** (CONFIRMED — `src/config/mod.rs:2057`). This is a deliberate optimization: intermediate tool-call rounds don't write to disk. Only the final round saves.

8. **The agent loop has no explicit concurrency guard**: since `start_directive` is async-recursive, each outer call holds a tokio stack frame waiting for the recursive call to complete. This is fine for correctness but prevents any inter-round parallelism.

9. **PATH environment variable is reconstructed on every `run_llm_function` call** (CONFIRMED — `src/function.rs:285-293`): `format!("{prepend_path}{current_path}")`. For agents with N calls × M rounds, this is N×M PATH rebuilds.

10. **The `ReqwestClient::builder()` is called inside every `chat_completions()` call** (CONFIRMED — `src/client/common.rs:74`). Reqwest clients are designed to be shared/cloned, not rebuilt. Each rebuild creates a new connection pool that cannot reuse the previous TCP connection.

---

## Call Graph Diagram

```
main()                                                    [src/main.rs:34]
  └─ run()                                               [src/main.rs:56]
       ├─ Config::use_agent() [if --agent]               [src/config/mod.rs:1488]
       │    └─ Agent::init()                             [src/config/agent.rs:33]  ← cold path
       └─ start_directive() [entry to round-trip loop]   [src/main.rs:195]  ASYNC
            │
            ├─ input.create_client()                     [src/config/input.rs:226]
            │   └─ init_client() [linear scan+alloc]     [src/client/macros.rs:75]
            │
            ├─ config.write().before_chat_completion()   [src/config/mod.rs:2049]
            │   └─ input.clone()                         ← allocates Input copy
            │
            ├─ call_chat_completions()  [non-stream]     [src/client/common.rs:406]  ASYNC
            │   ├─ abortable_run_with_spinner()
            │   │   └─ client.chat_completions()
            │   │       ├─ self.build_client()           [src/client/common.rs:74]  ← NEW ReqwestClient
            │   │       ├─ input.prepare_completion_data() [src/config/input.rs:236]
            │   │       │   └─ build_messages()          [src/config/input.rs:252]  ← Vec<Message> rebuild
            │   │       └─ chat_completions_inner()      ← HTTP POST (network I/O)
            │   └─ eval_tool_calls()  [SYNC]             [src/function.rs:22]
            │       └─ for call in calls:                ← SEQUENTIAL loop
            │           └─ call.eval()
            │               └─ run_llm_function()        [src/function.rs:271]  SYNC BLOCKS
            │                   ├─ PATH env rebuild
            │                   ├─ temp_file creation
            │                   ├─ run_command() [subprocess spawn] ← process boundary
            │                   └─ fs::read_to_string() [disk read]
            │
            OR (streaming):
            ├─ call_chat_completions_streaming()         [src/client/common.rs:441]  ASYNC
            │   ├─ tokio::join!(send_stream, render_stream)
            │   │   └─ chat_completions_streaming()
            │   │       ├─ self.build_client()           [src/client/common.rs:94]  ← NEW ReqwestClient
            │   │       ├─ prepare_completion_data()     ← Vec<Message> rebuild
            │   │       └─ SSE stream loop               ← network I/O (incremental)
            │   └─ eval_tool_calls()  [SYNC]             [src/function.rs:22]
            │
            ├─ config.write().after_chat_completion()    [src/config/mod.rs:2054]
            │   └─ if tool_results.is_empty(): save_message()   ← maybe disk write
            │
            └─ if tool_results not empty:
                └─ input.merge_tool_results()            [src/config/input.rs:212]
                    └─ start_directive() [RECURSIVE]     [src/main.rs:195]  ← LOOP-BACK
```

**Sync/Async Boundaries**:
- `start_directive`: ASYNC (tokio)
- HTTP calls (`chat_completions`, `chat_completions_streaming`): ASYNC
- SSE stream processing: ASYNC (tokio::join of HTTP + render)
- `eval_tool_calls()`: **SYNC** — blocks async executor
- `run_llm_function()`: **SYNC** — blocks on subprocess and disk I/O
- Session save: **SYNC** (file I/O, called from SYNC context within write lock)
- Session compression: ASYNC, spawned to `tokio::spawn` (non-blocking for caller)

**Process Boundaries**:
- `run_command()` inside `run_llm_function()`: spawns external process per tool call

---

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Run `eval_tool_calls` in background tokio task (`spawn_blocking`) to avoid blocking executor | high | S | low | CONFIRMED: `src/function.rs:22`, `src/client/common.rs:435` |
| 2 | Parallelize independent tool calls with `tokio::spawn` or `futures::join_all` | high | M | med | CONFIRMED: sequential `for call in calls` at `src/function.rs:31` |
| 3 | Cache `Box<dyn Client>` across round-trips (store in Config or pass through recursion) | med | M | low | CONFIRMED: `input.create_client()` at `src/config/input.rs:226` |
| 4 | Cache `ReqwestClient` across calls (store in client struct or Config) | high | M | low | CONFIRMED: `build_client()` per call at `src/client/common.rs:74` |
| 5 | Convert async recursion to explicit `loop` to avoid stack growth and enable context reuse | med | L | med | CONFIRMED: `src/main.rs:195` async recursion |
| 6 | Avoid `Input.clone()` in `before_chat_completion` — store reference or slim metadata | low | M | med | CONFIRMED: `src/config/mod.rs:2050` |
| 7 | Cache PATH string across tool calls in `run_llm_function` (same session = same PATH) | low | S | low | CONFIRMED: `src/function.rs:285-293` |

---

## Open Questions / Needs Verification

- Does `maybe_compress_session()` fire in non-REPL `start_directive` flows? (`src/config/mod.rs:1224` is only called from REPL code path — needs trace)
- What is the actual per-client-init cost of the config linear scan? (depends on number of registered clients — likely ~20 types in `register_client!`)
- Is `build_messages()` O(n) in message count or O(n²)? (depends on `session.build_messages` implementation — needs task 7)
- With async recursion: does the Rust compiler stack-allocate large futures across rounds, potentially causing stack overflow with many tool-call rounds?

---

## Hot-Path Classification

| Operation | Classification |
|-----------|---------------|
| `eval_tool_calls` + subprocess spawn | **Per-call** (hottest) |
| `build_client()` / `build_ReqwestClient()` | **Per-round-trip** |
| `build_messages()` | **Per-round-trip** |
| `before_chat_completion` Input clone | **Per-round-trip** |
| PATH env rebuild | **Per-call** |
| Temp file create + disk read | **Per-call** |
| `merge_tool_results` | **Per-round-trip** |
| HTTP POST + network I/O | **Per-round-trip** (dominates wall time) |
| Session compress | **Cold-path** (conditional, async) |
| `Agent::init()` | **Per-session** (cold path) |
