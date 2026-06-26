# Agent Context: `api_key_command` Feature

## Key Files

- `src/client/access_token.rs` — Token cache with expiry: `get_access_token`, `is_valid_access_token`, `set_access_token`. Uses `IndexMap<String, (String, i64)>` keyed by client name. Only 32 lines.
- `src/utils/command.rs:17-23` — `SHELL` static (LazyLock<Shell>) with `cmd`, `arg` fields for subprocess execution. Used by Bedrock's credential fetch.
- `src/client/macros.rs:155-156` — `client_common_fns!()` generates `fn name(&self) -> &str` delegating to `Self::name(&self.config)`. Confirms `self_.name()` is available in free functions.
- `src/client/macros.rs:226-236` — `config_get_fn!` macro: checks env var `{CLIENT_NAME}_{field}` then config YAML field. NOT being modified.
- `src/client/macros.rs:1-50` — `register_client!` macro: generates module declarations, `ClientConfig` enum, and `$client` structs with `config: $config` field.
- `src/client/openai.rs:12-22` — `OpenAIConfig` struct with `api_key: Option<String>` field.
- `src/client/openai.rs:47` — First call site: `let api_key = self_.get_api_key()?;` in `prepare_chat_completions`.
- `src/client/openai.rs:67` — Second call site: `let api_key = self_.get_api_key()?;` in `prepare_embeddings`.
- `src/client/openai_compatible.rs:9-18` — `OpenAICompatibleConfig` struct. Note: uses `.ok()` on get_api_key (optional auth).
- `src/client/openai_compatible.rs:42,63,80` — Three call sites using `self_.get_api_key().ok()`.
- `src/client/openai_compatible.rs:82` — Confirms `self_.name()` works in free functions (ernie check).
- `src/client/claude.rs:12-21` — `ClaudeConfig` struct.
- `src/client/claude.rs:46` — Call site: `let api_key = self_.get_api_key()?;`.
- `src/client/gemini.rs:11-20` — `GeminiConfig` struct.
- `src/client/gemini.rs:44,71` — Two call sites in `prepare_chat_completions` and `prepare_embeddings`.
- `src/client/cohere.rs:12-21` — `CohereConfig` struct.
- `src/client/cohere.rs:45,66,93` — Three call sites in chat_completions, embeddings, and rerank.
- `src/client/azure_openai.rs:7-16` — `AzureOpenAIConfig` struct.
- `src/client/azure_openai.rs:48,68` — Two call sites in chat_completions and embeddings.
- `src/client/mod.rs:1-14` — Module declarations and re-exports. Add `mod api_key_command;` after line 1 and `pub use` after line 13.
- `src/client/bedrock.rs:200-230` — `fetch_bedrock_creds_from_cli()` — reference pattern for sync subprocess execution.
- `config.example.yaml:95-130` — openai-compatible examples section (insert new example after this).

## Build Commands

```bash
cd /home/pdibiase/sources/aichat && cargo build 2>&1 | head -50
```

## Test Commands

```bash
cd /home/pdibiase/sources/aichat && cargo test 2>&1 | tail -30
```

## Architecture Notes

- All client modules already do `use super::*;` so anything `pub` in `mod.rs` re-exports is automatically available.
- `Option<String>` fields in serde structs deserialize to `None` when absent in YAML — no default annotation needed.
- The `prepare_*` functions are synchronous (called from async via `impl_client_trait!` macro expansion), so `std::process::Command` is appropriate (same as Bedrock).
- VertexAI and Bedrock have their own specialized credential flows — do NOT add `api_key_command` to them.
- The `access_token.rs` cache uses `parking_lot::RwLock` — thread-safe for concurrent reads.

## Implementation Reference (resolve_api_key logic)

```
1. If get_api_key() succeeds (env var or static config) → return it (no command needed)
2. If no api_key_command configured → return original error ("Miss 'api_key'")
3. If cache valid (is_valid_access_token) → return cached value
4. Execute command via SHELL → get token stdout (trimmed)
5. Cache token with expiry (set_access_token)
6. Return token
```
