# AIChat - Code Patterns & Design Decisions

## Core Design Patterns

### 1. Global Shared Configuration (`GlobalConfig`)
- **Pattern**: `Arc<RwLock<Config>>` — Thread-safe shared mutable state
- **Location**: `src/config/mod.rs:161` — Type alias definition
- **Usage**: Passed through all layers; read with `.read()`, write with `.write()`
- **Rationale**: Single source of truth for all runtime state (model, role, session, agent, RAG)
- **Critical invariant**: Write lock held only briefly to avoid deadlocks; no async operations while holding write lock

### 2. Macro-Based Client Registration (`register_client!`)
- **Pattern**: Declarative macro generates enums, structs, and trait impls for all providers
- **Location**: `src/client/macros.rs:1-200`
- **Generated artifacts**:
  - `ClientConfig` enum (serde-tagged by `"type"`)
  - Per-provider `XxxClient` structs
  - `init_client()` — factory function trying each provider
  - `list_client_types()`, `list_client_names()`, `list_all_models()`, `list_models()`
- **Companion macros**: `client_common_fns!`, `impl_client_trait!`, `config_get_fn!`
- **Adding a new provider**: Add a tuple to `register_client!()` in `src/client/mod.rs`, create `src/client/new_provider.rs` with `PROMPTS`, config struct, and prepare/execute functions

### 3. Trait-Based Polymorphism (`RoleLike`, `Client`)
- **`RoleLike` trait** (`src/config/role.rs:27-36`): Abstracts Role, Session, and Agent — provides model/temperature/top_p/use_tools access
  - Implemented by: `Role`, `Session`, `Agent`
  - Used in `Config::role_like_mut()` to modify the "active" configuration target
- **`Client` trait** (`src/client/common.rs:42-157`): Async trait for all LLM providers
  - Methods: `chat_completions()`, `chat_completions_streaming()`, `embeddings()`, `rerank()`
  - Default implementations handle dry-run, abort signals, and client building
  - `request_builder()` / `patch_request_data()` handle per-model request patching

### 4. Recursive Tool Call Loop
- **CMD mode**: `start_directive()` (`src/main.rs:154-183`) — Recursive async function
- **REPL mode**: `ask()` (`src/repl/mod.rs:~580-615`) — Recursive async function
- **Pattern**: Call LLM → if `tool_calls` returned → `eval_tool_calls()` → merge results → recurse
- **Safety**: `ToolCall::dedup()` detects and breaks infinite loops by checking duplicate call IDs

### 5. Streaming Architecture (Channel-Based)
- **Pattern**: `UnboundedSender<SseEvent>` → `UnboundedReceiver<SseEvent>`
- **Producer**: `SseHandler` (`src/client/stream.rs`) collects text chunks and tool calls
- **Consumer**: `render_stream()` → `markdown_stream()` or `raw_stream()` (`src/render/stream.rs`)
- **Concurrency**: `tokio::join!` runs producer and consumer simultaneously
- **Events**: `SseEvent::Text(String)` and `SseEvent::Done`

### 6. State Machine (Session Lifecycle)
```
None → use_session() → Session (empty) → add_message() → Session (with messages)
                                       → compress() → Session (compressed + new system msg)
                                       → exit_session() → save/discard → None
```
- **Dirty tracking**: `session.dirty` flag tracks unsaved changes
- **Auto-naming**: Background task (`maybe_autoname_session`) via `%create-title%` role
- **Auto-compression**: Background task (`maybe_compress_session`) when `tokens > compress_threshold`

### 7. RAG Hybrid Search
- **Vector search**: HNSW index (`hnsw_rs`) with cosine distance
- **Keyword search**: BM25 (`bm25` crate) with English language support
- **Fusion**: Reciprocal Rank Fusion (RRF) by default, or optional reranking via dedicated model
- **Location**: `src/rag/mod.rs:290-370` — `hybird_search()` method

### 8. Request Patching System
- **Location**: `src/client/common.rs:240-300` — `RequestData::apply_patch()`
- **Mechanism**: JSON merge-patch applied to URL, body, and headers
- **Sources**: Per-model `patch` field in `ModelData`, provider-level `patch` config, env var override
- **Use case**: Adding safety settings to Gemini, customizing headers per model

## Key Data Structures

### Message Pipeline
```
Input → [RAG search → patched_text] → build_messages() → [patch_messages()] → ChatCompletionsData → API
                                                                                       ↓
                                                                              ChatCompletionsOutput
                                                                                       ↓
                                                                              [eval_tool_calls()]
```

### Configuration Hierarchy
```
Config (global defaults)
  ├── Role (prompt + model overrides)
  │     └── Temperature, top_p, model_id, use_tools
  ├── Session (persistent conversation)
  │     └── Messages, compressed_messages, role sync
  ├── Agent (instructions + tools + RAG)
  │     └── AgentConfig + AgentDefinition + Variables
  └── RAG (document retrieval)
        └── Embedding model + HNSW + BM25 + data
```

### Variable Interpolation
- **System variables** (`src/utils/variables.rs`): `{{__os__}}`, `{{__shell__}}`, `{{__now__}}`, `{{__cwd__}}`, etc.
- **Agent variables** (`src/config/agent.rs:197-205`): `{{var_name}}` replaced from shared/session variables
- **Macro variables**: Same `{{var_name}}` syntax in macro command steps
- **REPL prompt variables** (`src/utils/render_prompt.rs`): `{var}`, `{?var ...}`, `{!var ...}` conditional blocks

## Error Handling Patterns

### Provider Error Normalization
- **Location**: `src/client/common.rs:430-480` — `catch_error()`
- **Pattern**: Multiple fallback paths for different provider error JSON formats
- Checks `error.type + message`, `error.code + message`, `errors[0].code + message`, `detail + status`, `error` (string), `message` (string)

### Abort Signal Propagation
- **Type**: `AbortSignal = Arc<AbortSignalInner>` with atomic booleans for Ctrl+C and Ctrl+D
- **Usage**: Threaded through all async operations; checked in spin loops and `tokio::select!`
- **Integration**: `poll_abort_signal()` uses `crossterm::event::poll()` in raw terminal mode

## File System Layout (Runtime)

```
$XDG_CONFIG_HOME/aichat/     (or ~/.config/aichat/)
├── config.yaml              # Main configuration
├── .env                     # Environment variables
├── roles/                   # Custom role files (*.md)
├── sessions/                # Saved sessions (*.yaml)
│   └── _/                   # Auto-saved sessions with timestamps
├── rags/                    # RAG databases (*.yaml)
├── macros/                  # Macro definitions (*.yaml)
├── functions/
│   ├── functions.json       # Tool declarations
│   ├── bin/                 # Tool executables
│   └── agents/              # Agent definitions
│       └── <agent>/
│           ├── index.yaml   # Agent definition
│           └── functions.json # Agent-specific tools
├── agents/                  # Agent runtime data
│   └── <agent>/
│       ├── config.yaml      # Agent config overrides
│       ├── rag.yaml         # Agent RAG data
│       ├── sessions/        # Agent sessions
│       └── messages.md      # Agent message log
├── messages.md              # Global message log
├── models-override.yaml     # Synced model definitions
└── aichat.log               # Debug log
```

## Threading Model

- **Main thread**: Tokio multi-thread runtime (`#[tokio::main]`)
- **REPL**: Synchronous `reedline` editor on main thread; async `ask()` via tokio
- **Background tasks**: `tokio::spawn` for auto-naming and auto-compression (fire-and-forget)
- **Server**: `hyper` + `tokio-graceful` for HTTP handling
- **Concurrency**: `parking_lot::RwLock` (sync) for GlobalConfig; `tokio::sync::mpsc` for streaming

## Notable Implementation Details

1. **Token estimation** (`src/utils/mod.rs:68-80`): Heuristic-based — ASCII words × 1.3, CJK characters × 0.5-1.0
2. **Think tag stripping** (`src/utils/mod.rs:82-84`): Regex removes `<think>...</think>` blocks from model responses (for reasoning models like DeepSeek-R1)
3. **Shell detection** (`src/utils/command.rs:25-65`): Auto-detects bash/zsh/fish/powershell/nushell from `$SHELL` or `$PSModulePath`
4. **Fuzzy JSON parsing**: Uses `jsonic` crate for tolerant parsing of LLM-generated function call arguments
5. **Embedded assets**: `rust-embed` for built-in roles (`%shell%`, `%code%`, etc.), syntaxes, and themes
6. **Terminal color detection**: `terminal-colorsaurus` for automatic light/dark theme selection
7. **Code extraction** (`extract_code_block`): Regex-based extraction of code blocks for `--code` mode
8. **Request patching**: JSON merge-patch (`json-patch`) enables per-model/per-provider API customization
9. **Document loaders**: Extensible via config — `pdf: 'pdftotext $1 -'`, `docx: 'pandoc --to plain $1'`
10. **Model sync**: `--sync-models` fetches latest `models.yaml` from GitHub repository
