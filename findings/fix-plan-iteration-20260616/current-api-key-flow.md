# Current API Key Resolution Flow

## Overview

The `api_key` field for most clients is resolved through the `config_get_fn!` macro (defined at `src/client/macros.rs:226-237`). This macro generates a simple getter method that follows a static resolution chain. There is **no existing mechanism** to resolve an API key via a helper command.

---

## The `config_get_fn!` Macro

**Location**: `src/client/macros.rs:226-237`

```rust
macro_rules! config_get_fn {
    ($field_name:ident, $fn_name:ident) => {
        fn $fn_name(&self) -> anyhow::Result<String> {
            let env_prefix = Self::name(&self.config);
            let env_name =
                format!("{}_{}", env_prefix, stringify!($field_name)).to_ascii_uppercase();
            std::env::var(&env_name)
                .ok()
                .or_else(|| self.config.$field_name.clone())
                .ok_or_else(|| anyhow::anyhow!("Miss '{}'", stringify!($field_name)))
        }
    };
}
```

### Resolution Order
1. **Environment variable**: `{CLIENT_NAME}_{FIELD_NAME}` (uppercased). For example, for a client named "openai" and field "api_key", it checks `OPENAI_API_KEY`.
2. **Config YAML field**: `self.config.$field_name.clone()` — reads the `api_key` field from the deserialized YAML config struct.
3. **Error**: If neither is available, returns `anyhow!("Miss 'api_key'")`.

### Key Characteristics
- **Synchronous**: The generated function is a plain `fn` (not `async`), so it cannot invoke async operations directly.
- **Stateless**: No caching — each call re-checks env var and config value.
- **Returns `Result<String>`**: Callers handle the error or use `.ok()` for optional keys.

---

## Client Config Structs Using `config_get_fn!` for `api_key`

| Client | Config Struct | File:Line | `api_key` Required? | Auth Method |
|--------|--------------|-----------|---------------------|-------------|
| OpenAI | `OpenAIConfig` | `openai.rs:12-22` | Yes (`.get_api_key()?`) | `bearer_auth` |
| OpenAI-Compatible | `OpenAICompatibleConfig` | `openai_compatible.rs:9-18` | Optional (`.get_api_key().ok()`) | `bearer_auth` if present |
| Claude | `ClaudeConfig` | `claude.rs:12-21` | Yes (`.get_api_key()?`) | `x-api-key` header |
| Gemini | `GeminiConfig` | `gemini.rs:11-20` | Yes (`.get_api_key()?`) | `x-goog-api-key` header |
| Cohere | `CohereConfig` | `cohere.rs:12-21` | Yes (`.get_api_key()?`) | `bearer_auth` |
| Azure OpenAI | `AzureOpenAIConfig` | `azure_openai.rs:7-16` | Yes (`.get_api_key()?`) | `api-key` header |
| Bedrock | `BedrockConfig` | `bedrock.rs:17-28` | N/A (uses `access_key_id`/`secret_access_key`) | AWS SigV4 signing |
| VertexAI | `VertexAIConfig` | `vertexai.rs:13-23` | N/A (uses `project_id`/`location` + OAuth) | `bearer_auth` (from access_token cache) |

---

## Where `get_api_key()` Is Called (the Interception Points)

Each client's `prepare_chat_completions` / `prepare_embeddings` / `prepare_rerank` functions call `get_api_key()` at the start of building the request. These are the per-request intercept points:

| Client | Call Site | File:Line |
|--------|-----------|-----------|
| OpenAI | `prepare_chat_completions` | `openai.rs:46` |
| OpenAI | `prepare_embeddings` | `openai.rs:66` |
| OpenAI-Compatible | `prepare_chat_completions` | `openai_compatible.rs:42` |
| OpenAI-Compatible | `prepare_embeddings` | `openai_compatible.rs:62` |
| OpenAI-Compatible | `prepare_rerank` | `openai_compatible.rs:79` |
| Claude | `prepare_chat_completions` | `claude.rs:45` |
| Gemini | `prepare_chat_completions` | `gemini.rs:44` |
| Gemini | `prepare_embeddings` | `gemini.rs:71` |
| Cohere | `prepare_chat_completions` | `cohere.rs:45` |
| Cohere | `prepare_embeddings` | `cohere.rs:66` |
| Cohere | `prepare_rerank` | `cohere.rs:93` |
| Azure OpenAI | `prepare_chat_completions` | `azure_openai.rs:48` |
| Azure OpenAI | `prepare_embeddings` | `azure_openai.rs:67` |

---

## How Auth Gets Applied to the HTTP Request

After obtaining the key, each client constructs a `RequestData` struct (`common.rs:259-263`) and attaches the key:

```rust
pub struct RequestData {
    pub url: String,
    pub headers: IndexMap<String, String>,
    pub body: Value,
}
```

Auth is applied via:
- `request_data.bearer_auth(api_key)` → sets `authorization: Bearer {api_key}` header
- `request_data.header("x-api-key", api_key)` → Claude-style header
- `request_data.header("api-key", api_key)` → Azure-style header
- `request_data.header("x-goog-api-key", api_key)` → Gemini-style header

Then `request_data.into_builder(client)` (`common.rs:294+`) builds the final `reqwest::RequestBuilder`.

---

## Possible Interception Points for `api_key_command`

### Option A: Modify `config_get_fn!` Macro

Add a third resolution step to the macro chain:
1. Env var → 2. Config YAML field → **3. Execute helper command** → 4. Error

**Pros**: All clients automatically get the feature with zero per-client changes.
**Cons**: The macro generates a sync `fn`, so running a subprocess here is possible (sync `Command::output()`), but it would block the thread. Caching would need to be external (like `access_token.rs`).

### Option B: New `config_get_fn_with_command!` Macro or Enhanced Getter

Create a new getter pattern that checks for `api_key_command` field in the config struct:
1. Check `access_token.rs` cache for valid cached token
2. If cached: return cached value
3. If not: env var → config YAML → **run `api_key_command` subprocess** → cache result → return
4. Error if all fail

**Pros**: Keeps the existing macro untouched for simple fields (like `api_base`). Only `api_key` needs the enhanced flow.
**Cons**: Requires per-client config struct changes to add the `api_key_command` field.

### Option C: At the `prepare_*` Function Level

Instead of modifying the getter, intercept at the `prepare_chat_completions` level — before calling `get_api_key()`, check if a command-based refresh is needed.

**Pros**: Very targeted, follows the VertexAI/Bedrock pattern of custom `Client` trait impls.
**Cons**: Would require touching every client that wants the feature, or adding pre-request hooks.

---

## Recommended Interception Point

**Option B is the strongest candidate** because:
1. The `api_key_command` field can be added to each Config struct (or a shared trait/base)
2. The `config_get_fn!` macro can be extended or a new variant created that:
   - First checks env / YAML as before
   - If that fails, checks for `self.config.api_key_command`
   - Runs the command (blocking subprocess is acceptable — it's a quick CLI call)
   - Caches via `access_token.rs` mechanism
3. No changes needed in the `prepare_*` functions — they continue calling `self.get_api_key()?`

The key insight is that `config_get_fn!` is synchronous, and `std::process::Command` is also synchronous — so a subprocess call fits naturally inside the existing macro expansion. Caching via `access_token.rs` (which uses a global `RwLock<IndexMap>`) allows the expensive subprocess to run only when the cached token expires.

---

## Summary of Files Involved

| File | Role |
|------|------|
| `src/client/macros.rs:226-237` | `config_get_fn!` macro definition |
| `src/client/access_token.rs:1-32` | Token caching infrastructure (get/set/is_valid with expiry) |
| `src/client/openai.rs:12-22, 25` | OpenAIConfig struct + `config_get_fn!(api_key, get_api_key)` |
| `src/client/openai_compatible.rs:9-18, 22` | OpenAICompatibleConfig struct + getter |
| `src/client/claude.rs:12-21, 24` | ClaudeConfig struct + getter |
| `src/client/gemini.rs:11-20, 23` | GeminiConfig struct + getter |
| `src/client/cohere.rs:12-21, 24` | CohereConfig struct + getter |
| `src/client/azure_openai.rs:7-16, 20` | AzureOpenAIConfig struct + getter |
| `src/client/common.rs:259-300` | `RequestData` struct and `bearer_auth`/`header`/`into_builder` |
| `src/client/vertexai.rs:421-480` | Existing token refresh pattern (OAuth2 via HTTP, async) |
| `src/client/bedrock.rs:200-239` | Existing credential refresh pattern (CLI subprocess, sync) |
| `config.example.yaml:81-240` | Client configuration examples in YAML |
