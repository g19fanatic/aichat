# AIChat - Entry Points & Critical Code Locations

## Application Entry & Routing

| Location | Description |
|----------|-------------|
| `src/main.rs:37-55` | `main()` — Tokio async entry, parses CLI, determines WorkingMode, creates GlobalConfig |
| `src/main.rs:57-152` | `run()` — Main routing: processes CLI flags, sets up role/session/agent/rag, dispatches to mode |
| `src/main.rs:154-183` | `start_directive()` — CMD mode recursive function: LLM call → tool calls → recurse |
| `src/main.rs:185-188` | `start_interactive()` — REPL mode entry, creates and runs `Repl` |
| `src/main.rs:190-251` | `shell_execute()` — Shell assistant: LLM → shell command → e/r/d/c/q menu |
| `src/main.rs:253-268` | `create_input()` — Factory: creates Input from text and/or files |

## Configuration Initialization

| Location | Description |
|----------|-------------|
| `src/config/mod.rs:165-195` | `Config::init()` — Load config file, env vars, setup model/functions/loaders |
| `src/config/mod.rs:196-220` | `Config::config_dir()` → `Config::agent_functions_dir()` — XDG path resolution chain |
| `src/config/mod.rs:1680-1740` | `Config::load_from_file()` — YAML deserialization with enhanced error reporting |
| `src/config/mod.rs:1740-1770` | `Config::load_dynamic()` — Config from env var (no file needed) |
| `src/config/mod.rs:1770-1880` | `Config::load_envs()` — Override all config from `AICHAT_*` environment variables |

## Client Initialization

| Location | Description |
|----------|-------------|
| `src/client/mod.rs:15-45` | `register_client!(...)` — Macro invocation registering 8 client types |
| `src/client/macros.rs:1-100` | `register_client!` macro — Generates `init_client()`, `list_models()`, `ClientConfig` enum |
| `src/client/common.rs:42-157` | `Client` trait — Core async trait with default implementations |
| `src/client/common.rs:370-425` | `call_chat_completions()` — Non-streaming completion with spinner |
| `src/client/common.rs:427-465` | `call_chat_completions_streaming()` — Streaming completion with channel-based rendering |

## Agent System

| Location | Description |
|----------|-------------|
| `src/config/agent.rs:34-108` | `Agent::init()` — Load agent from index.yaml, functions.json, optional RAG |
| `src/config/agent.rs:110-165` | `Agent::init_agent_variables()` — Interactive variable initialization |
| `src/config/agent.rs:197-205` | `Agent::interpolated_instructions()` — `{{var}}` substitution in instructions |
| `src/config/agent.rs:244-260` | `Agent::run_instructions_fn()` — Dynamic instructions via `_instructions` function |
| `src/config/mod.rs:1075-1100` | `Config::use_agent()` — Full agent initialization with optional session |

## Session System

| Location | Description |
|----------|-------------|
| `src/config/session.rs:50-60` | `Session::new()` — Create empty session from config |
| `src/config/session.rs:62-95` | `Session::load()` — Deserialize session from YAML file |
| `src/config/session.rs:375-425` | `Session::add_message()` — Handle normal, continue, and regenerate message flows |
| `src/config/session.rs:430-470` | `Session::build_messages()` — Construct message chain including compressed history |
| `src/config/mod.rs:835-900` | `Config::use_session()` — Start or join session with last-message incorporation |
| `src/config/mod.rs:930-960` | `Config::compress_session()` — LLM-driven session summarization |

## Role System

| Location | Description |
|----------|-------------|
| `src/config/role.rs:65-100` | `Role::new()` — Parse role from markdown with YAML frontmatter |
| `src/config/role.rs:190-230` | `Role::build_messages()` — Convert role+input to Message vec with system/user/assistant |
| `src/config/role.rs:27-36` | `RoleLike` trait — Interface for Role/Session/Agent polymorphism |

## Input Processing

| Location | Description |
|----------|-------------|
| `src/config/input.rs:30-50` | `Input::from_str()` — Simple text input creation |
| `src/config/input.rs:52-120` | `Input::from_files()` — Load from local files, URLs, external commands |
| `src/config/input.rs:200-212` | `Input::use_embeddings()` — RAG-enhanced input via vector search |
| `src/config/input.rs:230-247` | `Input::prepare_completion_data()` — Build ChatCompletionsData for API |

## Function Calling / Tools

| Location | Description |
|----------|-------------|
| `src/function.rs:20-40` | `eval_tool_calls()` — Execute tool calls with dedup and loop detection |
| `src/function.rs:120-175` | `ToolCall::eval()` — Resolve call config, run external function |
| `src/function.rs:210-285` | `run_llm_function()` — Spawn external command, capture output via `LLM_OUTPUT` |
| `src/config/mod.rs:1260-1320` | `Config::select_functions()` — Resolve available tools for current role/agent |

## RAG System

| Location | Description |
|----------|-------------|
| `src/rag/mod.rs:55-115` | `Rag::init()` — Interactive RAG setup: select model, chunk size, add docs |
| `src/rag/mod.rs:130-145` | `Rag::load()` — Deserialize RAG from YAML |
| `src/rag/mod.rs:200-240` | `Rag::search()` — Hybrid search entry point |
| `src/rag/mod.rs:290-370` | `Rag::hybird_search()` — Vector + BM25 + optional reranking |
| `src/rag/mod.rs:370-400` | `Rag::vector_search()` — HNSW cosine similarity search |
| `src/rag/mod.rs:400-420` | `Rag::keyword_search()` — BM25 keyword search |

## Rendering

| Location | Description |
|----------|-------------|
| `src/render/mod.rs:12-24` | `render_stream()` — Route to markdown or raw renderer |
| `src/render/stream.rs:20-50` | `markdown_stream()` — Real-time token rendering with raw terminal mode |
| `src/render/stream.rs:52-80` | `raw_stream()` — Simple piped output renderer |
| `src/render/markdown.rs:40-70` | `MarkdownRender::init()` — Setup syntect themes and syntax sets |

## HTTP Server

| Location | Description |
|----------|-------------|
| `src/serve.rs:40-60` | `run()` — Start hyper HTTP server with graceful shutdown |
| `src/serve.rs:140-175` | `Server::handle()` — Request router for all endpoints |
| `src/serve.rs:200-400` | `Server::chat_completions()` — OpenAI-compatible chat API (stream + non-stream) |

## REPL

| Location | Description |
|----------|-------------|
| `src/repl/mod.rs:105-150` | `Repl::init()` — Create reedline editor with completions/highlighting |
| `src/repl/mod.rs:152-200` | `Repl::run()` — Main REPL loop |
| `src/repl/mod.rs:260-560` | `run_repl_command()` — Command dispatcher for all 36 dot-commands |
| `src/repl/mod.rs:580-615` | `ask()` — Recursive chat function with tool call loop |

## Key Utility Functions

| Location | Description |
|----------|-------------|
| `src/utils/mod.rs:68-80` | `estimate_token_length()` — Heuristic token counting |
| `src/utils/mod.rs:82-84` | `strip_think_tag()` — Remove `<think>...</think>` from responses |
| `src/utils/command.rs:25-65` | `detect_shell()` — Auto-detect user's shell |
| `src/utils/command.rs:75-85` | `run_command()` — Execute external command |
| `src/utils/variables.rs:5-30` | `interpolate_variables()` — System variable substitution `{{__os__}}` etc. |
| `src/utils/request.rs:40-55` | `fetch()` — Simple HTTP GET |
| `src/utils/request.rs:60-150` | `fetch_with_loaders()` — Content-type aware URL fetching |
| `src/utils/loader.rs:20-40` | `load_recursive_url()` — Recursive website crawling for RAG |
| `src/utils/path.rs:20-40` | `expand_glob_paths()` — Glob pattern resolution for file inputs |
