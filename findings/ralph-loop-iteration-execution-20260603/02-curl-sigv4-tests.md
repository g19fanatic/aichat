# Findings: Curl SigV4 Signing Tests

## Summary

Comprehensive curl SigV4 tests reveal TWO key findings:
1. **Service name MUST be "bedrock"** (definitively confirmed via 403 error)
2. **`openai.gpt-5.5` does NOT exist in this AWS account** — it's not in foundation models, inference profiles, imported models, or marketplace models
3. **The `/v1/chat/completions` endpoint returns UnknownOperationException from ALL curl-based SigV4 signing approaches** — regardless of service name, signed headers, or hostname

## Test Results Matrix

### `/v1/chat/completions` path (ALL return UnknownOperationException)

| Test | Service Name | Signed Headers | Result |
|------|-------------|----------------|--------|
| 1 | bedrock | content-type;host;x-amz-date (curl default) | UnknownOperationException |
| 2 | bedrock-runtime | content-type;host;x-amz-date | UnknownOperationException |
| 3 | bedrock-mantle | content-type;host;x-amz-date | UnknownOperationException |
| 4 | bedrock | content-type;host;x-amz-date (explicit -u) | UnknownOperationException |
| 5 | bedrock-runtime | content-type;host;x-amz-date (explicit -u) | UnknownOperationException |
| Python | bedrock | content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token | UnknownOperationException |
| Python | bedrock-runtime | content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token | UnknownOperationException |
| Python | bedrock-mantle | content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token | UnknownOperationException |
| 9 | bedrock | + Accept: application/json | UnknownOperationException |
| 10 | bedrock | body: model="gpt-5.5" (no prefix) | UnknownOperationException |
| 12 | bedrock | hostname: bedrock.us-east-2.amazonaws.com (no -runtime) | UnknownOperationException |
| 13 | bedrock | path: /model/openai.gpt-5.5/v1/chat/completions | UnknownOperationException |
| 14 | bedrock | + X-Amz-Target: AmazonBedrockMantle.ChatCompletion | UnknownOperationException |
| 17 | bedrock-gateway | standard | UnknownOperationException |
| 19 | bedrock | path: /model/openai.gpt-5.5/chat/completions | UnknownOperationException |

### `/model/openai.gpt-5.5/invoke` path

| Test | Service Name | Result |
|------|-------------|--------|
| 6 | bedrock | **HTTP 400: "The provided model identifier is invalid."** |
| 7 | bedrock-runtime | **HTTP 403: "Credential should be scoped to correct service: 'bedrock'."** |
| 15 | bedrock (+ Accept header) | HTTP 400: "model identifier is invalid" |
| 16 | bedrock (no model in body) | HTTP 400: "model identifier is invalid" |
| 20 | bedrock (invoke-with-response-stream) | HTTP 400: "model identifier is invalid" |

### `/model/openai.gpt-5.5/converse` path

| Test | Service Name | Result |
|------|-------------|--------|
| 8 | bedrock | HTTP 400: "The provided model identifier is invalid." |
| 22 | bedrock (converse-stream) | HTTP 400: "model identifier is invalid" |

### Other tests

| Test | Details | Result |
|------|---------|--------|
| 18 | us-east-1 region | HTTP 000 (connection issue - wrong region for creds) |
| 21 | ARN format in URL | HTTP 403 (signature mismatch - URL encoding issue) |

## Critical Findings

### 1. Service Name is Definitively "bedrock" ✅

**PROOF**: Test 7 — using service name "bedrock-runtime" on the `/model/{id}/invoke` path returns:
```
HTTP 403: "Credential should be scoped to correct service: 'bedrock'."
```

This is a **definitive answer from the server itself**. The signing service name MUST be "bedrock".

### 2. `/v1/chat/completions` Endpoint Doesn't Route via Standard SigV4

The OpenAI-compatible endpoint `/v1/chat/completions` returns `UnknownOperationException` from ALL our attempts. This is a Coral framework error meaning "the service doesn't recognize this operation".

Possible explanations:
- The endpoint might require additional routing headers we're not sending
- The endpoint might not exist in this region/configuration
- Codex might access it via a different mechanism (SDK endpoint resolution)

**Key discrepancy**: AGENT.md claims aichat (with content-type fix) gets "model identifier is invalid" from this path. But looking at the CURRENT aichat code (bedrock.rs line 98), it uses `/model/{model_name}/invoke` — NOT `/v1/chat/completions`. This suggests the AGENT.md info about /v1/chat/completions might be from a prior code state that was reverted.

### 3. `openai.gpt-5.5` Does NOT Exist in Standard Bedrock APIs

**Foundation models (openai provider)**:
- openai.gpt-oss-120b-1:0
- openai.gpt-oss-20b-1:0
- openai.gpt-oss-safeguard-120b
- openai.gpt-oss-safeguard-20b

**NO** `openai.gpt-5.5` exists anywhere:
- Not in foundation models
- Not in inference profiles (0 APPLICATION type, ~40 SYSTEM_DEFINED — all Anthropic, Meta, Amazon, etc.)
- Not in imported models (empty)
- Not in marketplace endpoints (empty)

### 4. Current aichat Error Confirmed

Running the current aichat binary:
```
$ aichat --model aws-openai:openai.gpt-5.5 -- "say hello"
Error: Failed to call chat-completions api

Caused by:
    The provided model identifier is invalid.
```

This confirms the `/model/openai.gpt-5.5/invoke` path routes correctly (no UnknownOperationException) but the model simply doesn't exist in the standard InvokeModel API.

### 5. Debug Logging is Not Working

The aichat.log file remains empty even with `AICHAT_LOG_LEVEL=debug` and `RUST_LOG=debug`. The binary might not have logging compiled in, or the log path/level isn't being applied.

## Key Implications for the Fix

1. **The service name is NOT the problem** — "bedrock" is correct.
2. **The model doesn't exist in standard Bedrock APIs** — it's likely accessed through:
   - A newer Bedrock feature not yet in the standard API (Bedrock "Mantle" — which codex references)
   - A gateway/proxy layer that maps `openai.gpt-5.5` to an actual model
   - The OpenAI-compatible endpoint which we can't reach via standard SigV4
3. **The `/v1/chat/completions` endpoint might need a different access pattern** — perhaps codex's AWS SDK resolves a different endpoint URL entirely, or uses a newer protocol
4. **Investigating codex's actual network behavior is the highest-priority next step** — we need to see what URL, headers, and path codex actually sends

## Curl Command Used (for reference)

```bash
# Get credentials
aws configure export-credentials --profile anduril-armory --format env-no-export

# Test with curl (example)
curl -s -X POST \
  "https://bedrock-runtime.us-east-2.amazonaws.com/model/openai.gpt-5.5/invoke" \
  --aws-sigv4 "aws:amz:us-east-2:bedrock" \
  -u "$AWS_ACCESS_KEY_ID:$AWS_SECRET_ACCESS_KEY" \
  -H "Content-Type: application/json" \
  -H "x-amz-security-token: $AWS_SESSION_TOKEN" \
  -d '{"model":"openai.gpt-5.5","messages":[{"role":"user","content":"say hello"}],"max_tokens":50}'
```

## Authorization Header Analysis (from curl verbose output)

```
SignedHeaders=content-type;host;x-amz-date
Authorization: AWS4-HMAC-SHA256 Credential=ASIA.../20260603/us-east-2/bedrock/aws4_request, 
  SignedHeaders=content-type;host;x-amz-date, Signature=...
```

Note: curl's --aws-sigv4 does NOT include x-amz-security-token in SignedHeaders even though it sends the header. The token IS sent as a separate header but not signed.
