# 01 - Current State: aichat request to bedrock for openai.gpt-5.5

## Summary

The current aichat binary (with local modifications) sends a request to AWS Bedrock using the InvokeModel path (`/model/openai.gpt-5.5/invoke`). The request reaches Bedrock successfully but returns **"The provided model identifier is invalid."**

## Exact Request Details (Captured via instrumented binary)

### Endpoint
```
POST https://bedrock-runtime.us-east-2.amazonaws.com/model/openai.gpt-5.5/invoke
```

### Headers
```
host: bedrock-runtime.us-east-2.amazonaws.com
content-type: application/json
x-amz-date: 20260603T152820Z
x-amz-security-token: IQoJb3JpZ2luX2VjEHYaCXVzLXdlc3QtMiJH... (STS session token)
authorization: AWS4-HMAC-SHA256 Credential=ASIA6J5WRIUVTWRZXMUT/20260603/us-east-2/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature=60f6bdfcb351d800d5b4841d7ec3187f6b43fff2ad98c552e06ca32dbb9d4b23
```

### Body (prettified, tools truncated)
```json
{
  "model": "openai.gpt-5.5",
  "messages": [
    {"role": "user", "content": "say hello"}
  ],
  "tools": [
    // MASSIVE array - all 21+ tools from the user's aichat configuration (code_navigator, fs_cat, gui_session, etc.)
    // This includes audio_from_yt, audio_to_text, code_navigator, ddg_fetch, fetch_url_via_curl,
    // fs_cat, fs_git_diff, fs_info, fs_ls, fs_mkdir, fs_read, fs_rm, fs_write, get_current_time,
    // gui_session, patch, ralph_loop, recursive_grep, safe_script_executor, search_wikipedia,
    // skills, subagent, whitelist_command, sequentialthinking_sequentialthinking
  ]
}
```

### Error Response
```
Error: Failed to call chat-completions api

Caused by:
    The provided model identifier is invalid.
```

## SigV4 Signing Details

- **Service**: `bedrock` (in credential scope: `20260603/us-east-2/bedrock/aws4_request`)
- **Region**: `us-east-2`
- **Signed Headers**: `content-type;host;x-amz-date;x-amz-security-token`
- **Algorithm**: `AWS4-HMAC-SHA256`
- **Credentials Source**: STS temporary credentials from `anduril-armory` profile (fetched via `aws configure export-credentials`)

## Current Code Architecture (from git diff)

The code has been modified from the upstream with these key changes:

1. **BedrockModelCategory enum**: Routes `openai.*` models differently from standard Converse API models
2. **Profile support**: Added `profile` field to BedrockConfig, falls back to config profile when env vars aren't set
3. **OpenAI routing**: For `openai.*` models:
   - URI: `/model/{model_name}/invoke`
   - Body: `openai_build_chat_completions_body()` (standard OpenAI chat completions format)
   - Response parsing: Uses `openai_chat_completions()` and `openai_chat_completions_streaming()`
4. **SigV4 fixes**: Content-Type header included in signing, headers sorted alphabetically

### Key Code Paths (bedrock.rs)

```rust
// Line 95: OpenAI path
BedrockModelCategory::OpenAI => {
    let uri = format!("/model/{model_name}/invoke");
    let body = openai_build_chat_completions_body(data, &self.model);
    (uri, body)
}

// Line 116: Service name for SigV4
AwsRequest {
    method: Method::POST,
    host,                           // bedrock-runtime.{region}.amazonaws.com
    service: "bedrock".into(),      // SigV4 service name
    uri,                            // /model/openai.gpt-5.5/invoke
    querystring: "".into(),
    headers,
    body: body.to_string(),
}
```

## Key Observations

### 1. The URI is Wrong for OpenAI-Compatible Endpoint
The current code uses `/model/openai.gpt-5.5/invoke` which is the **InvokeModel** API path. But the OpenAI-compatible API on Bedrock uses `/v1/chat/completions`. The AGENT.md learnings note that `/v1/chat/completions` also returned "model identifier is invalid" — but that was with `service: "bedrock"` for signing.

### 2. SigV4 Service Name May Be Wrong
The signing uses `service: "bedrock"`. However:
- The codex binary strings mention "Bedrock Mantle" — suggesting a different service name
- The AWS SDK auto-resolves service names; for some newer Bedrock features, it might use `bedrock-runtime` or `bedrock-mantle`
- **The credential scope shows**: `/us-east-2/bedrock/aws4_request` — if AWS expects a different service name, the signature would be invalid (but we'd get an auth error, not "model identifier is invalid")

### 3. The Body Includes a Massive Tools Array
The request body includes ALL tools from the user's aichat configuration. This is a very large payload (~15KB+). While unlikely to cause the "model identifier" error, it could cause issues with other endpoints. The `use_tools: code_assistant` setting in the role is causing all tools to be included.

### 4. Error Analysis
- **"The provided model identifier is invalid"** means:
  - The request was properly authenticated (SigV4 signature accepted)
  - The endpoint was found (no 404 or UnknownOperationException)
  - AWS Bedrock looked at the model name and didn't recognize it
  - This could mean: wrong endpoint (InvokeModel doesn't support this model), wrong format, or the model needs a different access path

### 5. Previous Attempts (from AGENT.md learnings)
| Path | Service | Result |
|------|---------|--------|
| /v1/chat/completions (no content-type) | bedrock | UnknownOperationException |
| /model/openai.gpt-5.5/v1/chat/completions | bedrock | UnknownOperationException |
| /model/openai.gpt-5.5/converse | bedrock | "model identifier is invalid" |
| /model/openai.gpt-5.5/invoke | bedrock | "model identifier is invalid" ← CURRENT |
| /v1/chat/completions (WITH content-type+sorting) | bedrock | "model identifier is invalid" |

## Recommendations for Next Steps

1. **Try `/v1/chat/completions` with `service: "bedrock-runtime"`** — The OpenAI-compatible endpoint may require a different SigV4 service name. If the service name is wrong, the signature would be invalid, which could manifest as different errors depending on AWS's validation order.

2. **Try curl with different `--aws-sigv4` service names** — Quick way to test: `aws:amz:us-east-2:bedrock-runtime` vs `aws:amz:us-east-2:bedrock`

3. **Check if codex uses `bedrock-mantle` as the service** — The strings in codex binary showed "Amazon Bedrock Mantle" references.

4. **Try without tools in the body** — Strip the tools array to see if a minimal request works. The model may not support function calling through this API.

5. **Try different model name formats in the body** — `"gpt-5.5"`, `"openai/gpt-5.5"`, keep the path as `/v1/chat/completions`

## Debug Logging Note

The built-in debug logging (AICHAT_LOG_LEVEL=debug) does NOT work for release builds — the `v.parse().ok()` for LevelFilter is failing silently and defaulting to `Off`. The log file remains empty. Instrumentation was added temporarily via `eprintln!` to capture the above data (now reverted).

## Files Modified (current git diff)

Only one file is modified: `src/client/bedrock.rs` (74 insertions, 22 deletions)

Key changes:
- Added BedrockModelCategory enum and routing
- Added profile support for credential resolution
- Added content-type header to SigV4 signing
- Added header sorting for proper SigV4 canonical request
- Changed OpenAI response parsing to use openai_chat_completions functions
