# Message History Growth Per Tool-Call Round
**Task ID**: 7  **Scope**: Analyze how session messages and tool-call accumulations grow with each round-trip; identify clone costs, allocation patterns, and per-round overhead.

## Evidence Gathered

### Data Structures
- `src/function.rs:48-51` — `ToolResult` definition:
  ```rust
  pub struct ToolResult {
      pub call: ToolCall,  // { name: String, arguments: Value, id: Option<String> }
      pub output: Value,   // serde_json::Value — unbounded size (file contents, web search, etc.)
  }
  ```
  No `Arc` wrapping; fully owned clone. `Value` can be hundreds of KB for tool outputs.

- `src/client/message.rs:185-197` — `MessageContentToolCalls` and its `merge()`:
  ```rust
  pub struct MessageContentToolCalls {
      pub tool_results: Vec<ToolResult>,
      pub text: String,
      pub sequence: bool,
  }
  impl MessageContentToolCalls {
      pub fn merge(&mut self, tool_results: Vec<ToolResult>, _text: String) {
          self.tool_results.extend(tool_results);  // append in-place, no clone
          self.text.clear();
          self.sequence = true;
      }
  }
  ```
  `merge()` **appends** new results to the existing Vec in-place — O(new_N) work. The Vec grows across rounds: after R rounds with N tool calls each, `tool_results.len() == R × N`.

- `src/config/input.rs:18-33` — `Input` struct derives `Clone`:
  ```rust
  #[derive(Debug, Clone)]
  pub struct Input {
      // ...
      tool_calls: Option<MessageContentToolCalls>,  // THIS GROWS PER ROUND
      // ...
  }
  ```

### Key Operations

- `src/config/input.rs:212-221` — `merge_tool_results` (called per round):
  ```rust
  pub fn merge_tool_results(mut self, output: String, tool_results: Vec<ToolResult>) -> Self {
      match self.tool_calls.as_mut() {
          Some(exist_tool_results) => {
              exist_tool_results.merge(tool_results, output);  // extend Vec in-place
          }
          None => self.tool_calls = Some(MessageContentToolCalls::new(tool_results, output)),
      }
      self
  }
  ```
  Takes `mut self` (moves ownership — **no clone here**). Returns updated self.

- `src/config/input.rs:252-263` — `build_messages` (called per HTTP request):
  ```rust
  pub fn build_messages(&self) -> Result<Vec<Message>> {
      let mut messages = if let Some(session) = ... {
          session.build_messages(self)   // <-- clones session.messages
      } else {
          self.role().build_messages(self)
      };
      if let Some(tool_calls) = &self.tool_calls {
          messages.push(Message::new(
              MessageRole::Assistant,
              MessageContent::ToolCalls(tool_calls.clone()),  // <-- DEEP CLONE of entire accumulated Vec<ToolResult>
          ))
      }
      Ok(messages)
  }
  ```
  `tool_calls.clone()` at line ~260 deep-clones the **entire accumulated** `MessageContentToolCalls` including all `ToolResult` entries from all prior rounds.

- `src/config/session.rs:525-555` — `Session::build_messages` (called per HTTP request):
  ```rust
  pub fn build_messages(&self, input: &Input) -> Vec<Message> {
      let mut messages = self.messages.clone();  // <-- FULL CLONE of session message history
      // ...
      if need_add_msg {
          messages.push(Message::new(MessageRole::User, input.message_content()));
      }
      messages
  }
  ```
  `self.messages.clone()` clones ALL persisted session messages every round. During a single tool-call chain, `session.messages` does NOT change (it's only updated at chain end via `add_message`).

- `src/config/mod.rs:2043-2050` — `before_chat_completion` (called per round):
  ```rust
  pub fn before_chat_completion(&mut self, input: &Input) -> Result<()> {
      self.last_message = Some(LastMessage::new(input.clone(), String::new()));
      Ok(())
  }
  ```
  `input.clone()` at mod.rs:2050 deep-clones the entire `Input` including the growing `tool_calls`. Clone cost grows each round.

- `src/config/session.rs:469-500` — `Session::add_message` (called ONCE at chain end):
  ```rust
  // Only called via save_message at mod.rs:2080
  if let Some(tool_calls) = input.tool_calls() {
      self.messages.push(Message::new(
          MessageRole::Tool,
          MessageContent::ToolCalls(tool_calls.clone()),  // final clone at chain end
      ))
  }
  ```
  Adds 3 messages to `session.messages` after the entire tool-call chain: user + ToolCalls (ALL rounds' results) + assistant text. This is the only time `session.messages` grows during a tool-call session.

- `src/config/mod.rs:2076-2080` — `save_message`:
  ```rust
  fn save_message(&mut self, input: &Input, output: &str) -> Result<()> {
      let mut input = input.clone();  // ANOTHER Input clone at chain end
      input.clear_patch();
      if let Some(session) = input.session_mut(&mut self.session) {
          session.add_message(&input, output)?;
  ```

## Findings

### Finding 1: `tool_calls.clone()` in `build_messages` is the quadratic factor (CONFIRMED)
**Location**: `src/config/input.rs:260`

The `MessageContentToolCalls` accumulates ALL tool results from ALL rounds via `merge()`. On round R with N calls/round, `tool_calls.len() == R×N`. Each call to `build_messages()` (once per HTTP request) clones the entire Vec. Clone cost grows linearly with round number:

| Round | tool_results entries | Clone work (@ 500B/ToolResult) |
|-------|---------------------|--------------------------------|
| 1 | N | N × 500B |
| 2 | 2N | 2N × 500B |
| R | R×N | R×N × 500B |

**Total clone work for `tool_calls` across a complete R-round chain**: Σᵢ₌₁ᴿ (i×N×size) = N×size × R(R+1)/2 = **O(R² × N × size)**.

For a heavy agent (10 rounds × 3 tool calls × 2KB/result): total `tool_calls` clone work ≈ 10×11/2 × 3 × 2KB = 330KB of clone operations. This is per-request, repeated O(R) times.

### Finding 2: `session.messages.clone()` is O(M₀) per round, constant during a chain (CONFIRMED)
**Location**: `src/config/session.rs:525`

During a tool-call chain, `session.messages` does NOT grow — it stays at the "before chain started" count M₀. The clone is O(M₀) each round. Between chains, M₀ grows by 3 per completed chain (user + tool_calls + assistant). In long sessions, M₀ can be large (100+ messages). However, this is linear-per-round, not quadratic.

For a session with 20 prior chains (60 messages), each round clone = 60 × ~200B = 12KB. Over 10 rounds = 120KB. Significant but not the dominant factor.

### Finding 3: Input::clone() × 2-3 per round includes growing tool_calls (CONFIRMED)
**Locations**: `src/config/mod.rs:2050` (before_chat_completion), `src/client/common.rs:412` (non-stream path), `src/client/common.rs:82` (stream path internal clone).

Each `Input::clone()` clones `tool_calls: Option<MessageContentToolCalls>`. The clone cost grows with each round (same as Finding 1). With 2-3 clones per round, the total Input cloning overhead is 2-3× the `tool_calls` clone cost — same O(R²×N×size) complexity.

### Finding 4: The "dual accumulation" design — all rounds in ONE ToolCalls message (CONFIRMED)
**Location**: `src/client/message.rs:200-203` (`merge`) and `src/config/session.rs:494-498` (`add_message`)

All tool calls from ALL rounds are accumulated in a SINGLE `MessageContentToolCalls` object (via `extend()`). At chain end, this one object is saved as a single `MessageRole::Tool` message. This design means:
- During chain: `tool_calls.Vec` grows cross-round, making each clone heavier
- At chain end: ONE large message is saved vs. multiple smaller ones per round
- Trade-off: simplicity at cost of O(R²) clone overhead during chain

### Finding 5: `merge_tool_results` is efficient — no clone (CONFIRMED)
**Location**: `src/config/input.rs:212-221`

Takes `mut self` (ownership), modifies `tool_calls` in-place via `extend()`, returns `self`. The tool results `Vec<ToolResult>` passed in is moved (not cloned). This is the ONE efficient step in the chain.

### Finding 6: No `Arc` or `Cow` anywhere in the message/result types (CONFIRMED)
**Scan of**: `src/client/message.rs`, `src/function.rs:48-57`, `src/config/input.rs:18-33`

All types (`ToolResult`, `ToolCall`, `MessageContentToolCalls`, `Message`, `MessageContent`) are fully owned. Every `clone()` performs a deep copy. No reference-counted sharing. No `Cow<str>` for strings. This is the root cause of the O(R²) overhead.

### Finding 7: `save_message` clones Input one extra time at chain end (CONFIRMED)
**Location**: `src/config/mod.rs:2076-2078`

```rust
fn save_message(&mut self, input: &Input, output: &str) -> Result<()> {
    let mut input = input.clone();  // clones at chain end
    input.clear_patch();
```

At chain end, the Input (with full accumulated `tool_calls`) is cloned one more time. This is a cold-path cost (once per chain, not per round), but for a 10-round × 5-tool chain, this is a 50-ToolResult clone at the end.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Wrap `ToolResult` in `Arc<ToolResult>` — clone becomes O(1) pointer copy instead of O(data_size) | high | M | med | CONFIRMED — tool_calls.clone() is O(R²×N×size) |
| 2 | Change `Session::build_messages` to return a struct borrowing `&session.messages` + new messages, avoiding `messages.clone()` per round | high | L | high | CONFIRMED — full Vec clone per round |
| 3 | Change `chat_completions(non-stream)` to take `&Input` instead of `Input` by value — eliminates one `Input::clone()` per non-stream round | med | M | med | CONFIRMED — common.rs:412, task 2 |
| 4 | Wrap `MessageContentToolCalls` in `Arc<RwLock<...>>` or `Arc<MessageContentToolCalls>` — all clones become O(1) | high | M | med | CONFIRMED — cloned 2-3× per round |
| 5 | Change `build_messages` to return `impl Iterator<Item=&Message>` plus a tail of new messages — lazy, zero copy for historical messages | high | L | high | CONFIRMED — session.messages.clone() per round |
| 6 | In `before_chat_completion`, store `Arc<Input>` instead of `Input::clone()` | low | S | low | CONFIRMED — mod.rs:2050; only stores for last_message state |
| 7 | Split `MessageContentToolCalls` by round (Vec of round batches) instead of flat extend — enables per-round `Arc` sharing | med | L | med | CONFIRMED — current flat design causes quadratic clone |
| 8 | Avoid `input.clone()` in `save_message` (mod.rs:2076) by taking ownership or modifying in-place | low | S | low | CONFIRMED — cold path, once per chain |

## Open Questions / Needs Verification

1. **How often does `session.messages` actually grow large?** Depends on session duration. Need runtime measurement of typical M₀ at time of tool-call invocation to determine whether `session.messages.clone()` or `tool_calls.clone()` dominates.

2. **How large are typical `ToolResult.output` Values?** File read tools can return 10KB+, web search 50KB+. The O(R²) factor is multiplied by this. Need profiling to confirm actual byte counts.

3. **Does `Input::clone()` in streaming path (common.rs:82) actually clone `tool_calls`?** Yes — `Input` derives `Clone` and `tool_calls: Option<MessageContentToolCalls>` is included. But this needs confirmation that the streaming path actually reaches this clone with non-None `tool_calls` (i.e., after round 1).

4. **Is `MessageContentToolCalls.tool_results` ever accessed by index/position after accumulation?** If round-specific access is needed later, splitting by round would break it. Check API surface of `MessageContentToolCalls`.

5. **Can `session.build_messages` produce a `Cow<Vec<Message>>`?** The Rust lifetime constraints of returning borrowed data from `self.messages` while also potentially appending new messages (user message) creates borrow checker complexity. Requires careful design.

## Hot-Path Classification

**Per-round-trip** (hot path in tool-call chains):
- `tool_calls.clone()` in `build_messages` — O(R×N×size), grows each round → **QUADRATIC across chain**
- `session.messages.clone()` — O(M₀), constant per round but proportional to session depth
- `Input::clone()` × 2-3 (before_chat_completion + HTTP send path) — O(R×N×size) per instance

**Per-chain (once at end):**
- `save_message` `input.clone()` — O(total_tool_calls × size)
- `add_message` `tool_calls.clone()` — O(total_tool_calls × size)

**Summary**: The `tool_calls` accumulation pattern creates **O(R²) total clone work** across a chain. For chains ≥5 rounds with large tool outputs (web search, file reads), this becomes measurable memory bandwidth waste. The primary fix is `Arc<ToolResult>` or `Arc<MessageContentToolCalls>` to reduce clones to pointer copies.
