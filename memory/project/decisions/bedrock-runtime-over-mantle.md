---
summary: "Chose bedrock-runtime /v1/chat/completions over bedrock-mantle as primary endpoint for OpenAI model support"
created: 2026-06-02
updated: 2026-06-02
type: decision
scope: project
project: aichat
importance: high
confidence: verified
tags: [bedrock, architecture, endpoint]
sources:
  - type: soft
    key: conversation/2026-06-02/bedrock-mantle-troubleshooting
    note: "Source code analysis + Codex comparison revealed two viable options"
---

# Decision: bedrock-runtime over bedrock-mantle as Primary Endpoint

## Context
OpenAI models on AWS Bedrock (e.g., `openai.gpt-5.5`) are accessible via:
1. **bedrock-mantle**: `https://bedrock-mantle.{region}.api.aws` (SigV4 service: `"bedrock-mantle"`)
2. **bedrock-runtime**: `https://bedrock-runtime.{region}.amazonaws.com/v1/chat/completions` (SigV4 service: `"bedrock"`)

Both support `/v1/chat/completions` with standard OpenAI request/response format.

## Decision
Use **bedrock-runtime** `/v1/chat/completions` as the primary path.

## Rationale
- Zero changes to host construction (same `bedrock-runtime.{region}.amazonaws.com`)
- Zero changes to SigV4 service name (`"bedrock"` stays)
- Zero changes to credential resolution
- Same DNS resolution and network path
- Only the URI path and body/response format change
- bedrock-mantle available as user-configurable override (Phase 2 `endpoint:` field)

## Rejected Alternatives
1. **bedrock-mantle as primary**: Different host, different SigV4 service, limited to 12 regions
2. **Separate client type (`bedrock-mantle`)**: Duplicates code, forces two configs for same account
3. **RequestPatch override**: URL built in `aws_fetch()` not through `request_builder()` — can't override
4. **Generic `api_format` config field**: Unnecessary ceremony; prefix detection more ergonomic
5. **OpenAI SDK passthrough**: Not self-contained, loses integration

## Consequences
- OpenAI models work with same config, zero auth changes
- `openai.*` prefix detection triggers routing automatically
- VertexAI `ModelCategory` pattern proven — minimal architectural risk
