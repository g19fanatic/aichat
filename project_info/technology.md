# AIChat - Technology Stack & Dependencies

## Language & Toolchain
- **Language**: Rust (Edition 2021)
- **Build**: Cargo with `release` profile (LTO, strip, opt-level "z")
- **Async Runtime**: Tokio multi-thread (`rt-multi-thread`, `signal`, `time`, `macros`)
- **MSRV**: Not explicitly specified; uses `LazyLock` (Rust 1.80+)

## Core Dependencies

### CLI & User Interaction
| Crate | Purpose |
|-------|---------|
| `clap` (4.x, derive) | CLI argument parsing |
| `reedline` (0.40) | Interactive REPL editor (completions, highlighting, vi/emacs modes) |
| `inquire` (0.7) | Interactive prompts (Select, Confirm, Text, MultiSelect) |
| `crossterm` (0.28) | Terminal manipulation, raw mode, key events |
| `nu-ansi-term` | ANSI color/style output |

### HTTP & Networking
| Crate | Purpose |
|-------|---------|
| `reqwest` (0.12, rustls-tls, socks) | HTTP client for LLM APIs |
| `reqwest-eventsource` (0.6) | Server-Sent Events (SSE) streaming |
| `hyper` (1.0, full) | HTTP server for serve mode |
| `hyper-util` (0.1) | Server auto HTTP/1+2, legacy client |
| `http` (1.1) | HTTP types (Method, StatusCode, etc.) |
| `tokio-graceful` (0.2) | Graceful server shutdown |

### Serialization & Data
| Crate | Purpose |
|-------|---------|
| `serde` + `serde_json` + `serde_yaml` | JSON/YAML serialization (preserve_order for JSON) |
| `json-patch` (4.0) | JSON merge-patch for request patching |
| `jsonic` (0.2) | Fuzzy/lenient JSON parsing for LLM outputs |
| `bincode` (2.0) | Binary serialization for embedded themes/syntaxes |
| `indexmap` (2.2, serde) | Order-preserving maps for tools, variables |

### AI/ML Specific
| Crate | Purpose |
|-------|---------|
| `hnsw_rs` (0.3) | HNSW vector index for RAG vector search |
| `bm25` (2.0, parallelism) | BM25 keyword search for RAG |
| `rayon` (1.10) | Parallel processing for embeddings/indexing |

### Text Processing & Rendering
| Crate | Purpose |
|-------|---------|
| `syntect` (5.0, parsing, regex-onig) | Syntax highlighting for code blocks |
| `textwrap` (0.16) | Text wrapping for terminal output |
| `html_to_markdown` (0.1) | HTML→Markdown conversion for web content |
| `scraper` (0.23, deterministic) | HTML parsing for web crawling |
| `fancy-regex` (0.14) | Regex with lookahead/lookbehind support |
| `unicode-width` (0.2) | CJK-aware string width calculation |
| `unicode-segmentation` (1.11) | Unicode word segmentation for token estimation |

### Cryptography & Auth
| Crate | Purpose |
|-------|---------|
| `sha2` (0.10) | SHA-256 hashing for document dedup |
| `hmac` (0.12) | HMAC-SHA256 for AWS SigV4 (Bedrock) |
| `base64` (0.22) | Base64 encode/decode for images, auth tokens |
| `aws-smithy-eventstream` (0.60) | AWS event stream binary protocol (Bedrock) |

### System & OS Integration
| Crate | Purpose |
|-------|---------|
| `dirs` (6.0) | XDG-compatible config/data directories |
| `os_info` (3.8) | OS distribution detection for system prompts |
| `sys-locale` (0.3) | System locale detection |
| `terminal-colorsaurus` (0.4) | Terminal light/dark theme detection |
| `arboard` (3.3) | Cross-platform clipboard (Wayland on Linux) |
| `which` (8.0) | Executable path resolution |
| `duct` (1.0) | External command execution for `.file` backtick commands |
| `is-terminal` (0.4) | TTY detection |

### Utility
| Crate | Purpose |
|-------|---------|
| `anyhow` (1.0) | Error handling with context |
| `chrono` (0.4) | Date/time formatting |
| `uuid` (1.9, v4) | UUID generation for session files, request IDs |
| `parking_lot` (0.12) | Fast synchronous RwLock for GlobalConfig |
| `bitflags` (2.5) | StateFlags for REPL command visibility |
| `path-absolutize` (3.1) | Path normalization |
| `fuzzy-matcher` (0.3) | Fuzzy string matching for tab completion |
| `shell-words` (1.1) | Shell-like argument parsing |
| `simplelog` (0.12) / `log` (0.4) | Logging |
| `rust-embed` (8.5) | Embed static assets (roles, themes, syntaxes) |
| `urlencoding` (2.1) | URL encoding for API paths |
| `futures-util` (0.3) | Stream combinators |
| `bytes` (1.4) | Byte buffer management |
| `async-trait` (0.1) | Async trait methods |
| `async-recursion` (1.1) | Recursive async functions |

### Platform-Specific
- **macOS**: `crossterm` with `use-dev-tty` feature
- **Linux**: `arboard` with `wayland-data-control`
- **Non-Linux**: `arboard` defaults

### Dev Dependencies
- `pretty_assertions` (1.4) — Better test diffs
- `rand` (0.9) — Random data for tests

## Embedded Assets
- `assets/syntaxes.bin` — Syntax definitions from bat project (~898KB)
- `assets/monokai-extended.theme.bin` — Dark theme
- `assets/monokai-extended-light.theme.bin` — Light theme
- `assets/roles/*.md` — 6 built-in roles
- `assets/playground.html` — LLM Playground (~51KB)
- `assets/arena.html` — LLM Arena (~36KB)
- `models.yaml` — Provider model definitions (~64KB)
