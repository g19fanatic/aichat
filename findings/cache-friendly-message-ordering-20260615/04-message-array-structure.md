# Finding 04: Message Array Structure Analysis

## Overview

This document analyzes how the `messages[]` array is structured for each invocation scenario in aichat, identifies which parts are static vs dynamic across multiple invocations, and maps where user content sits relative to system prompts and history.

---

## (a) Message Array Layout for Each Scenario

### Scenario 1: Role-Only (No Session, No Agent)

**Code path**: `Input::build_messages()` → `Role::build_messages()` (src/config/role.rs:225)

Three sub-cases exist:

#### 1a. Empty Prompt (no role set, default derived role)

```
messages = [
  { role: "user", content: <user_content> }
]
```

- `<user_content>` = `Input::message_content()` = the assembled text (prompt + files)
- No system message at all

#### 1b. Embedded Prompt (role contains `__INPUT__` placeholder)

```
messages = [
  { role: "user", content: <merged_content> }
]
```

- The role's prompt has `__INPUT__` replaced with the user's content via `merge_prompt()`
- Example: Role prompt = `"Translate the following:\n__INPUT__"` → user content gets embedded
- Result is a single user message with role instructions baked in

#### 1c. Structured Prompt (normal role with system message ± few-shot examples)

The role prompt is parsed by `parse_structure_prompt()` (src/config/role.rs:305):
- Text before `### INPUT:` → system message
- `### INPUT:` / `### OUTPUT:` pairs → few-shot examples

```
messages = [
  { role: "system",    content: <role_system_text> },         // [0] System
  { role: "user",      content: <few_shot_input_1> },         // [1] Few-shot example
  { role: "assistant", content: <few_shot_output_1> },        // [2] Few-shot example
  { role: "user",      content: <few_shot_input_2> },         // [3] Few-shot example
  { role: "assistant", content: <few_shot_output_2> },        // [4] Few-shot example
  ...
  { role: "user",      content: <user_content> }              // [N] Current user message
]
```

If `continue_output` is set, an additional Assistant message is appended at the end.

---

### Scenario 2: Session (With Session, No Agent)

**Code path**: `Input::build_messages()` → `Session::build_messages()` (src/config/session.rs:525)

#### 2a. First Message (empty session)

Delegates to `input.role().build_messages(input)` — identical to Scenario 1 above.

#### 2b. Subsequent Messages (session has history)

```
messages = [
  { role: "system",    content: <role_system_text> },         // [0] From initial role
  { role: "user",      content: <first_user_message> },       // [1] First user turn
  { role: "assistant", content: <first_assistant_reply> },    // [2] First assistant reply
  { role: "user",      content: <second_user_message> },      // [3] Second turn
  { role: "assistant", content: <second_assistant_reply> },   // [4] Second reply
  ...
  { role: "user",      content: <current_user_content> }      // [N] NEW message appended
]
```

Key detail: When `messages.len() == 1` AND `compressed_messages.len() >= 2`, the session inserts compressed history from the last user message onward (src/config/session.rs:541-548).

#### 2c. With Compressed Messages

```
messages = [
  { role: "user",   content: <compressed_summary> },          // [0] Compressed context
  ...compressed_messages from last user onwards...
  { role: "user",   content: <current_user_content> }         // [N] NEW message
]
```

---

### Scenario 3: Agent (With Agent + Session)

**Code path**: `Agent::to_role()` (src/config/agent.rs:329) → `Session::sync_agent()` (src/config/session.rs:289) → `Session::build_messages()` → `Role::build_messages()`

Agents **always** use sessions. The agent's `interpolated_instructions()` becomes the `role_prompt`:

```rust
// session.rs:289
pub fn sync_agent(&mut self, agent: &Agent) {
    self.role_name = None;
    self.role_prompt = agent.interpolated_instructions();
    self.agent_variables = agent.variables().clone();
    self.agent_instructions = self.role_prompt.clone();
}
```

The `interpolated_instructions()` (src/config/agent.rs:230) resolves:
1. `session_dynamic_instructions` (if dynamic_instructions enabled)
2. OR `shared_dynamic_instructions`
3. OR `config.instructions`
4. OR `definition.instructions` (from index.yaml)
5. Then substitutes `{{variable}}` placeholders with variable values

#### Agent layout (first message):

```
messages = [
  { role: "system",    content: <agent_instructions> },       // [0] Interpolated instructions
  { role: "user",      content: <user_content> }              // [1] User's input
]
```

#### Agent layout (subsequent messages with tool calls):

```
messages = [
  { role: "system",    content: <agent_instructions> },       // [0] Agent system prompt
  { role: "user",      content: <user_msg_1> },               // [1] First user input
  { role: "assistant", content: <tool_calls_1> },             // [2] Tool call request
  { role: "tool",      content: <tool_results_1> },           // [3] Tool results
  { role: "assistant", content: <reply_1> },                  // [4] Final reply
  { role: "user",      content: <user_msg_2> },               // [5] Second user input
  ...
  { role: "user",      content: <current_user_content> }      // [N] Current input
]
```

Note: If `dynamic_instructions` is enabled, the agent instructions can change between sessions (via `_instructions` function execution), but within a single session they remain stable after `sync_agent()` is called.

---

## (b) Which Messages/Parts Are Cacheable

### Static vs Dynamic Classification

| Component | Stability | Cacheable? | Notes |
|-----------|-----------|------------|-------|
| System message (role prompt) | **STATIC** per role | ✅ Yes | Same text every call with the same role |
| Few-shot examples | **STATIC** per role | ✅ Yes | Defined in role, never changes |
| Agent instructions (non-dynamic) | **STATIC** per agent config | ✅ Yes | From definition.instructions + variables |
| Agent instructions (dynamic) | **SEMI-STATIC** | ⚠️ Partially | Changes when `_instructions` fn runs, but stable within a session |
| Conversation history | **GROWING** (append-only) | ✅ Prefix stable | Previous turns never change; only new turns append |
| Tool call/result pairs | **STATIC** once recorded | ✅ Prefix stable | Past tool interactions don't change |
| User message (file contents) | **STATIC** per invocation set | ✅ If positioned first | Same files = same content across calls |
| User message (prompt text) | **DYNAMIC** every call | ❌ No | Changes with every new question |

### Provider-Specific Cacheability

#### OpenAI (Prefix Caching)
- Caches based on the **first ~256 tokens** used as a hash to route to a specific machine
- Then caches the **longest prefix match** from position 0
- **Static system message**: ✅ Cached (always at position 0)
- **Few-shot examples**: ✅ Cached (stable prefix after system)
- **Conversation history**: ✅ Growing prefix cached
- **User message content**: The internal ordering of the user message matters critically — if prompt (dynamic) is first, the content hash changes and routes to different machines

#### Claude (Explicit Cache Control)
- System message extracted to top-level `"system"` field (src/client/message.rs:225, called in claude.rs:174)
- Existing `cache_control` logic in codebase (claude.rs:280-335):
  - `cache_control: {"type": "ephemeral"}` added to system message block
  - `cache_control` added to last message content block
- Cache breakpoints can be placed on any message block
- **System message**: ✅ Cacheable (explicit cache_control already implemented)
- **Conversation history**: ✅ Cacheable prefix (would benefit from ordering fix)
- **User message**: The last user message gets cache_control, but internal content ordering still matters for incremental caching

#### Gemini/VertexAI (Context Caching)
- System message extracted to `"systemInstruction"` field (vertexai.rs:321)
- Messages go into `"contents"` array
- Google's context caching is explicit (requires a separate API call to create a cached context)
- **System instruction**: ✅ Cacheable via context caching API
- **Conversation contents**: ✅ Prefix cacheable

### Key Insight: Intra-Message Ordering Problem

Within the **final user message** itself (the one containing both file contents and prompt text), the ordering determines cache efficiency:

```
Current assembly (Input::from_files, input.rs:57-120):
┌─────────────────────────────────────────┐
│ texts.push(raw_text)   ← PROMPT FIRST   │  ← DYNAMIC (changes every call)
│ texts.push(file_1)     ← FILES AFTER    │  ← STATIC  (same between calls)
│ texts.push(file_2)                      │  ← STATIC
│ text = texts.join("\n")                  │
└─────────────────────────────────────────┘
```

Result: The first bytes of the user message content are **always different** across invocations with different prompts, even when the files are identical.

---

## (c) Where User Content Sits in Relation to System Prompts and History

### Position Map (all scenarios):

```
┌───────────────────────────────────────────────────────────┐
│ POSITION 0: System Message                                │
│   - Role system prompt (role.rs structured prompt)        │
│   - OR Agent interpolated instructions                    │
│   - EXTRACTED by Claude/Gemini to separate field          │
│   - KEPT in messages[] for OpenAI                         │
├───────────────────────────────────────────────────────────┤
│ POSITIONS 1..N-1: History / Few-shot                      │
│   - Few-shot INPUT/OUTPUT pairs (role scenario)           │
│   - OR Conversation history (session scenario)            │
│   - Alternating User/Assistant/Tool messages              │
├───────────────────────────────────────────────────────────┤
│ POSITION N (LAST): Current User Message                   │
│   - Contains: Input::message_content()                    │
│   - Which is: MessageContent::Text(self.text())           │
│   - Where text = [raw_text + file_documents joined]       │
│                                                           │
│   Internal structure of final user message content:       │
│   ┌─────────────────────────────────────────────────┐    │
│   │ "what does this code do?\n"          ← PROMPT   │    │
│   │ "\n============ FILE: A.txt ===...   ← FILE 1   │    │
│   │ "\n============ FILE: B.txt ===...   ← FILE 2   │    │
│   └─────────────────────────────────────────────────┘    │
└───────────────────────────────────────────────────────────┘
```

### Provider Serialization Differences:

```
┌─ OpenAI ─────────────────────────────────────┐
│ {                                             │
│   "messages": [                               │
│     {"role":"system", "content":"..."},       │  ← System in array
│     {"role":"user", "content":"..."},         │  ← History...
│     {"role":"assistant", "content":"..."},    │
│     {"role":"user", "content":"<LAST_MSG>"}   │  ← Final user msg
│   ]                                           │
│ }                                             │
└───────────────────────────────────────────────┘

┌─ Claude ─────────────────────────────────────┐
│ {                                             │
│   "system": "...",                            │  ← Extracted out
│   "messages": [                               │
│     {"role":"user", "content":"..."},         │  ← History (no system)
│     {"role":"assistant", "content":"..."},    │
│     {"role":"user", "content":"<LAST_MSG>"}   │  ← Final user msg
│   ]                                           │
│ }                                             │
└───────────────────────────────────────────────┘

┌─ Gemini ─────────────────────────────────────┐
│ {                                             │
│   "systemInstruction": {"parts":[...]},       │  ← Extracted out
│   "contents": [                               │
│     {"role":"user", "parts":[...]},           │  ← History (no system)
│     {"role":"model", "parts":[...]},          │
│     {"role":"user", "parts":[{"text":"..."}]} │  ← Final user msg
│   ]                                           │
│ }                                             │
└───────────────────────────────────────────────┘
```

### `patch_messages()` Post-Processing (src/client/message.rs:206)

After messages are built but before provider serialization:

1. **`system_prompt_prefix`**: If model defines one, it's prepended to the existing system message (or a new system message is inserted at position 0)
2. **`no_system_message`**: If model flag is set, the system message is removed and its content is merged into the first user message via `merge_system()`

`merge_system()` behavior (message.rs:28-55):
- If user content is Text: converts to Array with `[system_text_part, user_text_part]`
- If user content is Array: inserts system text at position 0 of the array

### The Critical Cache Problem Illustrated

For repeated calls with same files but different prompts:

```
Call 1: aichat --file code.rs -- "explain this"
Call 2: aichat --file code.rs -- "find bugs in this"
Call 3: aichat --file code.rs -- "add tests for this"
```

**Current user message content (prompt FIRST):**
```
Call 1: "explain this\n\n============ FILE: code.rs ============\n<5000 tokens of code>"
Call 2: "find bugs in this\n\n============ FILE: code.rs ============\n<5000 tokens of code>"
Call 3: "add tests for this\n\n============ FILE: code.rs ============\n<5000 tokens of code>"
```

- First tokens differ → OpenAI routes to different machines → 0% cache hit
- Prefix differs at byte 0 → no prefix match possible → 0% cache benefit

**Proposed user message content (files FIRST, prompt LAST):**
```
Call 1: "\n============ FILE: code.rs ============\n<5000 tokens of code>\n\nexplain this"
Call 2: "\n============ FILE: code.rs ============\n<5000 tokens of code>\n\nfind bugs in this"
Call 3: "\n============ FILE: code.rs ============\n<5000 tokens of code>\n\nadd tests for this"
```

- First ~5000 tokens identical → OpenAI routes to SAME machine → prefix cached
- Prefix matches up to the prompt → significant cache savings
- Only the small trailing prompt text causes a cache miss

---

## Summary Table

| Scenario | System Msg | History | User Content Position | Cacheable Prefix |
|----------|-----------|---------|----------------------|------------------|
| Role-only (empty) | None | None | `messages[0]` (only msg) | None without files-first |
| Role-only (embedded) | None (baked in) | None | `messages[0]` (merged) | Role prompt prefix |
| Role-only (structured) | `messages[0]` | Few-shot `[1..N-1]` | `messages[N]` (last) | System + few-shot |
| Session (first msg) | `messages[0]` | None | `messages[1]` | System msg |
| Session (subsequent) | `messages[0]` | `[1..N-1]` | `messages[N]` (last) | System + all history |
| Agent (first msg) | `messages[0]` | None | `messages[1]` | Agent instructions |
| Agent (subsequent) | `messages[0]` | `[1..N-1]` + tool calls | `messages[N]` (last) | Instructions + history |

**The fix target**: `Input::from_files()` at src/config/input.rs:79-82 — reorder `texts[]` to push file documents BEFORE raw_text, making the static file content form the cacheable prefix of the user message.
