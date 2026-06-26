# AIChat - Context Loading Strategy

## Optimized Context Loading Order

When working with this codebase, load context in this priority order for maximum efficiency:

### Tier 1: Core State & Config (Always Relevant)
1. `src/config/mod.rs` — Central state, all settings, file paths, working modes (~2600 lines, load relevant sections)
2. `src/config/role.rs` — Role system, `RoleLike` trait (shared interface for Role/Session/Agent)
3. `src/main.rs` — Application entry, mode routing, recursive directive handler

### Tier 2: Domain-Specific (Load Based on Task)

**For Client/API work:**
- `src/client/common.rs` — `Client` trait, request/response types, streaming
- `src/client/model.rs` — Model abstraction, token estimation
- `src/client/macros.rs` — Client registration macro system
- `src/client/mod.rs` — Provider list, registration invocation
- Specific provider file (e.g., `src/client/openai.rs`, `src/client/claude.rs`)

**For Agent work:**
- `src/config/agent.rs` — Agent struct, init, variables, instructions
- `src/function.rs` — Tool call evaluation, external function execution

**For Session work:**
- `src/config/session.rs` — Session lifecycle, message management, compression

**For RAG work:**
- `src/rag/mod.rs` — RAG engine, hybrid search, document sync
- `src/rag/splitter/mod.rs` — Text splitting for document chunking

**For Input processing:**
- `src/config/input.rs` — Input creation, file loading, RAG integration

**For REPL work:**
- `src/repl/mod.rs` — REPL loop, 36 commands, `ask()` function
- `src/repl/completer.rs` — Tab completion logic

**For Server work:**
- `src/serve.rs` — HTTP API, OpenAI-compatible endpoints

**For Rendering:**
- `src/render/stream.rs` — Streaming output
- `src/render/markdown.rs` — Syntax highlighting

### Tier 3: Utilities (Load As Needed)
- `src/utils/mod.rs` — Common helpers (token estimation, env vars, color)
- `src/utils/command.rs` — Shell detection, command execution
- `src/utils/variables.rs` — System variable interpolation
- `src/utils/request.rs` — HTTP fetching, web crawling
- `src/utils/path.rs` — Glob expansion, file operations
- `src/utils/loader.rs` — Document loaders, protocol handlers
- `src/utils/spinner.rs` — Spinner and abortable tasks

## Key Cross-Cutting Concerns

### Config Access Pattern
All modules access config via `GlobalConfig` (`Arc<RwLock<Config>>`):
```rust
let value = config.read().some_field;     // Read
config.write().some_field = new_value;     // Write (keep brief!)
```

### Error Handling
All modules use `anyhow::Result` with `.with_context()` for chain errors.

### Trait Hierarchy
```
RoleLike (trait)
  ├── Role (direct prompt)
  ├── Session (conversation context)
  └── Agent (instructions + tools + RAG)
```
All implement: `model()`, `temperature()`, `top_p()`, `use_tools()`, `set_*()` variants.

### Message Flow
```
Input → Role/Session.build_messages() → patch_messages() → Client.chat_completions()
  ↓                                                              ↓
[RAG patch]                                               ChatCompletionsOutput
  ↓                                                              ↓
patched_text                                             [tool_calls → recurse]
```

## File Size Reference (for context budgeting)
| File | Lines (approx) | Load Priority |
|------|------|------|
| `config/mod.rs` | ~2600 | Selective |
| `serve.rs` | ~850 | Domain |
| `rag/mod.rs` | ~700 | Domain |
| `repl/mod.rs` | ~700 | Domain |
| `client/common.rs` | ~600 | Domain |
| `config/session.rs` | ~540 | Domain |
| `config/input.rs` | ~380 | Domain |
| `config/role.rs` | ~350 | Tier 1 |
| `config/agent.rs` | ~350 | Domain |
| `function.rs` | ~300 | Domain |
| `client/model.rs` | ~380 | Domain |
| `main.rs` | ~300 | Tier 1 |
| `utils/mod.rs` | ~200 | Utility |
| All others | <200 | As needed |
