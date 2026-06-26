# AIChat - Architecture & Module Map

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────┐
│                      CLI Entry (main.rs)                  │
│  Parse args → Determine WorkingMode → Initialize Config   │
│                    ↓           ↓           ↓              │
│               CMD Mode    REPL Mode    Serve Mode         │
└───────┬──────────┬──────────┬──────────┬─────────────────┘
        │          │          │          │
   ┌────▼───┐ ┌───▼────┐ ┌──▼───┐ ┌───▼────────┐
   │ Input  │ │  REPL  │ │Serve │ │   Config   │
   │ (files,│ │(reedline│ │(hyper│ │(roles,     │
   │  text, │ │ prompt │ │ HTTP │ │ sessions,  │
   │  URLs) │ │ loop)  │ │server│ │ agents)    │
   └───┬────┘ └───┬────┘ └──┬───┘ └─────┬─────┘
       │          │          │           │
       └──────────┼──────────┘           │
                  │                      │
          ┌───────▼──────────────────────▼───────┐
          │           Client Layer                │
          │  (OpenAI, Claude, Gemini, Bedrock,    │
          │   Cohere, VertexAI, AzureOpenAI,      │
          │   18 OpenAI-compatible providers)      │
          └──────────────┬───────────────────────┘
                         │
              ┌──────────┼──────────┐
              ▼          ▼          ▼
         ┌────────┐ ┌────────┐ ┌────────┐
         │ Chat   │ │Embeddings│ │Rerank │
         │Complets│ │  API    │ │ API   │
         └────────┘ └────────┘ └────────┘
                         │          │
                    ┌────▼──────────▼────┐
                    │    RAG System      │
                    │ (HNSW + BM25 +     │
                    │  Reranking)        │
                    └───────────────────┘
```

## Module Structure

### `src/main.rs` — Application Entry Point
- **Location**: `src/main.rs:1-243`
- Parses CLI with `clap`, determines `WorkingMode` (Cmd/Repl/Serve)
- Creates `GlobalConfig` via `Arc<RwLock<Config>>`
- Routes to `start_directive()` (CMD), `start_interactive()` (REPL), or `serve::run()` (Serve)
- `start_directive()` (`main.rs:154-183`): Recursive function for CMD mode with tool call loop
- `shell_execute()` (`main.rs:190-251`): Natural language → shell command with e/r/d/c/q workflow
- `create_input()` (`main.rs:253-268`): Factory for Input from text/files

### `src/cli.rs` — CLI Argument Definitions
- **Location**: `src/cli.rs:1-107`
- `Cli` struct with clap derive macros for all flags/options
- Key flags: `--model`, `--role`, `--session`, `--agent`, `--rag`, `--serve`, `--execute`, `--code`, `--file`
- `Cli::text()`: Merges positional args with stdin pipe

### `src/config/` — Configuration & State Management

#### `src/config/mod.rs` — Core Config (~2600 lines)
- **Location**: `src/config/mod.rs:1-2600+`
- `Config` struct: Central state holding model, role, session, agent, RAG, functions, all settings
- `GlobalConfig = Arc<RwLock<Config>>`: Thread-safe shared config
- **Key entry points**:
  - `Config::init()` (`mod.rs:165-195`): Async config init, loads file/env, sets up model
  - `Config::use_agent()` (`mod.rs:1075-1100`): Initialize agent with optional session
  - `Config::use_session()` (`mod.rs:835-900`): Start/join session
  - `Config::use_rag()` (`mod.rs:970-1000`): Initialize RAG
  - `Config::compress_session()` (`mod.rs:930-960`): Async session compression via LLM summarization
  - `Config::select_functions()` (`mod.rs:1260-1320`): Resolve which tools are available for a role
  - `macro_execute()` (`mod.rs:~2400`): Execute macro steps
- **File Layout**:
  - `config_dir()` / `roles_dir()` / `sessions_dir()` / `functions_dir()` / `agents_data_dir()` — XDG-compatible paths
  - `WorkingMode` enum: `Cmd`, `Repl`, `Serve`
  - `StateFlags` bitflags: `ROLE | SESSION_EMPTY | SESSION | RAG | AGENT`
  - `AssertState` enum: Used for REPL command visibility/validation

#### `src/config/agent.rs` — AI Agent System
- **Location**: `src/config/agent.rs:1-350`
- `Agent` struct: name, config, definition, variables (shared/session), functions, rag, model
- `Agent::init()` (`agent.rs:34-108`): Load definition from `index.yaml`, functions from `functions.json`, optional RAG
- `Agent::interpolated_instructions()` (`agent.rs:197-205`): Variable substitution in instructions `{{var}}`
- `Agent::init_agent_variables()` (`agent.rs:110-165`): Interactive or config-based variable initialization
- `AgentConfig`: Model override, temperature, tools, instructions, variables
- `AgentDefinition`: name, description, version, instructions, dynamic_instructions flag, variables, conversation_starters, documents
- `list_agents()` / `complete_agent_variables()`: Discovery functions

#### `src/config/role.rs` — Role System
- **Location**: `src/config/role.rs:1-350`
- `Role` struct: name, prompt, model_id, temperature, top_p, use_tools
- `RoleLike` trait: Abstraction implemented by Role, Session, Agent — provides `model()`, `temperature()`, etc.
- Built-in roles via `rust_embed`: `%shell%`, `%explain-shell%`, `%code%`, `%create-title%`
- Role files support YAML frontmatter metadata (`---\nmodel: ...\ntemperature: ...\n---`)
- `parse_structure_prompt()`: Supports `### INPUT:` / `### OUTPUT:` few-shot format
- `Role::build_messages()` (`role.rs:190-230`): Converts role+input into Message vec with system/user/assistant

#### `src/config/session.rs` — Session Management
- **Location**: `src/config/session.rs:1-540`
- `Session` struct: model, temperature, messages, compressed_messages, data_urls, agent state
- Session persistence: YAML files in sessions directory
- `Session::build_messages()` (`session.rs:430-470`): Builds message chain including compressed history
- `Session::compress()`: Replaces messages with LLM-generated summary
- Auto-naming: Generates session names from chat content via `%create-title%` role
- `Session::add_message()` (`session.rs:375-425`): Handles normal, continue, and regenerate flows
- Token tracking: `update_tokens()` recalculates on every message change

#### `src/config/input.rs` — Input Processing
- **Location**: `src/config/input.rs:1-380`
- `Input` struct: text, medias (images), data_urls, tool_calls, role, RAG integration
- `Input::from_str()`: Simple text input
- `Input::from_files()` / `from_files_with_spinner()`: Load local files, URLs, external commands
- `Input::use_embeddings()` (`input.rs:200-212`): RAG-enhanced input via vector search
- `Input::prepare_completion_data()` (`input.rs:230-247`): Builds `ChatCompletionsData` for API calls
- `Input::merge_tool_results()`: Chains tool call results back into input for recursive calls
- Media handling: Images → base64 data URLs, documents → text with headers

### `src/client/` — LLM Provider Clients

#### `src/client/mod.rs` — Client Registry
- **Location**: `src/client/mod.rs:1-50`
- `register_client!` macro instantiation for 8 client types
- `OPENAI_COMPATIBLE_PROVIDERS`: 18 providers sharing OpenAI-compatible protocol

#### `src/client/macros.rs` — Client Registration Macros
- **Location**: `src/client/macros.rs:1-200`
- `register_client!`: Generates `ClientConfig` enum, per-client structs, `init_client()`, `list_models()`
- `client_common_fns!`: Common trait method implementations
- `impl_client_trait!`: Wires prepare/execute functions into the `Client` trait
- `config_get_fn!`: Per-field config getter with env var fallback

#### `src/client/common.rs` — Client Trait & Shared Logic
- **Location**: `src/client/common.rs:1-600`
- `Client` trait: `chat_completions()`, `chat_completions_streaming()`, `embeddings()`, `rerank()`
- `RequestData`: URL + headers + body builder with `apply_patch()` for request patching
- `ChatCompletionsData` / `ChatCompletionsOutput`: Request/response types
- `call_chat_completions()` / `call_chat_completions_streaming()`: Top-level execution with spinner/abort
- `catch_error()`: Unified error extraction from various provider error formats
- Provider model list: `ALL_PROVIDER_MODELS` loaded from embedded `models.yaml` or override file

#### `src/client/model.rs` — Model Abstraction
- **Location**: `src/client/model.rs:1-380`
- `Model`: client_name + `ModelData` (name, type, max_tokens, pricing, capabilities)
- `Model::retrieve_model()`: Resolves `provider:model-name` to Model instance
- `ModelType` enum: `Chat`, `Embedding`, `Reranker`
- Token estimation: `messages_tokens()` / `total_tokens()` / `guard_max_input_tokens()`

#### `src/client/message.rs` — Message Types
- **Location**: `src/client/message.rs:1-220`
- `Message` { role, content }: Core chat message
- `MessageRole`: System, Assistant, User, Tool
- `MessageContent`: Text | Array(multimodal) | ToolCalls
- `MessageContentToolCalls`: Wraps tool results for multi-turn tool use
- `patch_messages()`: Handles `no_system_message` and `system_prompt_prefix` model quirks

#### `src/client/stream.rs` — SSE & JSON Streaming
- **Location**: `src/client/stream.rs:1-260`
- `SseHandler`: Manages streaming text and tool calls via unbounded channel
- `sse_stream()`: Generic SSE event consumer
- `json_stream()`: NDJSON / JSON array stream parser
- `JsonStreamParser`: Character-level JSON object boundary detection

#### Provider-specific clients:
- `src/client/openai.rs`: OpenAI API
- `src/client/openai_compatible.rs`: Generic OpenAI-compatible wrapper
- `src/client/claude.rs`: Anthropic Claude (custom format)
- `src/client/gemini.rs`: Google Gemini / AI Studio
- `src/client/cohere.rs`: Cohere v2
- `src/client/azure_openai.rs`: Azure OpenAI
- `src/client/vertexai.rs`: Google VertexAI
- `src/client/bedrock.rs`: AWS Bedrock (SigV4 auth, custom event stream)
- `src/client/access_token.rs`: OAuth token refresh for VertexAI/Bedrock

### `src/function.rs` — Function Calling / Tools
- **Location**: `src/function.rs:1-300`
- `Functions`: Loads declarations from `functions.json`
- `FunctionDeclaration`: name, description, `JsonSchema` parameters, agent flag
- `ToolCall` / `ToolResult`: Call+response pair
- `eval_tool_calls()`: Execute tool calls with dedup and infinite-loop detection
- `ToolCall::eval()`: Resolves call config (agent vs global), runs external binary
- `run_llm_function()`: Spawns external command with PATH prepended, captures output via `LLM_OUTPUT` file
- Uses `jsonic` crate for fuzzy/lenient JSON parsing of LLM-generated arguments

### `src/rag/` — Retrieval Augmented Generation

#### `src/rag/mod.rs` — RAG Engine (~700 lines)
- **Location**: `src/rag/mod.rs:1-700`
- `Rag` struct: embedding model, HNSW index, BM25 search engine, data
- **Hybrid search**: Vector (HNSW cosine) + Keyword (BM25) with Reciprocal Rank Fusion or reranking
- `Rag::init()`: Interactive setup — select embedding model, chunk size, add documents
- `Rag::search()`: `hybird_search()` → optional reranking → returns formatted context
- `Rag::sync_documents()`: Load/hash/split/embed documents, delta updates
- `RagData`: Serializable state with files, vectors, config
- `DocumentId`: Packed file_index + document_index in single `usize`
- Document splitting: `RecursiveCharacterTextSplitter` (in `splitter/` submodule)
- Batch embedding: Respects model's `max_batch_size` and `max_input_tokens` with retry logic

### `src/render/` — Output Rendering

#### `src/render/mod.rs` — Render Orchestration
- **Location**: `src/render/mod.rs:1-25`
- `render_stream()`: Routes to `markdown_stream()` or `raw_stream()` based on terminal/highlight config

#### `src/render/markdown.rs` — Syntax-Highlighted Markdown
- **Location**: `src/render/markdown.rs:1-320`
- `MarkdownRender`: Uses `syntect` for code highlighting + text wrapping
- Handles code block detection, language-specific syntax, theme-aware coloring
- Embedded syntax definitions from `syntaxes.bin`

#### `src/render/stream.rs` — Streaming Output
- **Location**: `src/render/stream.rs:1-180`
- `markdown_stream()`: Real-time token-by-token rendering with cursor management (raw mode)
- `raw_stream()`: Simple print-as-you-go for piped output

### `src/serve.rs` — HTTP Server
- **Location**: `src/serve.rs:1-850`
- Built on `hyper` + `tokio`
- **Endpoints**:
  - `POST /v1/chat/completions` — OpenAI-compatible chat API (stream + non-stream)
  - `POST /v1/embeddings` — Embeddings API
  - `POST /v1/rerank` — Rerank API
  - `GET /v1/models` — List models
  - `GET /v1/roles` — List roles
  - `GET /v1/rags` — List RAGs
  - `POST /v1/rags/search` — Search RAG
  - `GET /playground` — LLM Playground HTML
  - `GET /arena` — LLM Arena HTML
- Supports streaming via SSE and non-streaming JSON responses
- Tool calls forwarded back to client in streaming format

### `src/repl/` — Interactive REPL

#### `src/repl/mod.rs` — REPL Core (~700 lines)
- **Location**: `src/repl/mod.rs:1-700`
- `Repl` struct: Reedline editor with custom completer, highlighter, prompt
- 36 dot-commands (`.help`, `.model`, `.role`, `.session`, `.agent`, `.rag`, `.macro`, `.file`, `.set`, etc.)
- `run_repl_command()`: Main command dispatcher (~400 lines of match arms)
- `ask()`: Recursive function for chat with tool call loop
- Multi-line input: `:::` delimiters
- `split_args_text()`: Shell-like argument parsing with quote handling

#### `src/repl/completer.rs` — Tab Completion
- Context-aware completion for commands, models, roles, sessions, agents, macros, settings

#### `src/repl/highlighter.rs` — Syntax Highlighting
- Highlights recognized REPL commands in green

#### `src/repl/prompt.rs` — Dynamic Prompt
- Renders left/right prompts using template variables (model, session, tokens, role, etc.)

### `src/utils/` — Utility Modules

| File | Purpose |
|------|---------|
| `mod.rs` | Common utilities: env helpers, token estimation, text formatting, fuzzy filtering |
| `abort_signal.rs` | `AbortSignal` for cooperative cancellation (Ctrl+C/D) |
| `clipboard.rs` | Cross-platform clipboard with OSC52 fallback |
| `command.rs` | Shell detection, command execution, shell history append |
| `crypto.rs` | SHA256, HMAC-SHA256, hex/base64 encode/decode |
| `html_to_md.rs` | HTML→Markdown conversion via `html_to_markdown` |
| `input.rs` | `read_single_key()` for shell-execute menu |
| `loader.rs` | Document loaders (file, URL, recursive URL, protocol-based) |
| `path.rs` | Glob expansion, safe path joining, file listing |
| `render_prompt.rs` | Template engine for REPL prompts with `{var}`, `{?var}`, `{!var}` |
| `request.rs` | HTTP fetching, website crawling, GitHub repo tree crawling |
| `spinner.rs` | Terminal spinner animation with async integration |
| `variables.rs` | System variable interpolation: `{{__os__}}`, `{{__shell__}}`, `{{__now__}}`, etc. |

## Data Flow: Chat Completion

```
User Input → Input::from_str/from_files()
     ↓
Input::use_embeddings() → [RAG search if active]
     ↓
Input::build_messages() → [Role/Session message construction]
     ↓
Config::select_functions() → [Tool declarations]
     ↓
Input::prepare_completion_data() → ChatCompletionsData
     ↓
Client::chat_completions[_streaming]() → API call
     ↓
ChatCompletionsOutput { text, tool_calls }
     ↓
[If tool_calls] → eval_tool_calls() → run_llm_function() → merge → recurse
     ↓
Config::after_chat_completion() → save to session/messages
```

## Data Flow: Agent Initialization

```
Config::use_agent("agent-name")
     ↓
Agent::init() → Load index.yaml + functions.json
     ↓  
[Optional] Rag::load() or Rag::init() (from agent documents)
     ↓
AgentConfig::load() → merge with env vars
     ↓
Agent::init_agent_variables() → interactive or config-based
     ↓
Agent::update_shared_dynamic_instructions() → run _instructions function
     ↓
Config::use_session() → create session with agent context
```
