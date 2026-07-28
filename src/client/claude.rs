use super::*;

use crate::utils::strip_think_tag;

use anyhow::{bail, Context, Result};
use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::{json, Value};

const API_BASE: &str = "https://api.anthropic.com/v1";

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeConfig {
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub api_key_command: Option<String>,
    pub api_key_command_expires_in: Option<u64>,
    pub use_bearer_auth: Option<bool>,
    #[serde(default)]
    pub models: Vec<ModelData>,
    pub patch: Option<RequestPatch>,
    pub extra: Option<ExtraConfig>,
}

impl ClaudeClient {
    config_get_fn!(api_key, get_api_key);
    config_get_fn!(api_base, get_api_base);

    pub const PROMPTS: [PromptAction<'static>; 1] = [("api_key", "API Key", None)];
}

impl_client_trait!(
    ClaudeClient,
    (
        prepare_chat_completions,
        claude_chat_completions,
        claude_chat_completions_streaming
    ),
    (noop_prepare_embeddings, noop_embeddings),
    (noop_prepare_rerank, noop_rerank),
);

fn prepare_chat_completions(
    self_: &ClaudeClient,
    data: ChatCompletionsData,
) -> Result<RequestData> {
    let api_key = resolve_api_key(self_.name(), self_.get_api_key(), self_.config.api_key_command.as_deref(), self_.config.api_key_command_expires_in)?;
    let api_base = self_
        .get_api_base()
        .unwrap_or_else(|_| API_BASE.to_string());

    let url = format!("{}/messages", api_base.trim_end_matches('/'));
    let body = claude_build_chat_completions_body(data, &self_.model)?;

    let mut request_data = RequestData::new(url, body);

    request_data.header("anthropic-version", "2023-06-01");
    if self_.config.use_bearer_auth.unwrap_or(false) {
        request_data.bearer_auth(&api_key);
    } else {
        request_data.header("x-api-key", &api_key);
    }

    // Enable 1-hour extended cache TTL for newer Claude models
    // Note: user patches can override this header to combine multiple beta features
    if claude_supports_extended_cache(&self_.model) {
        request_data.header("anthropic-beta", "extended-cache-ttl-2025-04-11");
    }

    Ok(request_data)
}

pub async fn claude_chat_completions(
    builder: RequestBuilder,
    _model: &Model,
) -> Result<ChatCompletionsOutput> {
    let res = builder.send().await?;
    let status = res.status();
    let data: Value = response_to_json(res).await?;
    if !status.is_success() {
        catch_error(&data, status.as_u16())?;
    }
    debug!("non-stream-data: {data}");
    claude_extract_chat_completions(&data)
}

pub async fn claude_chat_completions_streaming(
    builder: RequestBuilder,
    handler: &mut SseHandler,
    _model: &Model,
) -> Result<()> {
    let mut function_name = String::new();
    let mut function_arguments = String::new();
    let mut function_id = String::new();
    let mut reasoning_state = 0;
    let handle = |message: SseMmessage| -> Result<bool> {
        let data: Value = serde_json::from_str(&message.data)?;
        debug!("stream-data: {data}");
        if let Some(typ) = data["type"].as_str() {
            match typ {
                "content_block_start" => {
                    if let (Some("tool_use"), Some(name), Some(id)) = (
                        data["content_block"]["type"].as_str(),
                        data["content_block"]["name"].as_str(),
                        data["content_block"]["id"].as_str(),
                    ) {
                        if !function_name.is_empty() {
                            let arguments: Value =
                                function_arguments.parse().with_context(|| {
                                    format!("Tool call '{function_name}' have non-JSON arguments '{function_arguments}'")
                                })?;
                            handler.tool_call(ToolCall::new(
                                function_name.clone(),
                                arguments,
                                Some(function_id.clone()),
                            ))?;
                        }
                        function_name = name.into();
                        function_arguments.clear();
                        function_id = id.into();
                    }
                }
                "content_block_delta" => {
                    if let Some(text) = data["delta"]["text"].as_str() {
                        handler.text(text)?;
                    } else if let Some(text) = data["delta"]["thinking"].as_str() {
                        if reasoning_state == 0 {
                            handler.text("<think>\n")?;
                            reasoning_state = 1;
                        }
                        handler.text(text)?;
                    } else if let (true, Some(partial_json)) = (
                        !function_name.is_empty(),
                        data["delta"]["partial_json"].as_str(),
                    ) {
                        function_arguments.push_str(partial_json);
                    }
                }
                "content_block_stop" => {
                    if reasoning_state == 1 {
                        handler.text("\n</think>\n\n")?;
                        reasoning_state = 0;
                    }
                    if !function_name.is_empty() {
                        let arguments: Value = if function_arguments.is_empty() {
                            json!({})
                        } else {
                            function_arguments.parse().with_context(|| {
                                format!("Tool call '{function_name}' have non-JSON arguments '{function_arguments}'")
                            })?
                        };
                        handler.tool_call(ToolCall::new(
                            function_name.clone(),
                            arguments,
                            Some(function_id.clone()),
                        ))?;
                    }
                }
                _ => {}
            }
        }
        Ok(false)
    };

    sse_stream(builder, handle).await
}

pub fn claude_build_chat_completions_body(
    data: ChatCompletionsData,
    model: &Model,
) -> Result<Value> {
    let ChatCompletionsData {
        mut messages,
        temperature,
        top_p,
        functions,
        stream,
        cache_content_blocks,
        cache_warm,
    } = data;

    let system_message = extract_system_message(&mut messages);

    let mut network_image_urls = vec![];

    let messages_len = messages.len();
    let messages: Vec<Value> = messages
        .into_iter()
        .enumerate()
        .flat_map(|(i, message)| {
            let Message { role, content } = message;
            match content {
                MessageContent::Text(text) if role.is_assistant() && i != messages_len - 1 => {
                    vec![json!({ "role": role, "content": strip_think_tag(&text) })]
                }
                MessageContent::Text(text) => vec![json!({
                    "role": role,
                    "content": text,
                })],
                MessageContent::Array(list) => {
                    let content: Vec<_> = list
                        .into_iter()
                        .map(|item| match item {
                            MessageContentPart::Text { text } => {
                                json!({"type": "text", "text": text})
                            }
                            MessageContentPart::ImageUrl {
                                image_url: ImageUrl { url },
                            } => {
                                if let Some((mime_type, data)) = url
                                    .strip_prefix("data:")
                                    .and_then(|v| v.split_once(";base64,"))
                                {
                                    json!({
                                        "type": "image",
                                        "source": {
                                            "type": "base64",
                                            "media_type": mime_type,
                                            "data": data,
                                        }
                                    })
                                } else {
                                    network_image_urls.push(url.clone());
                                    json!({ "url": url })
                                }
                            }
                        })
                        .collect();
                    vec![json!({
                        "role": role,
                        "content": content,
                    })]
                }
                MessageContent::ToolCalls(MessageContentToolCalls {
                    tool_results, text, ..
                }) => {
                    let mut assistant_parts = vec![];
                    let mut user_parts = vec![];
                    if !text.is_empty() {
                        assistant_parts.push(json!({
                            "type": "text",
                            "text": text,
                        }))
                    }
                    for tool_result in tool_results {
                        assistant_parts.push(json!({
                            "type": "tool_use",
                            "id": tool_result.call.id,
                            "name": tool_result.call.name,
                            "input": tool_result.call.arguments,
                        }));
                        user_parts.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_result.call.id,
                            "content": tool_result.output.to_string(),
                        }));
                    }
                    vec![
                        json!({
                            "role": "assistant",
                            "content": assistant_parts,
                        }),
                        json!({
                            "role": "user",
                            "content": user_parts,
                        }),
                    ]
                }
            }
        })
        .collect();

    if !network_image_urls.is_empty() {
        bail!(
            "The model does not support network images: {:?}",
            network_image_urls
        );
    }

    let mut body = json!({
        "model": model.real_name(),
        "messages": messages,
    });
    if let Some(v) = system_message {
        body["system"] = v.into();
    }
    if let Some(v) = model.max_tokens_param() {
        body["max_tokens"] = v.into();
    }
    // Cache warming: override max_tokens to 1 to minimize response cost.
    // The purpose is to prime the cache, not to get a useful response.
    if cache_warm {
        body["max_tokens"] = json!(1);
    }
    if let Some(v) = temperature {
        body["temperature"] = v.into();
    }
    if let Some(v) = top_p {
        body["top_p"] = v.into();
    }
    if stream {
        body["stream"] = true.into();
    }
    if let Some(functions) = functions {
        let mut tools: Vec<Value> = functions
            .iter()
            .map(|v| {
                json!({
                    "name": v.name,
                    "description": v.description,
                    "input_schema": v.parameters,
                })
            })
            .collect();
        tools.sort_by(|a, b| {
            a["name"].as_str().unwrap_or("").cmp(&b["name"].as_str().unwrap_or(""))
        });
        body["tools"] = json!(tools);
    }

    // Smart prompt caching: enable when there's a stable prefix worth caching
    let should_cache = body.get("system").is_some()
        || body["messages"].as_array().map_or(false, |m| m.len() > 1)
        || body.get("tools").is_some();
    if should_cache {
        // Determine the TTL applied to the system block. Anthropic processes
        // cache_control blocks in a FIXED order (tools -> system -> messages) and
        // requires the TTLs across that sequence to be MONOTONICALLY NON-INCREASING:
        // a longer TTL must NOT come after a shorter one. Because the tools
        // breakpoint precedes the system breakpoint, if the system block carries
        // ttl=1h then the tools breakpoint must ALSO carry ttl=1h (otherwise the
        // 1h would illegally "come after" the tools default 5m block, yielding a
        // 400 ValidationException). We propagate the longest TTL backward across
        // the ordered block list so earlier blocks are >= all later blocks.
        let system_cache_control = if claude_supports_extended_cache(model) {
            json!({"type": "ephemeral", "ttl": "1h"})
        } else {
            json!({"type": "ephemeral"})
        };
        // Add cache_control to last tool for tool-level caching. The tools block is
        // emitted FIRST, so it must carry a TTL >= the system block's TTL to keep
        // the tools -> system -> messages sequence non-increasing.
        if let Some(tools_arr) = body.get_mut("tools").and_then(|t| t.as_array_mut()) {
            if let Some(last_tool) = tools_arr.last_mut() {
                // The tools breakpoint is processed BEFORE the system block, so it
                // must carry a TTL >= the system block TTL. Stamp it with the same
                // (propagated) TTL used for the system block to keep the ordered
                // tools -> system -> messages TTL sequence monotonically non-increasing.
                last_tool["cache_control"] = system_cache_control.clone();
            }
        }
        // Explicit block-level cache_control on system message for better cache granularity
        if let Some(system_str) = body.get("system").and_then(|v| v.as_str()).map(|s| s.to_string()) {
            let cache_control = system_cache_control.clone();
            body["system"] = json!([{
                "type": "text",
                "text": system_str,
                "cache_control": cache_control
            }]);
        }
        // Split last user message into content blocks based on _cache_hints.
        // When cache_content_blocks are present, the user message text has been parsed into
        // named blocks. We replace the last user message content with an array of text blocks,
        // adding cache_control to blocks designated as breakpoints (is_breakpoint=true).
        // This enables per-field caching: stable fields (buffers, file_arguments) get cached
        // independently, while dynamic fields (active_buffer, prompt) are not cached.
        if !cache_content_blocks.is_empty() {
            if let Some(messages_arr) = body["messages"].as_array() {
                let msgs_len = messages_arr.len();
                if let Some(last_user_idx) = (0..msgs_len).rev()
                    .find(|&i| messages_arr[i]["role"] == "user")
                {
                    let content_blocks: Vec<Value> = cache_content_blocks.iter().map(|block| {
                        let mut obj = json!({
                            "type": "text",
                            "text": block.text,
                        });
                        if block.is_breakpoint {
                            obj["cache_control"] = json!({"type": "ephemeral"});
                        }
                        obj
                    }).collect();
                    body["messages"][last_user_idx]["content"] = json!(content_blocks);
                }
            }
        }
        // Explicit cache_control on last message for multi-turn conversation caching
        if let Some(messages_len) = body["messages"].as_array().map(|m| m.len()).filter(|&len| len > 1) {
            let last_idx = messages_len - 1;
            if let Some(content_str) = body["messages"][last_idx]["content"].as_str().map(|s| s.to_string()) {
                body["messages"][last_idx]["content"] = json!([{
                    "type": "text",
                    "text": content_str,
                    "cache_control": {"type": "ephemeral"}
                }]);
            } else if body["messages"][last_idx]["content"].is_array() {
                let content_len = body["messages"][last_idx]["content"].as_array().map_or(0, |a| a.len());
                if content_len > 0 {
                    body["messages"][last_idx]["content"][content_len - 1]["cache_control"] = json!({"type": "ephemeral"});
                }
            }
        }
        // Cache breakpoint at last history turn boundary
        // When multi-turn history messages are present (more than 2 messages from turns),
        // place cache_control on the last assistant message before the current user message.
        // This ensures the entire conversation history prefix is cached across requests,
        // so each new turn only pays for new content after this boundary.
        if let Some(messages_arr) = body["messages"].as_array() {
            let msgs_len = messages_arr.len();
            // Need at least 4 messages: history user + history assistant + ... + current user
            if msgs_len > 3 {
                // Find the last assistant message before the final message
                if let Some(last_asst_idx) = (0..msgs_len - 1)
                    .rev()
                    .find(|&i| messages_arr[i]["role"] == "assistant")
                {
                    if let Some(content_str) = body["messages"][last_asst_idx]["content"]
                        .as_str()
                        .map(|s| s.to_string())
                    {
                        body["messages"][last_asst_idx]["content"] = json!([{
                            "type": "text",
                            "text": content_str,
                            "cache_control": {"type": "ephemeral"}
                        }]);
                    } else if body["messages"][last_asst_idx]["content"].is_array() {
                        let content_len = body["messages"][last_asst_idx]["content"]
                            .as_array()
                            .map_or(0, |a| a.len());
                        if content_len > 0 {
                            body["messages"][last_asst_idx]["content"][content_len - 1]["cache_control"] =
                                json!({"type": "ephemeral"});
                        }
                    }
                }
            }
        }
        // Intermediate stepping-stone breakpoint for long conversations
        // When multi-turn history is present, place the stepping stone within the
        // history region rather than across all messages. This creates layered cache
        // hits: early history stays cached even as new turns push the boundary forward.
        if let Some(messages_len) = body["messages"].as_array().map(|m| m.len()).filter(|&len| len > 3) {
            // Find the history boundary: last assistant message before the final message
            let history_end = body["messages"].as_array()
                .and_then(|arr| (0..messages_len - 1).rev()
                    .find(|&i| arr[i]["role"] == "assistant"))
                .unwrap_or(0);
            // Only place stepping stone if history region is substantial (>10 messages)
            if history_end > 10 {
                let mid_idx = history_end / 2;
                // Find a user message near the midpoint within history
                let target_idx = (mid_idx..history_end)
                    .find(|&i| body["messages"][i]["role"] == "user")
                    .or_else(|| (0..mid_idx).rev()
                        .find(|&i| body["messages"][i]["role"] == "user"))
                    .unwrap_or(mid_idx);
                // Only place if within history bounds and not on a message with existing breakpoint
                if target_idx < history_end && target_idx < messages_len - 1 {
                    if let Some(content_str) = body["messages"][target_idx]["content"]
                        .as_str()
                        .map(|s| s.to_string())
                    {
                        body["messages"][target_idx]["content"] = json!([{
                            "type": "text",
                            "text": content_str,
                            "cache_control": {"type": "ephemeral"}
                        }]);
                    } else if body["messages"][target_idx]["content"].is_array() {
                        let content_len = body["messages"][target_idx]["content"]
                            .as_array()
                            .map_or(0, |a| a.len());
                        if content_len > 0 {
                            body["messages"][target_idx]["content"][content_len - 1]["cache_control"] =
                                json!({"type": "ephemeral"});
                        }
                    }
                }
            }
        }
    }

    Ok(body)
}

pub fn claude_extract_chat_completions(data: &Value) -> Result<ChatCompletionsOutput> {
    let mut text = String::new();
    let mut reasoning = None;
    let mut tool_calls = vec![];
    if let Some(list) = data["content"].as_array() {
        for item in list {
            match item["type"].as_str() {
                Some("thinking") => {
                    if let Some(v) = item["thinking"].as_str() {
                        reasoning = Some(v.to_string());
                    }
                }
                Some("text") => {
                    if let Some(v) = item["text"].as_str() {
                        if !text.is_empty() {
                            text.push_str("\n\n");
                        }
                        text.push_str(v);
                    }
                }
                Some("tool_use") => {
                    if let (Some(name), Some(input), Some(id)) = (
                        item["name"].as_str(),
                        item.get("input"),
                        item["id"].as_str(),
                    ) {
                        tool_calls.push(ToolCall::new(
                            name.to_string(),
                            input.clone(),
                            Some(id.to_string()),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(reasoning) = reasoning {
        text = format!("<think>\n{reasoning}\n</think>\n\n{text}")
    }

    if text.is_empty() && tool_calls.is_empty() {
        bail!("Invalid response data: {data}");
    }

    let extra = {
        let cache_creation = data["usage"]["cache_creation_input_tokens"].as_u64();
        let cache_read = data["usage"]["cache_read_input_tokens"].as_u64();
        if cache_creation.is_some() || cache_read.is_some() {
            Some(json!({
                "cache_creation_input_tokens": cache_creation,
                "cache_read_input_tokens": cache_read,
            }))
        } else {
            None
        }
    };

    let output = ChatCompletionsOutput {
        text: text.to_string(),
        tool_calls,
        id: data["id"].as_str().map(|v| v.to_string()),
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
        extra,
    };
    Ok(output)
}

/// Determine if a Claude model supports the extended 1-hour cache TTL.
/// Returns true for Claude 4+ models (sonnet-4, opus-4, haiku-4, etc.)
/// that support the `extended-cache-ttl-2025-04-11` beta.
/// Keep in sync with bedrock_cache_ttl() in bedrock.rs.
fn claude_supports_extended_cache(model: &Model) -> bool {
    let name = model.name().to_lowercase();
    name.contains("4-5")
        || name.contains("4-6")
        || name.contains("opus-4")
        || name.contains("sonnet-4")
        || name.contains("haiku-4")
        || name.contains("sonnet-5")
        || name.contains("opus-5")
}


#[cfg(test)]
mod cache_ttl_tests {
    use super::*;
    use crate::config::CacheContentBlock;
    use crate::function::{FunctionDeclaration, JsonSchema};

    fn empty_schema() -> JsonSchema {
        JsonSchema {
            type_value: Some("object".into()),
            description: None,
            properties: None,
            items: None,
            any_of: None,
            enum_value: None,
            default: None,
            required: None,
        }
    }

    /// Build a request body that reproduces the exact failing payload shape:
    ///   - a system message (stamped ttl=1h for extended-cache models)
    ///   - a tools array whose LAST tool receives a cache_control breakpoint
    ///   - a user message split into content blocks with breakpoints (5m each)
    ///   - an extra multi-turn message so the "last message" breakpoint (5m) fires
    fn build_failing_shape(model_name: &str) -> Value {
        let functions = vec![
            FunctionDeclaration {
                name: "aaa_first_tool".into(),
                description: "first".into(),
                parameters: empty_schema(),
                agent: false,
            },
            FunctionDeclaration {
                name: "recent_tool_calls".into(),
                description: "last".into(),
                parameters: empty_schema(),
                agent: false,
            },
        ];
        // Provide multi-turn messages so message-level breakpoints are emitted.
        let messages = vec![
            Message::new(MessageRole::System, MessageContent::Text("system prompt".into())),
            Message::new(MessageRole::Assistant, MessageContent::Text("earlier reply".into())),
            Message::new(MessageRole::User, MessageContent::Text("buffers + prompt".into())),
        ];
        let cache_content_blocks = vec![
            CacheContentBlock { field_name: "buffers".into(), text: "buffer text".into(), is_breakpoint: true },
            CacheContentBlock { field_name: "prompt".into(), text: "prompt text".into(), is_breakpoint: true },
        ];
        let data = ChatCompletionsData {
            messages,
            temperature: None,
            top_p: None,
            functions: Some(functions),
            stream: false,
            cache_content_blocks,
            cache_warm: false,
        };
        let model = Model::new("claude", model_name);
        claude_build_chat_completions_body(data, &model).unwrap()
    }

    /// Map an Anthropic cache_control value to a numeric TTL in seconds.
    /// Missing/absent ttl defaults to the implicit 5-minute TTL (300s).
    fn ttl_seconds(cache_control: &Value) -> u64 {
        match cache_control.get("ttl").and_then(|v| v.as_str()) {
            Some("1h") => 3600,
            Some("5m") | None => 300,
            Some(other) => panic!("unexpected ttl value: {other}"),
        }
    }

    /// Collect the ordered list of cache_control TTLs across the FIXED Anthropic
    /// processing order: tools -> system -> messages.
    fn collect_ttl_sequence(body: &Value) -> Vec<u64> {
        let mut seq = Vec::new();
        // 1. tools (in array order; only breakpointed tools carry cache_control)
        if let Some(tools) = body.get("tools").and_then(|t| t.as_array()) {
            for tool in tools {
                if let Some(cc) = tool.get("cache_control") {
                    seq.push(ttl_seconds(cc));
                }
            }
        }
        // 2. system blocks
        if let Some(system) = body.get("system").and_then(|s| s.as_array()) {
            for block in system {
                if let Some(cc) = block.get("cache_control") {
                    seq.push(ttl_seconds(cc));
                }
            }
        }
        // 3. messages content blocks
        if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        if let Some(cc) = block.get("cache_control") {
                            seq.push(ttl_seconds(cc));
                        }
                    }
                }
            }
        }
        seq
    }

    #[test]
    fn test_cache_control_ttl_sequence_is_non_increasing() {
        // Extended-cache model => system stamped 1h. The tools breakpoint precedes
        // the system block, so it must also be 1h to keep the sequence
        // (tools -> system -> messages) monotonically non-increasing.
        let body = build_failing_shape("claude-opus-4-6");
        let seq = collect_ttl_sequence(&body);

        assert!(
            seq.len() >= 3,
            "expected tools + system + message breakpoints, got sequence {seq:?} from body {body}"
        );

        // Core invariant: TTLs must be monotonically non-increasing across the
        // fixed tools -> system -> messages order. This FAILS on the old code
        // (tools defaulted to 5m while system was 1h => 300 then 3600 => increase).
        for w in seq.windows(2) {
            assert!(
                w[0] >= w[1],
                "cache_control TTL sequence must be non-increasing (tools->system->messages), got {seq:?}"
            );
        }

        // Concrete assertion for the reconstructed failing payload: the tools
        // breakpoint must now be 1h and the system block must remain 1h.
        let tools = body["tools"].as_array().unwrap();
        let last_tool_cc = tools.last().unwrap().get("cache_control").unwrap();
        assert_eq!(ttl_seconds(last_tool_cc), 3600, "tools breakpoint must be stamped ttl=1h");

        let system_cc = body["system"][0].get("cache_control").unwrap();
        assert_eq!(ttl_seconds(system_cc), 3600, "system block must retain ttl=1h");

        // And the first two entries of the sequence (tools, system) are the 1h prefix,
        // with any later message breakpoints at 5m.
        assert_eq!(seq[0], 3600, "first (tools) TTL must be 1h");
        assert_eq!(seq[1], 3600, "second (system) TTL must be 1h");
    }

    #[test]
    fn test_non_extended_model_uniform_5m_still_non_increasing() {
        // Non-extended models never emit 1h; everything is implicit 5m, which is
        // trivially non-increasing and must not regress.
        let body = build_failing_shape("claude-3-haiku");
        let seq = collect_ttl_sequence(&body);
        for &t in &seq {
            assert_eq!(t, 300, "non-extended model must only emit 5m TTLs, got {seq:?}");
        }
        for w in seq.windows(2) {
            assert!(w[0] >= w[1], "sequence must be non-increasing: {seq:?}");
        }
    }
}

