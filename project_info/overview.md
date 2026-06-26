# AIChat - Project Overview

## Identity
- **Name**: AIChat
- **Version**: 0.30.0
- **Author**: sigoden <sigoden@gmail.com>
- **License**: MIT OR Apache-2.0
- **Repository**: https://github.com/sigoden/aichat
- **Language**: Rust (Edition 2021)
- **Description**: All-in-one LLM CLI Tool featuring Shell Assistant, CMD & REPL Mode, RAG, AI Tools & Agents

## Purpose
AIChat is a unified command-line interface for interacting with 20+ LLM providers through multiple interaction modes (CMD, REPL, and Server). It provides rich features including role customization, session management, RAG (Retrieval Augmented Generation), function calling with external tools, AI agents, macros, and a built-in HTTP server with playground and arena UIs.

## Key Capabilities
1. **Multi-Provider Support**: OpenAI, Claude, Gemini, Ollama, Groq, Azure-OpenAI, VertexAI, Bedrock, Cohere, plus 18 OpenAI-compatible providers
2. **Three Operating Modes**: CMD (one-shot), REPL (interactive), Server (HTTP API)
3. **Shell Assistant**: Natural language → shell commands with execute/revise/describe/copy workflow
4. **Role System**: Customizable system prompts with model config, built-in roles, and file-based roles
5. **Session Management**: Persistent conversation context with auto-compression and auto-naming
6. **RAG**: Document ingestion with hybrid search (HNSW vector + BM25 keyword), optional reranking
7. **Function Calling / Tools**: External tool integration via `llm-functions` ecosystem
8. **AI Agents**: Combine Instructions + Tools + RAG into reusable agents
9. **Macros**: Repeatable multi-step REPL command sequences
10. **Built-in Server**: OpenAI-compatible API proxy, LLM Playground, LLM Arena

## Build & Run
```bash
cargo build --release          # Build optimized binary
cargo run -- <args>            # Run in development
cargo install aichat           # Install from crates.io
```

### Test Scripts (via Argcfile.sh)
```bash
argc test-init-config          # Test config initialization
argc test-function-calling     # Test function calling
argc test-clients <clients>    # Test various providers
argc test-server               # Test proxy server
argc chat <provider:model>     # Chat directly with any provider
argc models <provider>         # List models for a provider
```
