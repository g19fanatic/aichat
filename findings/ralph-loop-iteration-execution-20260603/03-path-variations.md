# Task 3: Path Variations Testing with SigV4

## Executive Summary

**CRITICAL DISCOVERY**: The correct path for the Bedrock OpenAI-compatible endpoint is **`/openai/v1/chat/completions`** (NOT `/v1/chat/completions` as currently used by aichat). The correct signing service is `"bedrock"`. This was proven by successfully calling the endpoint with model `openai.gpt-oss-120b-1:0` which returned an actual response.

## Methodology

- `curl --aws-sigv4` was found to be **NON-FUNCTIONAL** on this system (curl 7.81.0 compiled WITHOUT aws-sigv4 feature — flag is silently ignored)
- Used **Python with manual SigV4 signing** (hmac-sha256) for accurate testing
- Tested multiple path × service × model combinations

## Key Finding: curl --aws-sigv4 is BROKEN

**curl version**: 7.81.0 (Ubuntu 22.04 stock build)
**Features**: alt-svc AsynchDNS brotli GSS-API HSTS HTTP2 HTTPS-proxy IDN IPv6 Kerberos Largefile libz NTLM NTLM_WB PSL SPNEGO SSL TLS-SRP UnixSockets zstd

**NOTE**: The `aws` feature is NOT listed. The `--aws-sigv4` flag is silently accepted but does NOTHING. All curl tests sent unsigned requests (no Authorization header, no x-amz-date, no x-amz-security-token).

This explains why all curl tests to /v1/chat/completions returned UnknownOperationException — the requests were unsigned and rejected at the Coral routing layer.

## Results: Python SigV4-Signed Requests

### Path × Service Matrix (bedrock-runtime.us-east-2.amazonaws.com)

| Path | Service | HTTP | Response |
|------|---------|------|----------|
| `/model/openai.gpt-5.5/v1/chat/completions` | bedrock | 200 | UnknownOperationException |
| `/model/openai.gpt-5.5/chat/completions` | bedrock | 200 | UnknownOperationException |
| `/model/openai.gpt-5.5/v1/chat/completions` | bedrock-runtime | 200 | UnknownOperationException |
| `/model/openai.gpt-5.5/chat/completions` | bedrock-runtime | 200 | UnknownOperationException |
| `/v1/chat/completions` | bedrock | 200 | UnknownOperationException |
| `/v1/chat/completions` | bedrock-runtime | 200 | UnknownOperationException |
| **`/openai/v1/chat/completions`** | **bedrock** | **400** | **"The provided model identifier is invalid."** |
| `/openai/v1/chat/completions` | bedrock-runtime | 401 | "Credential should be scoped to correct service: 'bedrock'." |

### Key Observations:

1. **Only `/openai/v1/chat/completions` with service=`bedrock` gets past authentication** — all other combinations get UnknownOperationException (path doesn't exist) or 401 (wrong service)

2. **AWS explicitly confirms service name must be "bedrock"**: Error message "Credential should be scoped to correct service: 'bedrock'." when using bedrock-runtime

3. **ALL `/model/...` paths return UnknownOperationException** — these paths DO NOT EXIST on bedrock-runtime, even with proper SigV4 signing

4. **`/v1/chat/completions` (without /openai prefix) also returns UnknownOperationException** with proper SigV4 — this contradicts what was previously reported about aichat getting "model identifier is invalid" on this path. This discrepancy needs investigation.

## Results: Model Name Variations on /openai/v1/chat/completions

| Model Name | HTTP | Response |
|------------|------|----------|
| `openai.gpt-5.5` | 400 | "The provided model identifier is invalid." |
| `gpt-5.5` | 400 | "The provided model identifier is invalid." |
| `openai/gpt-5.5` | 400 | "The provided model identifier is invalid." |
| `us.openai.gpt-5.5` | 400 | "The provided model identifier is invalid." |
| `openai.gpt-5.5:0` | 400 | "The provided model identifier is invalid." |
| `amazon.openai.gpt-5.5` | 400 | "The provided model identifier is invalid." |
| **`openai.gpt-oss-120b-1:0`** | **200** | **SUCCESS! Returned "Hello! 👋 How can I help you today?"** |

## Results: Responses API

| Path | Model | HTTP | Response |
|------|-------|------|----------|
| `/openai/v1/responses` | `openai.gpt-5.5` | 400 | "The provided model identifier is invalid." |

The Responses API endpoint exists but also doesn't recognize `openai.gpt-5.5`.

## Results: Unsigned curl Tests (for path existence mapping)

| Path | HTTP | Response Format | Conclusion |
|------|------|-----------------|------------|
| `/openai/v1/chat/completions` | 401 | OpenAI JSON | Endpoint EXISTS (different auth layer) |
| `/openai/chat/completions` | 200 | UnknownOperationException | Does NOT exist |
| `/v1/chat/completions` | 200 | UnknownOperationException | Does NOT exist (unsigned) |
| `/model/.../v1/chat/completions` | 200 | UnknownOperationException | Does NOT exist |
| `/chat/completions` | 200 | UnknownOperationException | Does NOT exist |

## Critical Conclusions

### 1. PATH FIX NEEDED IN AICHAT
aichat currently uses `/v1/chat/completions`. It MUST use `/openai/v1/chat/completions`.

### 2. SERVICE NAME IS CORRECT
The signing service "bedrock" is already correct (confirmed by AWS error message).

### 3. MODEL IDENTIFIER IS THE REMAINING ISSUE
The `openai.gpt-5.5` model name is rejected by the /openai/v1/chat/completions endpoint on bedrock-runtime. However:
- `openai.gpt-oss-120b-1:0` works perfectly on this endpoint
- Codex works with `openai.gpt-5.5` 
- This suggests either:
  a. Codex uses a DIFFERENT endpoint/path than /openai/v1/chat/completions
  b. Codex uses an inference profile ARN instead of the raw model name
  c. The model needs to be enabled/subscribed in the account for this endpoint
  d. Codex maps the model name internally before sending to Bedrock

### 4. DISCREPANCY WITH PREVIOUS FINDINGS
AGENT.md states that aichat gets "model identifier is invalid" from `/v1/chat/completions`. But Python SigV4 testing shows `/v1/chat/completions` returns UnknownOperationException. This discrepancy may be due to:
- aichat including additional headers that route differently
- The Coral API routing being sensitive to specific header combinations
- A race condition or API version change

## Recommended Next Steps

1. **Change aichat's path from `/v1/chat/completions` to `/openai/v1/chat/completions`** — this is definitively the correct endpoint
2. **Investigate why openai.gpt-5.5 is rejected** — try inference profile ARNs, check if it's a subscription/access issue
3. **Trace what Codex actually sends** — the model identifier issue may require a different approach than just changing the path
4. **Test with inference profile ARN** — e.g., `arn:aws:bedrock:us-east-2:983393453355:inference-profile/openai.gpt-5.5`
