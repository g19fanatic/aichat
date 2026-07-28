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
        // Track every message-level cache_control placement so we can enforce
        // Anthropic's hard limit of at most 4 cache_control blocks per request.
        // Each entry is (message_index, content_block_index, keep_priority) where a
        // LOWER priority number means "keep first". The tools & system breakpoints
        // are the stable 1h prefix and are ALWAYS kept (they are separate top-level
        // fields, not part of `messages`), so they are not tracked here but they DO
        // count toward the budget of 4.
        //   priority 0 = last history-turn boundary   (highest keep)
        //   priority 1 = the single kept content-block breakpoint
        //   priority 2 = last-message breakpoint
        //   priority 3 = intermediate stepping-stone  (dropped first)
        //   priority 4 = extra content-block breakpoints beyond the first kept one
        let mut cc_sites: Vec<(usize, usize, u8)> = Vec::new();
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
                    // Record the content-block breakpoints for the cap pass. Only the
                    // LAST breakpoint is a preferred keeper (priority 1); any earlier
                    // ones are "extra" content-block breakpoints (priority 4, dropped
                    // first among content blocks) so we retain at most one.
                    let breakpoint_block_indices: Vec<usize> = content_blocks
                        .iter()
                        .enumerate()
                        .filter(|(_, b)| b.get("cache_control").is_some())
                        .map(|(bi, _)| bi)
                        .collect();
                    if let Some(last_pos) = breakpoint_block_indices.len().checked_sub(1) {
                        for (pos, &bi) in breakpoint_block_indices.iter().enumerate() {
                            let priority = if pos == last_pos { 1u8 } else { 4u8 };
                            cc_sites.push((last_user_idx, bi, priority));
                        }
                    }
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
                // last-message breakpoint => keep priority 2
                cc_sites.push((last_idx, 0, 2));
            } else if body["messages"][last_idx]["content"].is_array() {
                let content_len = body["messages"][last_idx]["content"].as_array().map_or(0, |a| a.len());
                if content_len > 0 {
                    body["messages"][last_idx]["content"][content_len - 1]["cache_control"] = json!({"type": "ephemeral"});
                    // last-message breakpoint => keep priority 2
                    cc_sites.push((last_idx, content_len - 1, 2));
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
                        // last history-turn boundary => highest keep priority 0
                        cc_sites.push((last_asst_idx, 0, 0));
                    } else if body["messages"][last_asst_idx]["content"].is_array() {
                        let content_len = body["messages"][last_asst_idx]["content"]
                            .as_array()
                            .map_or(0, |a| a.len());
                        if content_len > 0 {
                            body["messages"][last_asst_idx]["content"][content_len - 1]["cache_control"] =
                                json!({"type": "ephemeral"});
                            // last history-turn boundary => highest keep priority 0
                            cc_sites.push((last_asst_idx, content_len - 1, 0));
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
                        // intermediate stepping-stone => lowest keep priority 3 (dropped first)
                        cc_sites.push((target_idx, 0, 3));
                    } else if body["messages"][target_idx]["content"].is_array() {
                        let content_len = body["messages"][target_idx]["content"]
                            .as_array()
                            .map_or(0, |a| a.len());
                        if content_len > 0 {
                            body["messages"][target_idx]["content"][content_len - 1]["cache_control"] =
                                json!({"type": "ephemeral"});
                            // intermediate stepping-stone => lowest keep priority 3 (dropped first)
                            cc_sites.push((target_idx, content_len - 1, 3));
                        }
                    }
                }
            }
        }

        // ---------------------------------------------------------------------
        // Enforce Anthropic's hard limit of at most 4 cache_control blocks per
        // request. Exceeding this yields a 400 ("at most 4 blocks with
        // cache_control may be provided"). We run a single deterministic capping
        // pass over ALL placed breakpoints.
        //
        // The tools + system breakpoints form the stable 1h prefix and are ALWAYS
        // kept (they are top-level fields, processed FIRST by Anthropic). They are
        // not tracked in `cc_sites` but they DO count toward the budget of 4.
        //
        // The message-level breakpoints in `cc_sites` are all 5m and are processed
        // AFTER the prefix, so keeping any subset of them can never place a longer
        // TTL after a shorter one — the non-increasing-TTL invariant holds by
        // construction. We keep the highest-priority message-level breakpoints
        // (lowest priority number) up to the remaining budget, and STRIP
        // cache_control from the rest.
        {
            const MAX_CACHE_BLOCKS: usize = 4;

            // Count the 1h prefix breakpoints (tools last element + any system block).
            let tools_has_cc = body
                .get("tools")
                .and_then(|t| t.as_array())
                .and_then(|a| a.last())
                .map_or(false, |t| t.get("cache_control").is_some());
            let system_has_cc = match body.get("system") {
                Some(Value::Array(blocks)) => {
                    blocks.iter().any(|b| b.get("cache_control").is_some())
                }
                Some(Value::Object(obj)) => obj.get("cache_control").is_some(),
                _ => false,
            };
            let prefix_count = (tools_has_cc as usize) + (system_has_cc as usize);
            let msg_budget = MAX_CACHE_BLOCKS.saturating_sub(prefix_count);

            // Deduplicate by (msg_idx, block_idx), keeping the BEST (lowest) priority
            // so a block targeted by multiple sites is counted once and kept if any
            // site wants it.
            let mut sites = cc_sites;
            sites.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
            sites.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

            if sites.len() > msg_budget {
                // Stable-sort by keep-priority (ascending = keep first). Ties retain
                // the earlier processing order established above.
                sites.sort_by_key(|&(_, _, prio)| prio);
                // Everything beyond the budget gets its cache_control stripped.
                for &(mi, bi, _) in sites.iter().skip(msg_budget) {
                    if let Some(block) = body["messages"][mi]["content"]
                        .get_mut(bi)
                        .and_then(|b| b.as_object_mut())
                    {
                        block.remove("cache_control");
                    }
                }
            }
        }
    }

    ensure_tool_use_result_pairing(&mut body);

    Ok(body)
}

/// Enforce Anthropic's tool_use/tool_result adjacency invariant.
///
/// Every assistant `tool_use` block MUST be answered by a `tool_result`
/// (matching `tool_use_id`, non-empty content) in the IMMEDIATELY-following
/// user message. Interrupted turns can leave dangling/empty results, and
/// conversation replay can leave orphan tool_results with no preceding
/// tool_use, both of which cause a 400. This pass repairs the assembled body
/// in place without reordering existing messages.
fn ensure_tool_use_result_pairing(body: &mut Value) {
    let msgs = match body.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m.clone(),
        None => return,
    };

    let mut out: Vec<Value> = Vec::with_capacity(msgs.len() + 2);
    let mut i = 0usize;
    while i < msgs.len() {
        let msg = &msgs[i];
        let required_ids = if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            collect_tool_use_ids(msg)
        } else {
            Vec::new()
        };

        if required_ids.is_empty() {
            // Non-assistant, or assistant with no tool_use.
            // If it's a user message reached here, its preceding message was
            // NOT an assistant-with-tool_use (that path consumes i+1 below),
            // so any tool_result it carries is an orphan -> strip it.
            let mut cleaned = msg.clone();
            if cleaned.get("role").and_then(|r| r.as_str()) == Some("user") {
                strip_all_tool_results(&mut cleaned);
            }
            out.push(cleaned);
            i += 1;
            continue;
        }

        // Assistant carries tool_use blocks: the next message must answer them.
        out.push(msg.clone());
        match msgs.get(i + 1) {
            Some(next) if next.get("role").and_then(|r| r.as_str()) == Some("user") => {
                let mut user = next.clone();
                repair_user_message(&required_ids, &mut user);
                out.push(user);
                i += 2;
            }
            _ => {
                // No following user message: synthesize one.
                let results: Vec<Value> =
                    required_ids.iter().map(|id| synth_tool_result(id)).collect();
                out.push(json!({ "role": "user", "content": results }));
                i += 1; // do NOT consume msgs[i+1]; it is unrelated.
            }
        }
    }

    if let Some(m) = body.get_mut("messages") {
        *m = Value::Array(out);
    }
}

/// Ordered list of `tool_use` ids in an assistant message's content.
fn collect_tool_use_ids(msg: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                if let Some(id) = b.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids
}

/// True if a tool_result's `content` is missing/empty/hollow.
fn is_hollow_content(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        _ => false,
    }
}

fn synth_tool_result(id: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": "[interrupted: no result captured]",
        "is_error": true,
    })
}

/// Ensure `user` answers every required id; drop orphan tool_results.
/// Non-tool_result blocks (e.g. text) are preserved in place.
fn repair_user_message(required_ids: &[String], user: &mut Value) {
    if user.get("content").and_then(|c| c.as_array()).is_none() {
        // No content array: replace with synthesized results wholesale.
        let results: Vec<Value> =
            required_ids.iter().map(|id| synth_tool_result(id)).collect();
        user["content"] = Value::Array(results);
        return;
    }
    let content = user
        .get_mut("content")
        .and_then(|c| c.as_array_mut())
        .expect("content array checked above");

    // 1. Drop orphan tool_results (tool_use_id not in required set).
    content.retain(|b| {
        if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
            match b.get("tool_use_id").and_then(|v| v.as_str()) {
                Some(tid) => required_ids.iter().any(|r| r == tid),
                None => false, // malformed tool_result -> drop
            }
        } else {
            true // preserve text and other block types
        }
    });

    // 2. Repair or synthesize each required id, in order.
    for id in required_ids {
        let pos = content.iter().position(|b| {
            b.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                && b.get("tool_use_id").and_then(|v| v.as_str()) == Some(id.as_str())
        });
        match pos {
            Some(idx) => {
                let existing = content[idx].get("content");
                if is_hollow_content(existing) {
                    content[idx]["content"] =
                        Value::String("[interrupted: no result captured]".to_string());
                    content[idx]["is_error"] = Value::Bool(true);
                }
            }
            None => content.push(synth_tool_result(id)),
        }
    }
}

/// Remove all tool_result blocks from a user message (orphans w/ no preceding tool_use).
fn strip_all_tool_results(msg: &mut Value) {
    if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
        content.retain(|b| b.get("type").and_then(|t| t.as_str()) != Some("tool_result"));
    }
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

    /// Count the TOTAL number of cache_control blocks emitted across the FIXED
    /// Anthropic processing order (tools -> system -> messages). This is the
    /// quantity Anthropic caps at 4.
    fn count_cache_control_blocks(body: &Value) -> usize {
        collect_ttl_sequence(body).len()
    }

    /// Build a worst-case body that (before the cap) would emit 5-6 cache_control
    /// blocks: extended-cache model (tools=1h, system=1h), 2 content-block
    /// breakpoints on the last user message, and a long multi-turn history
    /// (>10 history messages) so the last-message, history-turn, AND stepping-stone
    /// breakpoints all fire.
    fn build_over_limit_shape(model_name: &str) -> Value {
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
        // A system message + a long alternating history so history_end > 10 and the
        // stepping-stone breakpoint fires, followed by a final user turn.
        let mut messages = vec![Message::new(
            MessageRole::System,
            MessageContent::Text("system prompt".into()),
        )];
        // 14 alternating user/assistant history messages => history region well
        // beyond the >10 threshold used by the stepping-stone logic.
        for i in 0..14 {
            let role = if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            messages.push(Message::new(
                role,
                MessageContent::Text(format!("history message {i}")),
            ));
        }
        // Final current user turn (this is the one split into content blocks).
        messages.push(Message::new(
            MessageRole::User,
            MessageContent::Text("buffers + prompt".into()),
        ));
        let cache_content_blocks = vec![
            CacheContentBlock {
                field_name: "buffers".into(),
                text: "buffer text".into(),
                is_breakpoint: true,
            },
            CacheContentBlock {
                field_name: "prompt".into(),
                text: "prompt text".into(),
                is_breakpoint: true,
            },
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

    #[test]
    fn test_cache_control_blocks_capped_at_four() {
        // This worst-case shape would emit 5-6 cache_control blocks on the
        // pre-cap code (tools + system + 2 content-blocks + last-message +
        // history-turn + stepping-stone). The cap pass must reduce the TOTAL
        // to at most 4.
        let body = build_over_limit_shape("claude-opus-4-6");
        let count = count_cache_control_blocks(&body);
        assert!(
            count <= 4,
            "cache_control blocks must be capped at 4, got {count} in body {body}"
        );

        // The stable 1h prefix (tools + system) must survive the cap.
        let tools = body["tools"].as_array().unwrap();
        assert!(
            tools.last().unwrap().get("cache_control").is_some(),
            "tools breakpoint (1h prefix) must be preserved after capping"
        );
        assert!(
            body["system"][0].get("cache_control").is_some(),
            "system breakpoint (1h prefix) must be preserved after capping"
        );

        // The surviving TTL sequence must still be non-increasing.
        let seq = collect_ttl_sequence(&body);
        for w in seq.windows(2) {
            assert!(
                w[0] >= w[1],
                "capped TTL sequence must remain non-increasing, got {seq:?}"
            );
        }
        // First two entries are the 1h prefix.
        assert_eq!(seq.first().copied(), Some(3600), "first TTL must be 1h (tools)");
        assert_eq!(seq.get(1).copied(), Some(3600), "second TTL must be 1h (system)");
    }

    #[test]
    fn test_valid_pairing_unchanged() {
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "n", "input": {} } ] },
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "ok" } ] },
        ] });
        let before = body.clone();
        ensure_tool_use_result_pairing(&mut body);
        assert_eq!(body, before, "valid body must pass through unchanged");
    }

    #[test]
    fn test_dangling_tool_use_gets_synthesized_result() {
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "n", "input": {} } ] },
            { "role": "user", "content": [ { "type": "text", "text": "hi" } ] },
        ] });
        ensure_tool_use_result_pairing(&mut body);
        let user = &body["messages"][1]["content"];
        let tr = user
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["type"] == json!("tool_result") && b["tool_use_id"] == json!("t1"))
            .expect("synthesized tool_result for t1 must exist");
        assert_eq!(tr["is_error"], json!(true));
        assert!(!tr["content"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_empty_tool_result_treated_as_missing() {
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "n", "input": {} } ] },
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": "" } ] },
        ] });
        ensure_tool_use_result_pairing(&mut body);
        let tr = &body["messages"][1]["content"][0];
        assert_eq!(tr["is_error"], json!(true));
        assert!(!tr["content"].as_str().unwrap().trim().is_empty());
    }

    #[test]
    fn test_dangling_tool_use_no_following_user_appends_user() {
        let mut body = json!({ "messages": [
            { "role": "assistant", "content": [
                { "type": "tool_use", "id": "t1", "name": "n", "input": {} } ] },
        ] });
        ensure_tool_use_result_pairing(&mut body);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2, "a user message must be synthesized");
        assert_eq!(msgs[1]["role"], json!("user"));
        assert_eq!(msgs[1]["content"][0]["tool_use_id"], json!("t1"));
        assert_eq!(msgs[1]["content"][0]["is_error"], json!(true));
    }

    #[test]
    fn test_orphan_tool_result_dropped() {
        let mut body = json!({ "messages": [
            { "role": "user", "content": [
                { "type": "text", "text": "hi" },
                { "type": "tool_result", "tool_use_id": "ghost", "content": "x" } ] },
        ] });
        ensure_tool_use_result_pairing(&mut body);
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1, "orphan tool_result must be dropped");
        assert_eq!(content[0]["type"], json!("text"));
    }
}

