# Implementation Plan: `api_key_command` Feature

## Overview

This document enumerates every file, struct, function, and line that must be created or modified to implement the `api_key_command` feature. The design is based on the approach documented in `api-key-command-design.md` (Option B: helper function, not macro modification).

---

## 1. New File: `src/client/api_key_command.rs`

### Purpose
Contains the `resolve_api_key` helper function and `run_api_key_command` subprocess execution logic.

### Functions to Implement

```rust
/// Execute a shell command and return its trimmed stdout as the API key.
/// Uses the SHELL detection from utils/command.rs.
fn run_api_key_command(command: &str) -> Result<String>

/// Resolve api_key with command fallback and caching.
/// Priority: env/config (get_api_key result) → cached token → command execution → error
pub fn resolve_api_key(
    client_name: &str,
    get_api_key_result: Result<String>,
    api_key_command: Option<&str>,
    expires_in: Option<u64>,
) -> Result<String>
```

### Dependencies
- `use anyhow::{bail, Context, Result};`
- `use super::access_token::{get_access_token, is_valid_access_token, set_access_token};`
- `use crate::utils::SHELL;`
- `use chrono::Utc;`
- `use std::process::Command;`

### Implementation Notes
- `run_api_key_command`: Uses `SHELL.cmd` and `SHELL.arg` (from `src/utils/command.rs:17`) to execute the command via `std::process::Command` (synchronous, matches Bedrock's approach at `bedrock.rs:206`)
- `resolve_api_key`: Checks static resolution first (pass-through on success), then cache via `is_valid_access_token`/`get_access_token`, then runs command and caches via `set_access_token`
- Expiry: `expires_in.map(|s| Utc::now().timestamp() + s as i64).unwrap_or(i64::MAX)`

---

## 2. Module Registration: `src/client/mod.rs`

**File**: `src/client/mod.rs:1-8`

### Change
Add `mod api_key_command;` and re-export the public function.

```diff
 mod access_token;
+mod api_key_command;
 mod common;
 mod message;
 #[macro_use]
 mod macros;
 mod model;
 mod stream;

 pub use crate::function::ToolCall;
 pub use common::*;
 pub use message::*;
 pub use model::*;
 pub use stream::*;
```

Also add a `pub use` for the resolve function (or keep it `pub(super)` and use `super::api_key_command::resolve_api_key` from within client modules since they all `use super::*`):

```diff
+pub use self::api_key_command::resolve_api_key;
```

**Alternative**: Since all client modules already do `use super::*;`, making `resolve_api_key` public in `api_key_command.rs` and the module itself `pub(crate)` would suffice.

---

## 3. Config Struct Changes (Add 2 Fields Each)

Each config struct that has `api_key` needs two new `Option` fields. All use `#[serde(default)]` or rely on `Option`'s default deserialization.

### 3a. `src/client/openai.rs` — `OpenAIConfig`

**Lines**: 12-22 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize, Default)]
 pub struct OpenAIConfig {
     pub name: Option<String>,
     pub api_key: Option<String>,
     pub api_base: Option<String>,
     pub organization_id: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

### 3b. `src/client/openai_compatible.rs` — `OpenAICompatibleConfig`

**Lines**: 9-18 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize)]
 pub struct OpenAICompatibleConfig {
     pub name: Option<String>,
     pub api_base: Option<String>,
     pub api_key: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

### 3c. `src/client/claude.rs` — `ClaudeConfig`

**Lines**: 12-21 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize)]
 pub struct ClaudeConfig {
     pub name: Option<String>,
     pub api_key: Option<String>,
     pub api_base: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

### 3d. `src/client/gemini.rs` — `GeminiConfig`

**Lines**: 11-20 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize, Default)]
 pub struct GeminiConfig {
     pub name: Option<String>,
     pub api_key: Option<String>,
     pub api_base: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

### 3e. `src/client/cohere.rs` — `CohereConfig`

**Lines**: 12-21 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize, Default)]
 pub struct CohereConfig {
     pub name: Option<String>,
     pub api_key: Option<String>,
     pub api_base: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

### 3f. `src/client/azure_openai.rs` — `AzureOpenAIConfig`

**Lines**: 7-16 (struct definition)

```diff
 #[derive(Debug, Clone, Deserialize)]
 pub struct AzureOpenAIConfig {
     pub name: Option<String>,
     pub api_base: Option<String>,
     pub api_key: Option<String>,
+    pub api_key_command: Option<String>,
+    pub api_key_command_expires_in: Option<u64>,
     #[serde(default)]
     pub models: Vec<ModelData>,
     pub patch: Option<RequestPatch>,
     pub extra: Option<ExtraConfig>,
 }
```

---

## 4. `prepare_*` Function Changes (Replace `get_api_key()` Calls)

Each call site needs to replace `self_.get_api_key()?` (or `.ok()`) with `resolve_api_key(...)`.

### Important: Accessing Client Name in Free Functions

The `prepare_*` functions receive `self_: &XxxClient` — the client's name is accessible via `self_.name()` (generated by `client_common_fns!()` macro at `macros.rs:155`, which calls `Self::name(&self.config)`).

### 4a. `src/client/openai.rs`

**Line 46** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

**Line 66** — `prepare_embeddings`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

### 4b. `src/client/openai_compatible.rs`

**Line 42** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key().ok();
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    ).ok();
```

**Line 62** — `prepare_embeddings`:
```diff
-    let api_key = self_.get_api_key().ok();
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    ).ok();
```

**Line 79** — `prepare_rerank`:
```diff
-    let api_key = self_.get_api_key().ok();
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    ).ok();
```

### 4c. `src/client/claude.rs`

**Line 45** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

### 4d. `src/client/gemini.rs`

**Line 44** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

**Line 71** — `prepare_embeddings`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

### 4e. `src/client/cohere.rs`

**Line 45** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

**Line 66** — `prepare_embeddings`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

**Line 93** — `prepare_rerank`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

### 4f. `src/client/azure_openai.rs`

**Line 48** — `prepare_chat_completions`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

**Line 67** — `prepare_embeddings`:
```diff
-    let api_key = self_.get_api_key()?;
+    let api_key = resolve_api_key(
+        self_.name(),
+        self_.get_api_key(),
+        self_.config.api_key_command.as_deref(),
+        self_.config.api_key_command_expires_in,
+    )?;
```

---

## 5. Documentation: `config.example.yaml`

**File**: `config.example.yaml` (add after the `openai-compatible` example block, ~line 130)

Add a commented example showing `api_key_command` usage:

```yaml
  # Example: Using a command to dynamically obtain an API key
  # - type: openai-compatible
  #   name: staging
  #   api_base: https://staging.internal.example.com/v1
  #   api_key_command: "/usr/local/bin/get-staging-token"        # Shell command whose stdout becomes api_key
  #   api_key_command_expires_in: 1800                           # Optional: seconds until cached token expires
  #   models:
  #     - name: gpt-4o
  #       max_input_tokens: 128000
```

---

## 6. Files NOT Modified

| File | Reason |
|------|--------|
| `src/client/macros.rs` | `config_get_fn!` macro remains untouched; logic added externally |
| `src/client/access_token.rs` | Reused as-is for caching (no API changes needed) |
| `src/client/vertexai.rs` | Has its own OAuth2 mechanism; `api_key_command` not applicable |
| `src/client/bedrock.rs` | Has its own AWS CLI mechanism; `api_key_command` not applicable |
| `src/client/common.rs` | No changes needed (trait definition unchanged) |
| `src/client/model.rs` | No changes needed |
| `src/client/stream.rs` | No changes needed |
| `src/utils/command.rs` | Reused as-is (`SHELL` static used by api_key_command.rs) |

---

## 7. Summary: Complete File Change Manifest

### New Files (1)
| File | Lines (estimated) | Purpose |
|------|-------------------|---------|
| `src/client/api_key_command.rs` | ~45 | `run_api_key_command()` + `resolve_api_key()` |

### Modified Files (8)
| File | Lines Changed | Change Description |
|------|---------------|-------------------|
| `src/client/mod.rs:1-2` | +2 | Add `mod api_key_command;` + re-export |
| `src/client/openai.rs:15-16, 46, 66` | +4, ~2 | Add struct fields; update 2 call sites |
| `src/client/openai_compatible.rs:12-13, 42, 62, 79` | +4, ~3 | Add struct fields; update 3 call sites |
| `src/client/claude.rs:15-16, 45` | +4, ~1 | Add struct fields; update 1 call site |
| `src/client/gemini.rs:14-15, 44, 71` | +4, ~2 | Add struct fields; update 2 call sites |
| `src/client/cohere.rs:15-16, 45, 66, 93` | +4, ~3 | Add struct fields; update 3 call sites |
| `src/client/azure_openai.rs:10-11, 48, 67` | +4, ~2 | Add struct fields; update 2 call sites |
| `config.example.yaml:~130` | +8 | Add commented `api_key_command` example |

### Total Call Sites Modified: 14
(2 OpenAI + 3 OpenAI-Compatible + 1 Claude + 2 Gemini + 3 Cohere + 2 Azure + 1 config example)

---

## 8. Implementation Order (Recommended)

The implementation should proceed in this order to allow incremental compilation checks:

1. **Create `src/client/api_key_command.rs`** — standalone module with both functions
2. **Register module in `src/client/mod.rs`** — `mod api_key_command;` + re-export
3. **Add struct fields** to all 6 config structs (compile check: `cargo build` should pass since fields are `Option` with no new behavior yet)
4. **Update call sites** in all `prepare_*` functions (14 locations)
5. **Update `config.example.yaml`** — add commented documentation
6. **Test** — `cargo build` + `cargo test` + manual test with a config using `api_key_command`

---

## 9. Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| `SHELL` import path wrong | Low | `use crate::utils::SHELL;` — verified via grep at `utils/mod.rs:15` re-export |
| `access_token.rs` not visible from new module | Low | Path is `super::access_token::*` — same as `vertexai.rs:1` |
| Serde ignores unknown fields by default | None | New `Option` fields deserialize to `None` for existing configs |
| Blocking subprocess in async context | Low | Same pattern as Bedrock (`bedrock.rs:206`); `prepare_*` are sync fns called from async context |
| Cache key collision between api_key_command and VertexAI | Low | VertexAI uses `self.name()` as cache key; api_key_command also uses client name. Different clients have different names, so no collision. Same-name collision is user misconfiguration. |
| `self_.name()` availability in free functions | None | Verified: `self_.name()` works in free fns because the Client trait impl (from `client_common_fns!()`) delegates to `Self::name(&self.config)` at `macros.rs:155-156`. In free functions, `self_` is the client struct which has the trait method. Confirmed by `openai_compatible.rs:82` using `self_.name()`. |

---

## 10. Dependency / Import Details

### `src/client/api_key_command.rs` imports:
```rust
use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::process::Command;

use super::access_token::{get_access_token, is_valid_access_token, set_access_token};
use crate::utils::SHELL;
```

### Client modules (openai.rs, etc.) — no new imports needed:
- `resolve_api_key` is accessible via `use super::*;` (already present in all client modules) since `mod.rs` re-exports it
- No additional `use` statements required in individual client files

---

## 11. Full `resolve_api_key` Logic (Pseudocode)

```
fn resolve_api_key(client_name, get_api_key_result, api_key_command, expires_in):
    // Step 1: Static resolution succeeded → use it (env var or config YAML)
    if get_api_key_result is Ok(key):
        return Ok(key)
    
    // Step 2: No command configured → return original error
    if api_key_command is None:
        return get_api_key_result  // propagates "Miss 'api_key'" error
    
    // Step 3: Check cache
    if is_valid_access_token(client_name):
        return get_access_token(client_name)
    
    // Step 4: Execute command
    let token = run_api_key_command(api_key_command)?
    
    // Step 5: Cache with expiry
    let expires_at = match expires_in:
        Some(secs) => Utc::now().timestamp() + secs as i64
        None => i64::MAX
    set_access_token(client_name, token.clone(), expires_at)
    
    return Ok(token)
```

This prioritization ensures:
- Environment variables always win (existing contract)
- Static config `api_key` wins over command (simpler debugging)
- Cached token avoids repeated subprocess calls
- Command only runs on first request or cache expiry
