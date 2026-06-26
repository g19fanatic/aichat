---
summary: "BedrockConfig 'profile' field in config.yaml is silently ignored — serde drops unknown fields"
created: 2026-06-02
updated: 2026-06-02
type: problem
scope: project
project: aichat
importance: medium
confidence: verified
status: in-progress
tags: [bedrock, config, serde, bug]
sources:
  - type: file
    path: src/client/bedrock.rs
    note: "BedrockConfig struct ~line 17-26 has no profile field"
---

# Bug: `profile:` Config Field Silently Ignored

## Problem
Users can set `profile: anduril-armory` in their bedrock client config YAML, but it has no effect.
The `BedrockConfig` struct in `src/client/bedrock.rs` does NOT have a `profile` field.
Serde's `#[derive(Deserialize)]` silently drops unknown fields.

## Current Behavior
```yaml
- type: bedrock
  name: aws-openai
  region: us-east-2
  profile: anduril-armory  # ← SILENTLY IGNORED
```

Credentials are actually sourced from:
1. `BEDROCK_AWS_PROFILE` environment variable (first priority)
2. `AWS_PROFILE` environment variable (fallback)

## Fix
Add `pub profile: Option<String>` to `BedrockConfig` struct and update `fetch_bedrock_creds_from_cli()` 
to use config profile as fallback after env vars:
```rust
let profile = std::env::var("BEDROCK_AWS_PROFILE")
    .or_else(|_| std::env::var("AWS_PROFILE"))
    .ok()
    .or_else(|| config_profile.map(|s| s.to_string()))?;
```

## Impact
Not critical (env vars work as workaround) but confusing for users who expect config to work.
Planned fix in Phase 2 of bedrock-mantle implementation.
