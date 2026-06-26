# Design: `api_key_command` Feature

## Overview

A generic mechanism allowing any client to resolve its `api_key` by executing a shell command, with cached results and configurable expiry. This enables users with rotating/expiring bearer tokens to have aichat automatically refresh credentials.

## 1. YAML Schema

### Field Name & Location

```yaml
clients:
  - type: openai-compatible
    name: staging
    api_base: https://staging.internal.example.com/v1
    api_key_command: "vault read -field=token secret/staging-api-key"  # NEW FIELD
    api_key_command_expires_in: 3600                                   # NEW FIELD (optional, seconds)
    models:
      - name: gpt-4o
        max_input_tokens: 128000
```

**Field definitions:**

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `api_key_command` | `Option<String>` | No | `None` | Shell command whose stdout becomes the `api_key` value |
| `api_key_command_expires_in` | `Option<u64>` | No | `None` (never expires within session) | Seconds until the cached command output is considered stale |

### Placement in Config Structs

Added to every `*Config` struct that has an `api_key` field:
- `OpenAIConfig`
- `OpenAICompatibleConfig`
- `ClaudeConfig`
- `CohereConfig`
- `GeminiConfig`
- `AzureOpenAIConfig`

**NOT** added to `VertexAIConfig` or `BedrockConfig` (they have their own specialized credential refresh mechanisms).

### Priority / Resolution Order

The `api_key` resolution follows this priority (highest first):

1. **Environment variable**: `{CLIENT_NAME}_API_KEY` (existing behavior via `config_get_fn!`)
2. **Config YAML `api_key` field**: Static string in config (existing behavior)
3. **`api_key_command`**: Execute command and use stdout (NEW)
4. Error: "Miss 'api_key'"

This means: if either env var OR `api_key` is set, the command is never executed. The command is a fallback for when no static key is available.

### Alternative Considered: Override Priority

An alternative design where `api_key_command` *always* takes precedence (even over env/config) was considered but rejected because:
- It violates the existing "env var overrides everything" contract
- Users who set env vars expect them to win
- Users who want command-only can simply omit `api_key` from config

## 2. Caching Strategy

### Reuse `access_token.rs`

The existing `access_token.rs` module (`src/client/access_token.rs`) provides exactly the right primitives:

```rust
// Existing API:
pub fn get_access_token(client_name: &str) -> Result<String>
pub fn is_valid_access_token(client_name: &str) -> bool
pub fn set_access_token(client_name: &str, token: String, expires_at: i64)
```

It uses a global `LazyLock<RwLock<IndexMap<String, (String, i64)>>>` keyed by `client_name`, storing `(token, expires_at_timestamp)`.

**Design decision**: Reuse this exact module for `api_key_command` caching. The VertexAI client already uses it for OAuth tokens; `api_key_command` tokens are semantically identical (a cached credential with optional expiry).

### Cache Key

Use the client's `name` (e.g., `"staging"`, `"openai"`) as the cache key — same pattern as VertexAI.

### Expiry Behavior

| `api_key_command_expires_in` | Behavior |
|------------------------------|----------|
| `Some(N)` | Token expires N seconds after command execution |
| `None` | Token expires at `i64::MAX` (effectively never within a session) |

On each request:
1. Check `is_valid_access_token(client_name)` 
2. If valid → return cached token via `get_access_token(client_name)`
3. If invalid/missing → execute command → cache result with `set_access_token(client_name, token, expires_at)`

### Thread Safety

`access_token.rs` uses `parking_lot::RwLock` — already safe for concurrent access from multiple async tasks. However, there's a TOCTOU race: two concurrent requests could both see the cache as invalid and both run the command. This is acceptable because:
- The command is idempotent (user's responsibility)
- The last writer wins (both get a valid token)
- VertexAI has the same race condition today and it's fine

## 3. Command Execution Approach

### Synchronous Subprocess (Recommended)

Use **`std::process::Command`** (synchronous), matching the existing Bedrock pattern in `fetch_bedrock_creds_from_cli()` (line 206 of `bedrock.rs`).

**Rationale:**
- Bedrock already uses synchronous `std::process::Command` successfully
- The command runs on a Tokio blocking thread via `tokio::task::spawn_blocking` (or is called from within the sync `prepare_*` functions which are already in a blocking-safe context since they're called within async functions)
- Keeps implementation simple and consistent
- The crate does NOT use `tokio::process::Command` anywhere today

**Implementation sketch:**

```rust
use std::process::Command;

fn run_api_key_command(command_str: &str) -> Result<String> {
    let shell = &*SHELL;  // Reuse existing SHELL detection from utils/command.rs
    let output = Command::new(&shell.cmd)
        .arg(&shell.arg)
        .arg(command_str)
        .output()
        .with_context(|| format!("Failed to execute api_key_command: {command_str}"))?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("api_key_command failed (exit {}): {}", 
              output.status.code().unwrap_or(-1), stderr.trim());
    }
    
    let token = String::from_utf8(output.stdout)
        .context("api_key_command output is not valid UTF-8")?
        .trim()
        .to_string();
    
    if token.is_empty() {
        bail!("api_key_command returned empty output");
    }
    
    Ok(token)
}
```

### Why Not Async?

- `prepare_chat_completions` (and similar `prepare_*` functions) are synchronous functions called from within async trait methods
- VertexAI can use async because it manually implements the `Client` trait and calls its refresh inside the async `chat_completions_inner` method
- For macro-generated clients (`impl_client_trait!`), the `prepare_*` functions are sync
- Wrapping in `tokio::task::spawn_blocking` would require the `prepare_*` functions to become async, which is a much larger refactor

**Chosen approach**: Run the command synchronously. It's acceptable because:
- Token commands are typically fast (< 1s)
- They run infrequently (only on cache miss)
- Bedrock does the same thing today with `aws configure export-credentials`

### Shell Usage

Reuse the existing `SHELL` detection from `src/utils/command.rs:17`:
```rust
pub static SHELL: LazyLock<Shell> = LazyLock::new(detect_shell);
```

This detects the user's shell (bash, zsh, powershell, etc.) and provides the correct `-c` argument. The `api_key_command` string is passed as a single shell argument, allowing pipes, env vars, etc.

## 4. Integration with `config_get_fn!` Macro

### Option A: Modify the Macro (Rejected)

Modifying `config_get_fn!` to handle `api_key_command` would:
- Complicate the macro for all fields (not just `api_key`)
- Require passing extra context (client name, expiry config)
- Break the elegant simplicity of the current macro

### Option B: New Helper Function (Recommended)

Create a new function that wraps the macro-generated getter with command fallback:

```rust
/// Resolves api_key with command fallback and caching.
/// Called instead of raw get_api_key() in prepare_* functions.
fn resolve_api_key(
    client_name: &str,
    get_api_key_result: Result<String>,
    api_key_command: Option<&str>,
    expires_in: Option<u64>,
) -> Result<String> {
    // 1. If static resolution succeeded (env var or config), use it
    if let Ok(key) = get_api_key_result {
        return Ok(key);
    }
    
    // 2. If no command configured, return the original error
    let command = match api_key_command {
        Some(cmd) => cmd,
        None => return get_api_key_result,
    };
    
    // 3. Check cache
    if is_valid_access_token(client_name) {
        return get_access_token(client_name);
    }
    
    // 4. Execute command
    let token = run_api_key_command(command)?;
    
    // 5. Cache with expiry
    let expires_at = match expires_in {
        Some(secs) => Utc::now().timestamp() + secs as i64,
        None => i64::MAX,
    };
    set_access_token(client_name, token.clone(), expires_at);
    
    Ok(token)
}
```

### Integration Point

In each client's `prepare_chat_completions` (and `prepare_embeddings`, `prepare_rerank`):

```rust
// BEFORE (openai.rs):
let api_key = self_.get_api_key()?;

// AFTER:
let api_key = resolve_api_key(
    Self::name(&self_.config),
    self_.get_api_key(),
    self_.config.api_key_command.as_deref(),
    self_.config.api_key_command_expires_in,
)?;
```

This is a per-call-site change but keeps the macro untouched and is explicit about what's happening.

### Macro-Level Alternative (Future)

A future refactor could introduce a `config_get_fn_with_command!` macro variant, but the explicit approach is clearer and lower-risk for initial implementation.

## 5. Error Handling

### Command Failures

| Failure Mode | Behavior |
|-------------|----------|
| Command not found | Error: "Failed to execute api_key_command: {cmd}" |
| Non-zero exit | Error: "api_key_command failed (exit N): {stderr}" |
| Empty stdout | Error: "api_key_command returned empty output" |
| Non-UTF-8 output | Error: "api_key_command output is not valid UTF-8" |
| Timeout | Not implemented (OS-level); user should add timeout to their command |

### Error Propagation

Errors from `resolve_api_key` propagate the same way as errors from `get_api_key()` today — they bubble up through `prepare_chat_completions` → `chat_completions_inner` → the retry loop in `chat_completions`. The existing retry logic (3 retries with exponential backoff) will re-attempt, which means:
- If the command fails transiently, retries may succeed
- If the command fails permanently, all 3 retries fail and the user sees the error

### Cache Invalidation on Error

If a cached token leads to an HTTP 401/403 from the API, the existing retry mechanism will call `prepare_chat_completions` again. At that point, the token is still cached and valid (time-wise). **This is a known limitation** — the cache expiry is purely time-based, not error-based.

**Future enhancement**: Invalidate the cache on 401/403 responses (not in initial implementation to keep scope small).

## 6. Security Considerations

- The command runs with the same privileges as the aichat process
- Command output (the token) is stored in memory only (not written to disk)
- The command string itself is stored in `config.yaml` (already user-controlled)
- No sandboxing — user is responsible for command safety (same as Bedrock's `aws` CLI call)

## 7. Environment Variable Support

Following the existing `config_get_fn!` pattern, the new fields can also be set via environment variables:

- `{CLIENT_NAME}_API_KEY_COMMAND` — overrides `api_key_command` from config
- `{CLIENT_NAME}_API_KEY_COMMAND_EXPIRES_IN` — overrides expiry from config

This follows naturally from adding these as config struct fields that could be consumed by env var patterns. However, the initial implementation may omit env var support for these fields (only `api_key` itself uses `config_get_fn!`; the command/expiry fields are read directly from the config struct).

## 8. Summary of Changes Required

### New Code
1. `src/client/common.rs` or new file `src/client/api_key_command.rs`:
   - `run_api_key_command(command: &str) -> Result<String>`
   - `resolve_api_key(client_name, get_api_key_result, command, expires_in) -> Result<String>`

### Modified Config Structs (add 2 fields each)
2. `src/client/openai.rs` — `OpenAIConfig`
3. `src/client/openai_compatible.rs` — `OpenAICompatibleConfig`
4. `src/client/claude.rs` — `ClaudeConfig`
5. `src/client/cohere.rs` — `CohereConfig`
6. `src/client/gemini.rs` — `GeminiConfig`
7. `src/client/azure_openai.rs` — `AzureOpenAIConfig`

### Modified `prepare_*` Functions (replace `get_api_key()` calls)
8. All `prepare_chat_completions`, `prepare_embeddings`, `prepare_rerank` in the above clients

### Documentation
9. `config.example.yaml` — add commented example

### No Changes Needed
- `src/client/macros.rs` — untouched
- `src/client/access_token.rs` — reused as-is
- `src/client/vertexai.rs` — has its own mechanism
- `src/client/bedrock.rs` — has its own mechanism

## 9. User-Facing Behavior Summary

```yaml
# config.yaml example
clients:
  - type: openai-compatible
    name: staging
    api_base: https://staging.internal.example.com/v1
    api_key_command: "/usr/local/bin/get-staging-token"
    api_key_command_expires_in: 1800  # refresh every 30 min
    models:
      - name: gpt-4o
        max_input_tokens: 128000
```

**What happens:**
1. User starts aichat and selects the "staging" model
2. First request: no env var, no static `api_key` → executes `/usr/local/bin/get-staging-token`
3. Command stdout (trimmed) becomes the bearer token
4. Token cached for 1800 seconds
5. Subsequent requests within 30 minutes reuse cached token
6. After 30 minutes, next request re-executes the command
7. If command fails, user sees error message and request fails (with retries)
