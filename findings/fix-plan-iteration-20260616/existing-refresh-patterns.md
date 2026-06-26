# Existing Token Refresh Patterns in aichat

## Overview

Two clients in aichat already implement per-request credential refresh: **VertexAI** (OAuth2 token) and **Bedrock** (AWS CLI credentials). Both bypass the standard `config_get_fn!` macro for their credential fields, using custom logic instead. This document analyzes both patterns and how they generalize.

---

## 1. The `access_token.rs` Caching Layer

**File**: `src/client/access_token.rs` (34 lines)

This is a global, thread-safe, in-memory token cache used by VertexAI. It stores tokens keyed by client name with an expiry timestamp.

### Data Structure
```rust
static ACCESS_TOKENS: LazyLock<RwLock<IndexMap<String, (String, i64)>>> =
    LazyLock::new(|| RwLock::new(IndexMap::new()));
```

Each entry is: `client_name → (token_string, expires_at_unix_timestamp)`

### API (3 functions)

| Function | Signature | Purpose |
|----------|-----------|---------|
| `get_access_token` | `(client_name: &str) -> Result<String>` | Retrieves cached token; errors if not present |
| `is_valid_access_token` | `(client_name: &str) -> bool` | Returns true iff token exists AND `now < expires_at` |
| `set_access_token` | `(client_name: &str, token: String, expires_at: i64)` | Upserts token + expiry |

### Concurrency Model
- Uses `parking_lot::RwLock` — multiple readers, exclusive writers
- Read lock for `get_access_token` and `is_valid_access_token`
- Write lock for `set_access_token`
- Safe for concurrent async tasks (parking_lot doesn't poison on panic)

### Key Characteristics
- **No automatic refresh**: Callers must check validity and refresh manually
- **Client-name-keyed**: Supports multiple instances of the same provider type (e.g., two VertexAI configs)
- **Simple expiry**: Just a Unix timestamp comparison, no refresh-ahead or jitter
- **In-memory only**: Tokens are lost on process restart (re-fetched on next request)

---

## 2. VertexAI Token Refresh Pattern

**File**: `src/client/vertexai.rs`

### Trigger Points
The refresh is called at the START of every API method, before building the request:

```rust
// Line 44 (chat_completions_inner)
prepare_gcloud_access_token(client, self.name(), &self.config.adc_file).await?;

// Line 62 (chat_completions_streaming_inner)
prepare_gcloud_access_token(client, self.name(), &self.config.adc_file).await?;

// Line 85 (embeddings_inner)
prepare_gcloud_access_token(client, self.name(), &self.config.adc_file).await?;
```

### The Refresh Function (line 450)
```rust
pub async fn prepare_gcloud_access_token(
    client: &reqwest::Client,
    client_name: &str,
    adc_file: &Option<String>,
) -> Result<()> {
    if !is_valid_access_token(client_name) {
        let (token, expires_in) = fetch_access_token(client, adc_file)
            .await
            .with_context(|| "Failed to fetch access token")?;
        let expires_at = Utc::now()
            + Duration::try_seconds(expires_in)
                .ok_or_else(|| anyhow!("Failed to parse expires_in of access_token"))?;
        set_access_token(client_name, token, expires_at.timestamp())
    }
    Ok(())
}
```

### Flow
1. **Check cache**: `is_valid_access_token(client_name)` — returns true if token exists and not expired
2. **If invalid/missing**: Call `fetch_access_token()` (async HTTP to Google OAuth2)
3. **Store**: `set_access_token(name, token, expires_at)` — cache for future requests
4. **Use**: Later, `get_access_token(name)` retrieves it for `request_data.bearer_auth(access_token)`

### Token Acquisition (`fetch_access_token`)
- Reads Application Default Credentials JSON file (`~/.config/gcloud/application_default_credentials.json`)
- Performs HTTP POST to `https://oauth2.googleapis.com/token` with refresh_token grant
- Returns `(access_token, expires_in)` where `expires_in` is seconds until expiry
- **Async**: Uses the `reqwest::Client` already available (the HTTP client for API calls)

### Token Usage in Request Building
The token is NOT passed through `config_get_fn!`. Instead:
```rust
// In prepare_chat_completions() (line 93):
let access_token = get_access_token(self_.name())?;
// ...build URL, body...
request_data.bearer_auth(access_token);  // line 150
```

### Key Design Decisions
- **Lazy refresh**: Only refreshes when token is expired/missing, not proactively
- **Per-request check**: Every API call checks; cheap when token is valid (just timestamp comparison)
- **Error propagation**: If refresh fails, the entire API call fails with context
- **No retry on refresh failure**: Single attempt to get a new token
- **Client-scoped**: Uses `self.name()` as cache key — different VertexAI configs get different tokens
- **Manual Client trait impl**: VertexAI does NOT use `impl_client_trait!` macro because it needs the custom pre-request hook

---

## 3. Bedrock Credential Refresh Pattern

**File**: `src/client/bedrock.rs`

### Trigger Points
Credential fetch happens synchronously at the start of request building:

```rust
// In chat_completions_builder() (line 70):
let (access_key_id, secret_access_key, session_token) =
    fetch_bedrock_creds_from_cli(config_profile).unwrap_or_else(|| (
        self.get_access_key_id().unwrap_or_default(),
        self.get_secret_access_key().unwrap_or_default(),
        self.get_session_token().ok(),
    ));

// Same pattern in embeddings_builder() (line 144)
```

### The Refresh Function (`fetch_bedrock_creds_from_cli`, line 200)
```rust
fn fetch_bedrock_creds_from_cli(config_profile: Option<&str>) -> Option<(String, String, Option<String>)> {
    let profile = std::env::var("BEDROCK_AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_PROFILE"))
        .ok()
        .or_else(|| config_profile.map(|s| s.to_string()))?;

    let output = std::process::Command::new("aws")
        .args(["configure", "export-credentials", "--profile", &profile, "--format", "env-no-export"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Parse KEY=VALUE output
    let text = String::from_utf8_lossy(&output.stdout);
    let mut akid = None::<String>;
    let mut sak = None::<String>;
    let mut st = None::<String>;

    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "AWS_ACCESS_KEY_ID"     => akid = Some(v.trim().to_string()),
                "AWS_SECRET_ACCESS_KEY" => sak  = Some(v.trim().to_string()),
                "AWS_SESSION_TOKEN"     => st   = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }

    Some((akid?, sak?, st))
}
```

### Flow
1. **Determine profile**: `BEDROCK_AWS_PROFILE` env → `AWS_PROFILE` env → `config.profile` YAML field
2. **Run CLI**: `aws configure export-credentials --profile <profile> --format env-no-export`
3. **Parse output**: Extract `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`
4. **Fallback**: If CLI fails (command not found, bad profile, etc.), fall back to `config_get_fn!` values
5. **Use directly**: Credentials are used immediately in the same request builder (no caching)

### Key Design Decisions
- **Synchronous**: Uses `std::process::Command` (blocking) — runs on every request
- **No caching**: Fresh CLI invocation per API call (AWS CLI handles its own credential caching internally)
- **Graceful fallback**: `unwrap_or_else` falls back to static config if CLI fails
- **No expiry tracking**: Doesn't know when credentials expire; relies on AWS CLI to handle rotation
- **Optional/soft**: If no profile env var and no config profile, returns `None` immediately (falls back to static)
- **Manual Client trait impl**: Bedrock manually implements `Client` trait (like VertexAI) for custom pre-request logic

---

## 4. Comparison: VertexAI vs Bedrock Patterns

| Aspect | VertexAI | Bedrock |
|--------|----------|---------|
| **Execution model** | Async (HTTP POST to OAuth2 endpoint) | Sync (subprocess: `aws` CLI) |
| **Caching** | Yes — `access_token.rs` with expiry timestamp | No — fresh invocation per request |
| **Expiry awareness** | Yes — checks `expires_at` before refresh | No — relies on external tooling |
| **Fallback on failure** | Hard error — request fails | Soft — falls back to static config |
| **Credential source** | JSON file (ADC) + Google OAuth2 endpoint | External CLI binary (`aws`) |
| **Concurrency safety** | RwLock-based global cache | Per-call subprocess (inherently safe) |
| **Integration point** | Before `prepare_chat_completions()` | Inside `chat_completions_builder()` |
| **Client trait** | Manual `impl Client` | Manual `impl Client` |
| **Works with `impl_client_trait!`?** | No — needs custom pre-request hook | No — needs custom credential injection |

---

## 5. How This Generalizes to `api_key_command`

### The Core Problem
Standard clients (OpenAI, Claude, Gemini, OpenAI-Compatible, Cohere) use `config_get_fn!` which resolves a STATIC api_key from env var or YAML. There's no hook for dynamic refresh.

### What the User Needs
A generic mechanism that:
1. Runs an arbitrary command to get a fresh token
2. Caches the result with a configurable TTL
3. Works for ANY client type (not just VertexAI/Bedrock)
4. Doesn't require each client to manually implement `Client` trait

### Reusable Components from Existing Patterns

**From `access_token.rs`** (highly reusable):
- The `IndexMap<String, (String, i64)>` cache pattern — key by client name, store (value, expires_at)
- The `is_valid_access_token` / `get_access_token` / `set_access_token` API pattern
- Thread-safe `RwLock` approach

**From Bedrock** (partially reusable):
- The subprocess execution pattern (`std::process::Command`)
- The KEY=VALUE output parsing approach (though for api_key_command, just stdout trimmed is simpler)
- The "try command, fall back to static config" pattern

### Generalization Strategy

The ideal `api_key_command` feature would combine:
1. **Bedrock's execution model**: Run a subprocess command (like `aws` CLI)
2. **VertexAI's caching model**: Cache the result with time-based expiry via `access_token.rs`
3. **Integration at `config_get_fn!` level**: Modify the macro or add a parallel path so standard clients get dynamic keys WITHOUT manually implementing `Client` trait

### Integration Points (in priority order)

1. **Option A — Modify `config_get_fn!` macro**: Add a command field check before static resolution. Least disruption to existing client code.
   ```
   env_var → api_key_command (cached) → config YAML → error
   ```

2. **Option B — Override in `request_builder()`**: Check for command-based key in the default `request_builder()` on the `Client` trait. Would intercept after `prepare_*` functions set the static key.

3. **Option C — New prepare function**: Like `prepare_gcloud_access_token()` but generic. Each client would need to call it (requires touching all clients).

**Recommendation**: Option A is cleanest. The `config_get_fn!` macro is THE resolution point for all config fields. Adding command-based resolution there means zero changes to individual client code. The cached result from `access_token.rs` (or a new similar cache) is returned as if it were a static config value.

### Caching Considerations

- **Reuse `access_token.rs`?**: Yes, the pattern is perfect. May need to rename or generalize (e.g., `credential_cache.rs`) since it stores any string value with expiry.
- **TTL source**: Unlike VertexAI (which gets `expires_in` from OAuth2 response), the command won't report expiry. Need a configurable TTL in YAML (e.g., `api_key_command_ttl: 300` seconds).
- **Default TTL**: Should be conservative — e.g., 5 minutes (300s) — to avoid stale tokens while reducing subprocess overhead.
- **Sync vs Async execution**: The `config_get_fn!` macro generates synchronous getters. Subprocess calls via `std::process::Command` are blocking but fast. Async (tokio::process) would be ideal but may require changing the macro's return type. Bedrock's precedent shows sync subprocess is acceptable.

---

## 6. Summary of Findings

1. **`access_token.rs` is a clean, reusable caching layer** — 34 lines, thread-safe, simple API. Can be reused or extended for `api_key_command` caching.

2. **VertexAI pattern** = async HTTP refresh + time-based cache + manual Client impl. Good caching design but tightly coupled to OAuth2 flow and requires manual trait implementation.

3. **Bedrock pattern** = sync subprocess + no caching + graceful fallback. Good subprocess execution model but expensive (new process per request) due to no caching.

4. **Neither pattern is directly reusable as-is** for a generic `api_key_command` because both require manual `Client` trait implementation. The goal is a mechanism that works with the `impl_client_trait!` macro-based clients.

5. **The ideal design combines**: Bedrock's subprocess execution + VertexAI's caching + integration at the `config_get_fn!` macro level (or a new parallel resolution path) for zero per-client code changes.
