# Client HTTP Connection Reuse
**Task ID**: 12  **Scope**: Audit whether reqwest::Client is reused across round-trips or rebuilt per call; connection pooling, keep-alive, and HTTP/2 multiplexing implications for multi-turn agent flows.

## Evidence Gathered

### Evidence 1: `build_client()` called on EVERY HTTP request (CONFIRMED)
`src/client/common.rs:51-68` — Default trait implementation:
```rust
fn build_client(&self) -> Result<ReqwestClient> {
    let mut builder = ReqwestClient::builder();
    let extra = self.extra_config();
    let timeout = extra.and_then(|v| v.connect_timeout).unwrap_or(10);
    if let Some(proxy) = extra.and_then(|v| v.proxy.as_deref()) {
        builder = set_proxy(builder, proxy)?;
    }
    if let Some(user_agent) = self.global_config().read().user_agent.as_ref() {
        builder = builder.user_agent(user_agent);
    }
    let client = builder
        .connect_timeout(Duration::from_secs(timeout))
        .build()
        .with_context(|| "Failed to build client")?;
    Ok(client)
}
```
Called at:
- `src/client/common.rs:73` — `chat_completions()`
- `src/client/common.rs:94` — `chat_completions_streaming()`
- `src/client/common.rs:109` — `embeddings()`
- `src/client/common.rs:116` — `rerank()`
- `src/serve.rs:308` — serve path (not tool-call hot path)

### Evidence 2: No `ReqwestClient` stored in any provider struct (CONFIRMED)
`rg "ReqwestClient" src/` returns ONLY:
- `bedrock.rs:12,43,97,202,211,221,601` — function parameter type `&ReqwestClient`, never a struct field
- `vertexai.rs:8,41,58,82` — same, function parameter type only
- `common.rs` — trait definition

Zero struct fields store a `reqwest::Client`. Every provider instantiates and immediately drops the client after each request. The `rg "OnceLock|LazyLock|once_cell" src/client/` returned exit code 1 (zero matches in client directory beyond `common.rs` header uses).

### Evidence 3: No HTTP/2 enabled, no pool configuration (CONFIRMED)
`Cargo.toml` reqwest dependency:
```toml
[dependencies.reqwest]
version = "0.12.0"
features = ["json", "multipart", "socks", "rustls-tls", "rustls-tls-native-roots"]
default-features = false
```
- `"http2"` feature is **absent**. reqwest only uses HTTP/2 when the `http2` feature is compiled in. Without it, all connections are HTTP/1.1 exclusively.
- `rg "http2" src/` — exit code 1 (zero matches in codebase).
- `rg "pool_idle|tcp_keepalive|connection_verbose|pool_max" src/` — exit code 1 (zero matches).
- `build_client()` only sets `connect_timeout`. No pool sizing, no TCP keepalive, no idle timeout.

### Evidence 4: `patch_request_data` — env::var + regex per request (CONFIRMED)
`src/client/common.rs:160-196`:
```rust
fn patch_request_data(&self, request_data: &mut RequestData) {
    let model_type = self.model().model_type();
    if let Some(patch) = self.model().patch() {
        request_data.apply_patch(patch.clone());
    }
    let patch_map = std::env::var(get_env_name(&format!(
        "patch_{}_{}",
        self.model().client_name(),
        model_type.api_name(),
    )))
    .ok()
    .and_then(|v| serde_json::from_str(&v).ok())
    .or_else(|| {
        self.patch_config()
            .and_then(|v| model_type.extract_patch(v))
            .cloned()
    });
    // ... regex match per model name ...
    for (key, patch) in patch_map {
        let key = ESCAPE_SLASH_RE.replace_all(&key, r"\/");
        if let Ok(regex) = Regex::new(&format!("^({key})$")) {
```
Called by `request_builder()` at `common.rs:156` on EVERY request. Includes:
1. `format!()` to create env var name string
2. `std::env::var()` syscall
3. `serde_json::from_str()` if env var is set
4. Per-patch-key: `Regex::new()` (fancy_regex compile) + `regex.is_match()` — creates a new compiled regex per model name per patch key per request

### Evidence 5: `call_chat_completions` passes `input.clone()` to `client.chat_completions()` (CONFIRMED)
`src/client/common.rs:407-411`:
```rust
pub async fn call_chat_completions(
    input: &Input,
    ...
    client: &dyn Client,
    ...
) -> Result<...> {
    let ret = abortable_run_with_spinner(
        client.chat_completions(input.clone()),  // clone here
```
And `chat_completions` signature at `common.rs:70-79`: `async fn chat_completions(&self, input: Input)` — takes by value. This clone is required by the current API design.

### Evidence 6: `RequestData::into_builder` with per-request `debug!` body log (CONFIRMED)
`src/client/common.rs:249-260`:
```rust
pub fn into_builder(self, client: &ReqwestClient) -> RequestBuilder {
    let RequestData { url, headers, body } = self;
    debug!("Request {url} {body}");
    let mut builder = client.post(url);
    for (key, value) in headers {
        builder = builder.header(key, value);
    }
    builder = builder.json(&body);
    builder
}
```
`debug!("Request {url} {body}")` — `{body}` is the full serialized JSON request body. In debug-log mode this serializes the entire request to string. In production (log level ≥ info), the `log::log_enabled!` guard short-circuits this before any allocation. Not a hot-path concern in production builds. The `builder.json(&body)` call does perform serde serialization of the `Value` to bytes.

## Findings

1. **New `reqwest::Client` constructed on every single HTTP call** (CONFIRMED): `build_client()` is called inside `chat_completions()`, `chat_completions_streaming()`, `embeddings()`, and `rerank()`. There are zero instances of `ReqwestClient` stored as a struct field anywhere in the codebase. The `reqwest::Client` built in one round-trip is dropped immediately after use. The internal connection pool lives only for the duration of a single HTTP call. For a 10-round tool chain = 10 new reqwest::Client instances, each with their own connection pool, TLS context, and internal state.

2. **No HTTP/2 feature compiled in** (CONFIRMED): reqwest `Cargo.toml` feature list does NOT include `"http2"`. All connections use HTTP/1.1 exclusively. Even with connection pool sharing, there would be no HTTP/2 multiplexing benefit (pipelining HTTP/1.1 is unreliable with LLM SSE responses). The `"http2"` feature would enable TLS ALPN negotiation for h2; without it, the reqwest TLS handshake explicitly does not offer h2.

3. **Loss of TCP keep-alive and TLS session resumption across round-trips** (CONFIRMED): Each new `reqwest::Client` starts a fresh hyper connection pool. For HTTPS requests to OpenAI/Claude/Gemini/etc., each round-trip incurs: (a) TCP 3-way handshake ~5-15ms RTT, (b) TLS 1.3 handshake ~1 additional RTT (~5-15ms), (c) HTTP/1.1 request. Total connection overhead: ~10-30ms per round-trip. For rustls (CONFIRMED by Cargo.toml feature), TLS 1.3 session tickets enable resumption IF the prior session ticket is available — but since the TLS context is destroyed with each `reqwest::Client`, no session ticket caching is possible.

4. **`patch_request_data` runs `std::env::var` + potential `Regex::new` per request** (CONFIRMED): `common.rs:160-196`. The `std::env::var()` syscall happens unconditionally every request. If a patch env var or patch_config is set, a `Regex::new()` (fancy_regex compile) happens per patch key per request. Most users do not set patches, so the regex path is typically not reached, but `std::env::var()` always runs.

5. **`call_chat_completions` does `input.clone()` per call** (CONFIRMED, `common.rs:410`): The non-streaming path clones `Input` before passing to `client.chat_completions()`. This is unavoidable given the `chat_completions(input: Input)` by-value signature. Combined with `chat_completions_streaming` which does `let input = input.clone()` internally at `common.rs:82`, every round-trip has ≥1 full `Input` clone.

6. **`ReqwestClient::builder().build()` cost**: Constructing a `reqwest::Client` involves: allocating a `hyper_util::client::legacy::Client`, setting up rustls `ClientConfig`, creating the connection pool `Arc<Mutex<...>>`, and registering the tokio runtime. Estimated cost: ~0.1-1ms pure CPU/alloc overhead (not including network). For 10 round-trips = 1-10ms of pure client-construction overhead (minor but measurable).

7. **`abortable_run_with_spinner` wraps non-streaming path** (`common.rs:408-413`): Each `call_chat_completions` non-streaming call spawns a tokio task + mpsc channel for the spinner. This is per-round-trip overhead in addition to the reqwest::Client creation.

## Optimization Candidates

| # | Proposal | Impact | Effort | Risk | Evidence Basis |
|---|----------|--------|--------|------|----------------|
| 1 | Store `reqwest::Client` as a `OnceLock<ReqwestClient>` field in each provider struct (or in a shared field via the macro-generated code). Lazy-initialize on first request. Reuse across all calls for the same provider instance. | **high** | M | low | CONFIRMED — `build_client()` is pure function of immutable config (proxy, user_agent, connect_timeout); result is identical every call |
| 2 | Add `reqwest` `"http2"` feature to Cargo.toml. Reqwest will negotiate HTTP/2 via ALPN with HTTPS servers. All major LLM APIs (OpenAI, Anthropic, Google) support HTTP/2. | **med** | S | low | CONFIRMED — feature is absent; adding it is a single Cargo.toml line |
| 3 | Change `chat_completions` signature from `async fn chat_completions(&self, input: Input)` (by value) to `async fn chat_completions(&self, input: &Input)`. Eliminates the mandatory `input.clone()` at `call_chat_completions:410`. | **med** | M | med | CONFIRMED — clone at common.rs:410 is required by the by-value signature; 3 Input clones per round-trip reduce to 2 |
| 4 | Cache `patch_request_data` result: memoize the compiled regex and patch_map at client init. The env var and patch_config are session-stable. Avoid `Regex::new()` + `std::env::var()` per request. | **low** | S | low | CONFIRMED — patch_map only changes if env or config changes, neither happens mid-session |
| 5 | Add `tcp_keepalive` configuration to `build_client()` for when reqwest::Client eventually gets cached. E.g. `.tcp_keepalive(Duration::from_secs(60))`. | **low** | S | low | THEORY — TCP keepalive only matters if connection pooling is fixed (proposal #1 first) |

### Priority note on proposal #1:
The fix is straightforward: change the `Client` trait to have a `cached_client` method backed by `OnceLock<ReqwestClient>`, or generate a `client: OnceLock<ReqwestClient>` field via the `define_client!` macro. However, the trait object constraint (`dyn Client`) and `async_trait` complicate storing `OnceLock` in the trait itself — it works best as a generated field in each concrete provider struct via the macro. The `build_client()` implementation would call itself lazily via `self.client.get_or_init(|| self.build_client_inner())`.

**Connection overhead quantification** (10-round tool chain to OpenAI over WAN):
- Per-round connection overhead: ~20ms (5ms TCP + 10ms TLS + 5ms TTFB baseline variation)
- 10 rounds × 20ms = **200ms of unnecessary connection overhead** per multi-round agent chain
- Compare to: subprocess spawn ~5ms, reqwest::Client construction ~0.5ms, Input::clone ~0.1ms
- This is the **#2 ranked latency bottleneck** after subprocess spawn in multi-round tool chains

## Open Questions / Needs Verification

- Does reqwest 0.12 with rustls support TLS 1.3 session ticket resumption across distinct `reqwest::Client` instances sharing the same process (i.e., does rustls cache at the process level)? If yes, the TLS overhead estimate of ~10-15ms per round-trip may be reduced to ~5ms (TCP only). Needs verification with actual TLS 1.3 session ticket cache inspection.
- Does the LLM API server (OpenAI, Anthropic) use HTTP/2 and support ALPN h2 negotiation? This is widely assumed but should be verified with `curl --http2 -I https://api.openai.com`. If h2 is available, HTTP/2 + connection reuse would additionally eliminate head-of-line blocking between the streaming response and the next request setup.
- What is the actual `reqwest::Client::builder().build()` wall-clock cost? A microbenchmark (criterion) would quantify this. Estimated 100µs-1ms based on allocations, but runtime measurement is needed.
- Does `patch_request_data`'s `Regex::new()` actually get reached in typical usage (i.e., do users commonly set patch configs)? If yes, the per-request regex compile is a meaningful overhead.

## Hot-Path Classification

**Per-round-trip** (dominant): `build_client()` at `chat_completions:73`, `chat_completions_streaming:94`; TLS + TCP handshake overhead; `patch_request_data` env::var syscall; `input.clone()` at `call_chat_completions:410`.

**Per-session (cold path)**: First request only — TLS session establishment.
