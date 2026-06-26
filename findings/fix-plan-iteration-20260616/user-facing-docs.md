# `api_key_command` — Dynamic API Key Resolution via Shell Command <!-- Implementation Complete -->

## Overview

The `api_key_command` feature allows any aichat client to resolve its API key by executing a shell command at runtime. This is useful when your API key or bearer token is short-lived (e.g., rotated every 30 minutes) and you have a command-line tool that can fetch a fresh one.

Instead of hardcoding a static `api_key` in your config, you point aichat at a command. Aichat runs the command, caches the output, and uses it as the API key until it expires.

---

## Configuration

### Basic Setup

Add `api_key_command` to any client that normally takes an `api_key`:

```yaml
clients:
  - type: openai-compatible
    name: staging
    api_base: https://staging.internal.example.com/v1
    api_key_command: "/usr/local/bin/get-staging-token"
    api_key_command_expires_in: 1800   # seconds (30 minutes)
    models:
      - name: gpt-4o
        max_input_tokens: 128000
```

### Fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `api_key_command` | string | No | *(none)* | Shell command whose stdout becomes the API key |
| `api_key_command_expires_in` | integer | No | 3600 (1 hour) | Seconds until the cached key is considered stale and the command is re-executed |

### Supported Client Types

`api_key_command` works with all clients that use an `api_key`:

- `openai`
- `openai-compatible`
- `claude`
- `gemini`
- `cohere`
- `azure-openai`

It does **NOT** apply to `vertexai` or `bedrock`, which have their own specialized credential mechanisms (OAuth2 and AWS STS respectively).

---

## Resolution Priority

When aichat needs an API key for a request, it resolves it in this order:

1. **Environment variable** — `{CLIENT_NAME}_API_KEY` (e.g., `OPENAI_API_KEY`, `STAGING_API_KEY`)
2. **Config YAML `api_key` field** — static string in your config file
3. **`api_key_command`** — execute the command and use its stdout (cached)
4. **Error** — if none of the above succeeds

This means:
- If you set an environment variable, the command is **never** executed
- If you set a static `api_key` in config, the command is **never** executed
- The command is a **fallback** for when no static key is available
- To use `api_key_command`, simply **omit** both the env var and the `api_key` field

---

## Caching Behavior

### How Caching Works

The first time aichat needs the API key (and no static key is available):
1. Executes your `api_key_command`
2. Reads stdout, trims whitespace, stores it in an in-memory cache
3. Uses the cached value for all subsequent requests

### Expiry (`api_key_command_expires_in`)

| Setting | Behavior |
|---------|----------|
| `api_key_command_expires_in: 1800` | Re-runs the command after 1800 seconds (30 minutes) |
| `api_key_command_expires_in: 300` | Re-runs the command after 5 minutes |
| *(field omitted)* | Token is cached with default 1-hour (3600s) expiry; re-runs after 1 hour |

### Cache Scope

- The cache is **in-memory only** — restarting aichat always re-runs the command
- Each client instance has its own cache entry (keyed by client `name`)
- Multiple models under the same client share the same cached key
- The cache is thread-safe for concurrent requests

### Cache Invalidation

The cache is **purely time-based**. If a cached token causes an HTTP 401/403 from the API but hasn't expired according to `api_key_command_expires_in`, aichat will retry with the same (now-invalid) cached token.

**Workaround**: Set `api_key_command_expires_in` to a value shorter than your token's actual lifetime to ensure the cache expires before the token does.

---

## Command Execution

### Shell Environment

Your command runs through your system's default shell:
- **Linux/macOS**: `/bin/bash -c "<your command>"` (or `$SHELL` if set)
- **Windows**: `cmd.exe /C "<your command>"` (or PowerShell if detected)

This means you can use pipes, environment variables, subshells, and other shell features:

```yaml
api_key_command: "vault read -field=token secret/api-key | tr -d '\\n'"
api_key_command: "aws secretsmanager get-secret-value --secret-id my-key --query SecretString --output text"
api_key_command: "cat /run/secrets/api-token"
api_key_command: "op read 'op://Vault/API Key/credential'"
```

### Output Requirements

The command should:
- Exit with status code 0 (success)
- Print the API key/token to **stdout**
- Output valid UTF-8 text

Leading and trailing whitespace (including newlines) is automatically stripped.

> **Note**: The implementation uses `String::from_utf8_lossy` for output conversion.
> Non-UTF-8 bytes will be replaced with the Unicode replacement character (U+FFFD)
> rather than causing an error. However, this may result in a malformed token.

### Timing

The command runs **synchronously** — aichat blocks until it completes. Keep your command fast (ideally under 1 second). For slow credential fetches, consider:
- Using a local caching wrapper script
- Setting a longer `api_key_command_expires_in` to reduce how often it runs
- Having your command call a local daemon that pre-caches tokens

---

## Error Handling

### Command Failures

| Failure | Error Message | What Happens |
|---------|---------------|--------------|
| Command not found / cannot execute | `Failed to execute api_key_command: <error>` | Request fails |
| Non-zero exit code | `api_key_command exited with <status>: <stderr>` | Request fails |
| Empty stdout | `api_key_command returned empty output` | Request fails |
| Non-UTF-8 output | *(handled gracefully — invalid bytes replaced with U+FFFD)* | Token may be malformed |

### Retry Behavior

When the command fails:
- aichat's built-in retry mechanism (3 attempts with exponential backoff) applies
- Each retry re-executes the command (since the cache doesn't have a valid entry)
- If all retries fail, the user sees the error message

### No Fallback to Static Key

If you configure `api_key_command` and omit `api_key`, there is no fallback — if the command fails, the request fails. If you want a fallback, you can set a static `api_key` as well (the command will only be used if the static key is also absent).

---

## Examples

### Example 1: HashiCorp Vault

Fetch a token from Vault with 1-hour TTL:

```yaml
clients:
  - type: openai-compatible
    name: internal-llm
    api_base: https://llm.internal.corp/v1
    api_key_command: "vault read -field=token secret/llm/api-key"
    api_key_command_expires_in: 3600
    models:
      - name: llama-70b
        max_input_tokens: 128000
```

### Example 2: AWS Secrets Manager

```yaml
clients:
  - type: openai
    api_key_command: "aws secretsmanager get-secret-value --secret-id openai-key --query SecretString --output text"
    api_key_command_expires_in: 86400   # refresh daily
```

### Example 3: 1Password CLI

```yaml
clients:
  - type: claude
    api_key_command: "op read 'op://Development/Anthropic API/credential'"
    api_key_command_expires_in: 7200    # refresh every 2 hours
```

### Example 4: Custom Script with Token Refresh

```yaml
clients:
  - type: openai-compatible
    name: staging
    api_base: https://staging.example.com/v1
    api_key_command: "/home/user/bin/refresh-staging-token.sh"
    api_key_command_expires_in: 1800    # re-run every 30 min
    models:
      - name: gpt-4o
        max_input_tokens: 128000
```

Where `refresh-staging-token.sh` might be:

```bash
#!/bin/bash
# Fetch a new short-lived token from your IdP
curl -s -X POST https://auth.example.com/token \
  -d "grant_type=client_credentials" \
  -d "client_id=$CLIENT_ID" \
  -d "client_secret=$CLIENT_SECRET" | jq -r '.access_token'
```

### Example 5: Simple File-Based Token (No Expiry)

If your token is written to a file by another process and you just want to read it:

```yaml
clients:
  - type: openai-compatible
    name: local
    api_base: http://localhost:8080/v1
    api_key_command: "cat /tmp/current-token.txt"
    # No expires_in — cached for entire session; restart aichat to pick up changes
```

### Example 6: Google Cloud Identity Token (for OpenAI-compatible endpoints behind IAP)

```yaml
clients:
  - type: openai-compatible
    name: gcp-endpoint
    api_base: https://my-endpoint.run.app/v1
    api_key_command: "gcloud auth print-identity-token"
    api_key_command_expires_in: 3000   # GCP tokens expire in ~3600s; refresh early
    models:
      - name: my-model
        max_input_tokens: 32000
```

---

## Testing Your Configuration

### Step 1: Verify Your Command Works

Run your command directly in the terminal and confirm it outputs a valid token:

```bash
$ /usr/local/bin/get-staging-token
eyJhbGciOiJSUzI1NiIs...
```

Ensure:
- Exit code is 0: `echo $?` should print `0`
- Output is a single token (no extra lines, JSON wrapping, etc.)
- No prompts or interactive input are required

### Step 2: Configure Without Static Key

In your `config.yaml`, make sure you do NOT set `api_key` alongside `api_key_command`:

```yaml
clients:
  - type: openai-compatible
    name: staging
    api_base: https://staging.example.com/v1
    # api_key: <omitted on purpose>
    api_key_command: "/usr/local/bin/get-staging-token"
    api_key_command_expires_in: 1800
    models:
      - name: gpt-4o
        max_input_tokens: 128000
```

Also ensure no environment variable overrides it:
```bash
unset STAGING_API_KEY
```

### Step 3: Test with aichat

```bash
$ aichat -m staging:gpt-4o "Hello, world"
```

If the command works, you should see a normal response. If it fails, you'll see an error message indicating what went wrong with the command execution.

### Step 4: Verify Caching (Optional)

To confirm caching works, add logging to your command script:

```bash
#!/bin/bash
echo "$(date): token requested" >> /tmp/token-refresh.log
# ... actual token fetch ...
echo "$TOKEN"
```

Then make multiple requests and check the log — you should see the command called only once (until `api_key_command_expires_in` elapses).

---

## Interaction with Environment Variables

The environment variable for the client's API key always takes priority:

```bash
# This OVERRIDES api_key_command — the command will never run
export STAGING_API_KEY="static-token-from-env"
```

The env var name is derived from the client name (uppercased) + `_API_KEY`:
- Client name `staging` → env var `STAGING_API_KEY`
- Client name `openai` → env var `OPENAI_API_KEY`
- Client name `my-service` → env var `MY-SERVICE_API_KEY`

To use `api_key_command`, ensure the corresponding env var is **unset**.

---

## Comparison with VertexAI and Bedrock

| Feature | `api_key_command` | VertexAI | Bedrock |
|---------|-------------------|----------|---------|
| Credential source | Any shell command | Google OAuth2 (automatic) | AWS CLI (automatic) |
| Caching | Yes, time-based | Yes, uses OAuth2 `expires_in` | No (per-request) |
| Configuration | Explicit in YAML | Automatic from ADC file | Automatic from AWS profile |
| Works with | All api_key clients | VertexAI only | Bedrock only |
| User effort | Write/find a command | `gcloud auth` setup | `aws configure` setup |

`api_key_command` is the **generic** solution for any client type. VertexAI and Bedrock have purpose-built integrations that handle their platform-specific credential flows automatically.

---

## FAQ

**Q: Can I use `api_key_command` with VertexAI or Bedrock?**
A: No. These clients don't use the `api_key` field — they have their own credential mechanisms. Use their native configuration instead.

**Q: What if my command takes a long time to run?**
A: aichat blocks during command execution. Use `api_key_command_expires_in` to cache the result and reduce how often the command runs. Consider wrapping slow commands in a local caching script.

**Q: Can my command read aichat's config or context?**
A: The command runs as a simple subprocess with no special environment from aichat. It inherits the current shell environment (env vars, PATH, etc.) but receives no arguments or stdin from aichat.

**Q: What if the token expires mid-conversation?**
A: Each API request checks the cache. If `api_key_command_expires_in` has elapsed, the command is re-run automatically before the next request. Users don't need to do anything — the refresh is transparent.

**Q: Can I use different commands for different models under the same client?**
A: No. `api_key_command` is per-client, not per-model. All models under the same client share the same API key. If different models need different credentials, configure them as separate clients.

**Q: Does it work in REPL mode, command mode, and serve mode?**
A: Yes. The API key resolution happens per-request regardless of aichat's execution mode. The cache persists for the duration of the process.

**Q: What happens if I set both `api_key` and `api_key_command`?**
A: The static `api_key` takes priority (resolution order: env var → `api_key` → `api_key_command`). The command will never be executed. This can be useful as a fallback: set `api_key_command` for production but keep `api_key` for local development testing.

**Q: Is the token stored on disk?**
A: No. The cached token exists only in memory for the lifetime of the aichat process. It is never written to disk, history files, or log files.

---

## Implementation Details (Source Reference)

This section documents where the feature is implemented for developers working on the codebase.

### Core Module

| File | Lines | Contents |
|------|-------|----------|
| `src/client/api_key_command.rs` | 1–69 | `run_api_key_command()` (lines 12–30) and `resolve_api_key()` (lines 36–69) |
| `src/client/mod.rs` | 2, 14 | Module declaration and `pub use` re-export |

### Config Struct Fields (`api_key_command` + `api_key_command_expires_in`)

| File | Lines |
|------|-------|
| `src/client/openai.rs` | 18–19 |
| `src/client/openai_compatible.rs` | 14–15 |
| `src/client/claude.rs` | 17–18 |
| `src/client/gemini.rs` | 16–17 |
| `src/client/cohere.rs` | 17–18 |
| `src/client/azure_openai.rs` | 12–13 |

### Call Sites (using `resolve_api_key`)

| File | Line(s) | Function | Error Handling |
|------|---------|----------|----------------|
| `src/client/openai.rs` | 48, 68 | `prepare_chat_completions`, `prepare_embeddings` | `?` (hard fail) |
| `src/client/openai_compatible.rs` | 44, 64, 81 | `prepare_chat_completions`, `prepare_embeddings`, `prepare_rerank` | `.ok()` (optional auth) |
| `src/client/claude.rs` | 47 | `prepare_chat_completions` | `?` (hard fail) |
| `src/client/gemini.rs` | 46, 73 | `prepare_chat_completions`, `prepare_embeddings` | `?` (hard fail) |
| `src/client/cohere.rs` | 47, 68, 95 | `prepare_chat_completions`, `prepare_embeddings`, `prepare_rerank` | `?` (hard fail) |
| `src/client/azure_openai.rs` | 50, 69 | `prepare_chat_completions`, `prepare_embeddings` | `?` (hard fail) |

### Deviations from Original Design Document (`implementation-plan.md`)

| Area | Original Plan | Actual Implementation |
|------|---------------|----------------------|
| Default expiry when field omitted | `i64::MAX` (never expires) | `DEFAULT_EXPIRES_IN = 3600` (1 hour) |
| UTF-8 handling | Strict (`String::from_utf8()`) | Lossy (`String::from_utf8_lossy()`) — replaces invalid bytes with U+FFFD |
| Error message format (exit code) | `"api_key_command failed (exit N): <stderr>"` | `"api_key_command exited with <status>: <stderr>"` |
| Error imports | `use anyhow::{bail, Context, Result}` | `use anyhow::{anyhow, bail, Result}` — uses `anyhow!()` for IO error wrapping |

### Dependencies Used

- `anyhow` — error handling (`anyhow!`, `bail!`, `Result`)
- `chrono::Utc` — timestamp for cache expiry calculation
- `std::process::Command` — subprocess execution
- `parking_lot::RwLock` (via `access_token.rs`) — thread-safe token cache
- `crate::utils::SHELL` — platform-specific shell detection (cmd/arg pair)

### Total Call Sites: 14

- 2 OpenAI + 3 OpenAI-Compatible + 1 Claude + 2 Gemini + 3 Cohere + 2 Azure OpenAI + 1 config example (task 17)
