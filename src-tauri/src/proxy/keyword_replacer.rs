//! Keyword Replacer for CodeBuddy Provider
//!
//! Replaces Claude/Anthropic branding with CodeBuddy/Tencent equivalents
//! in API responses. Only applied when the provider is CodeBuddy.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// Replacement rules (order-sensitive: longer matches first)
static REPLACEMENTS: &[(&str, &str)] = &[
    // Longest first to avoid partial matches
    ("Claude Code", "CodeBuddy Code"),
    ("Anthropic's official CLI for Claude", "Tencent's official CLI for CodeBuddy"),
    ("Claude", "CodeBuddy"),
    ("Anthropic", "Tencent"),
    ("https://github.com/anthropics/claude-code/issues", "https://cnb.cool/codebuddy/codebuddy-code/-/issues"),
];

/// Regex that matches any of the source keywords (longest first)
static KEYWORD_RE: Lazy<Regex> = Lazy::new(|| {
    let pattern = REPLACEMENTS
        .iter()
        .map(|(src, _)| regex::escape(src))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&pattern).expect("invalid keyword replacement regex")
});

/// Apply keyword replacement to a plain text string
pub fn apply_keyword_replacement(text: &str) -> String {
    KEYWORD_RE
        .replace_all(text, |caps: &regex::Captures| {
            let matched = caps.get(0).unwrap().as_str();
            for (src, dst) in REPLACEMENTS {
                if *src == matched {
                    return dst.to_string();
                }
            }
            matched.to_string()
        })
        .into_owned()
}

/// Apply keyword replacement to a response body (JSON)
///
/// Walks the JSON tree and applies text replacement to string values
/// inside content blocks of type "text".
pub fn apply_keyword_replacement_to_response(body: Value) -> Value {
    match body {
        Value::Object(map) => {
            let replaced: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| {
                    let rv = if k == "content" {
                        replace_in_content_array(v)
                    } else {
                        apply_keyword_replacement_to_response(v)
                    };
                    (k, rv)
                })
                .collect();
            Value::Object(replaced)
        }
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(apply_keyword_replacement_to_response)
                .collect(),
        ),
        Value::String(s) => Value::String(apply_keyword_replacement(&s)),
        other => other,
    }
}

/// Replace keywords in a content array (list of content blocks)
fn replace_in_content_array(value: Value) -> Value {
    match value {
        Value::Array(blocks) => Value::Array(
            blocks
                .into_iter()
                .map(|block| replace_in_content_block(block))
                .collect(),
        ),
        other => apply_keyword_replacement_to_response(other),
    }
}

/// Replace keywords in a single content block
fn replace_in_content_block(block: Value) -> Value {
    match &block {
        Value::Object(map) => {
            let block_type = map.get("type").and_then(|v| v.as_str());
            let is_text_block = block_type == Some("text");
            let is_tool_use_block = block_type == Some("tool_use");

            if is_text_block {
                if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                    let mut new_map = map.clone();
                    new_map.insert(
                        "text".to_string(),
                        Value::String(apply_keyword_replacement(text)),
                    );
                    return Value::Object(new_map);
                }
            }

            if is_tool_use_block {
                if let Some(id) = map.get("id").and_then(|v| v.as_str()) {
                    let converted = super::providers::convert_tool_call_id(id);
                    if converted != id {
                        let mut new_map = map.clone();
                        new_map.insert("id".to_string(), Value::String(converted));
                        // Also apply keyword replacement to nested fields
                        return apply_keyword_replacement_to_response(Value::Object(new_map));
                    }
                }
            }

            apply_keyword_replacement_to_response(block)
        }
        _ => apply_keyword_replacement_to_response(block),
    }
}

/// Apply keyword replacement and CodeBuddy tool call ID conversion to an Anthropic SSE stream.
///
/// Intercepts `text_delta` and `thinking_delta` events for keyword replacement,
/// and `content_block_start` events with `tool_use` type for ID conversion (tooluse_xxx → call_xxx).
/// Other SSE events pass through unchanged.
pub fn create_keyword_replacement_stream<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut stream = Box::pin(stream).peekable();
        let mut event_counter: u64 = 0;

        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(b) => b,
                Err(e) => {
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e.to_string()));
                    continue;
                }
            };

            let data = String::from_utf8_lossy(&bytes);
            event_counter += 1;

            // CodeBuddy 调试：记录 keyword_replacer 收到的上游 SSE 事件
            log::info!("[CodeBuddy-DBG] keyword_replacer IN  [#{event_counter}] {} bytes: {}", bytes.len(), data.trim_end());

            // Check if this SSE data line contains events that need processing
            let needs_keyword_replace = data.contains("\"text_delta\"")
                || data.contains("\"thinking_delta\"");
            let needs_tool_id_convert = data.contains("\"tool_use\"") && data.contains("tooluse_");

            if !needs_keyword_replace && !needs_tool_id_convert {
                // CodeBuddy 调试：记录 keyword_replacer 透传的事件
                log::info!("[CodeBuddy-DBG] keyword_replacer OUT [#{event_counter}] passthrough (no change needed)");
                yield Ok(bytes);
                continue;
            }

            // Try to parse and replace
            if let Some(replaced) = try_replace_in_sse_data(&data) {
                // CodeBuddy 调试：记录替换后的输出
                log::info!("[CodeBuddy-DBG] keyword_replacer OUT [#{event_counter}] replaced: {}", replaced.trim_end());
                yield Ok(Bytes::from(replaced));
            } else {
                // CodeBuddy 调试：记录未替换的输出
                log::info!("[CodeBuddy-DBG] keyword_replacer OUT [#{event_counter}] unchanged (parse ok, no match)");
                yield Ok(bytes);
            }
        }
    }
}

/// Try to apply keyword replacement and tool call ID conversion to an SSE data line.
/// Returns Some(replaced_string) if any change was applied, None if no change needed.
fn try_replace_in_sse_data(data: &str) -> Option<String> {
    // SSE format: "data: {json}\n" or "event: ...\ndata: {json}\n\n"
    // We need to find the JSON payload after "data: " and replace text in it
    let json_start = data.find("data: ")?;
    let json_str = &data[json_start + 6..];

    // Trim trailing whitespace/newlines for parsing
    let json_str_trimmed = json_str.trim_end();
    if json_str_trimmed.is_empty() || json_str_trimmed == "[DONE]" {
        return None;
    }

    let mut value: Value = match serde_json::from_str(json_str_trimmed) {
        Ok(v) => v,
        Err(_) => return None,
    };

    let mut changed = false;

    // Handle text_delta events — keyword replacement
    if let Some(delta) = value.get_mut("delta") {
        if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
            if let Some(text) = delta.get_mut("text").and_then(|t| t.as_str()) {
                let replaced = apply_keyword_replacement(text);
                if replaced != text {
                    *delta.get_mut("text").unwrap() = Value::String(replaced);
                    changed = true;
                }
            }
        } else if delta.get("type").and_then(|t| t.as_str()) == Some("thinking_delta") {
            if let Some(thinking) = delta.get_mut("thinking").and_then(|t| t.as_str()) {
                let replaced = apply_keyword_replacement(thinking);
                if replaced != thinking {
                    *delta.get_mut("thinking").unwrap() = Value::String(replaced);
                    changed = true;
                }
            }
        }
    }

    // Handle content_block_start with tool_use type — convert tooluse_xxx → call_xxx
    if let Some(content_block) = value.get_mut("content_block") {
        if content_block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
            if let Some(id) = content_block.get_mut("id").and_then(|i| i.as_str()) {
                let converted = super::providers::convert_tool_call_id(id);
                if converted != id {
                    *content_block.get_mut("id").unwrap() = Value::String(converted);
                    changed = true;
                }
            }
        }
    }

    if !changed {
        return None;
    }

    let new_json = serde_json::to_string(&value).unwrap_or_default();
    let prefix = &data[..json_start + 6];
    // Preserve trailing newlines
    let trailing: &str = &json_str[json_str_trimmed.len()..];
    Some(format!("{prefix}{new_json}{trailing}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_replacement() {
        assert_eq!(
            apply_keyword_replacement("Claude is great"),
            "CodeBuddy is great"
        );
    }

    #[test]
    fn test_anthropic_replacement() {
        assert_eq!(
            apply_keyword_replacement("Anthropic's official CLI for Claude"),
            "Tencent's official CLI for CodeBuddy"
        );
    }

    #[test]
    fn test_claude_code_replacement() {
        assert_eq!(
            apply_keyword_replacement("Claude Code is a tool"),
            "CodeBuddy Code is a tool"
        );
    }

    #[test]
    fn test_url_replacement() {
        assert_eq!(
            apply_keyword_replacement("Report at https://github.com/anthropics/claude-code/issues"),
            "Report at https://cnb.cool/codebuddy/codebuddy-code/-/issues"
        );
    }

    #[test]
    fn test_no_replacement_for_unrelated() {
        assert_eq!(
            apply_keyword_replacement("Hello world"),
            "Hello world"
        );
    }

    #[test]
    fn test_longest_match_first() {
        // "Claude Code" should match as a whole, not just "Claude"
        assert_eq!(
            apply_keyword_replacement("Use Claude Code today"),
            "Use CodeBuddy Code today"
        );
    }

    #[test]
    fn test_replace_in_response_text_block() {
        let body = json!({
            "content": [
                {"type": "text", "text": "Claude is made by Anthropic"}
            ]
        });

        let result = apply_keyword_replacement_to_response(body);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "CodeBuddy is made by Tencent");
    }

    #[test]
    fn test_replace_in_response_nested() {
        let body = json!({
            "id": "msg_123",
            "content": [
                {"type": "text", "text": "Welcome to Claude Code!"}
            ],
            "model": "claude-3"
        });

        let result = apply_keyword_replacement_to_response(body);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "Welcome to CodeBuddy Code!");
    }

    #[test]
    fn test_replace_in_response_tool_use_id_conversion() {
        let body = json!({
            "id": "msg_123",
            "content": [
                {"type": "tool_use", "id": "tooluse_abc123", "name": "Bash", "input": {}}
            ],
            "model": "claude-3"
        });

        let result = apply_keyword_replacement_to_response(body);
        assert_eq!(result["content"][0]["id"], "call_abc123");
    }

    #[test]
    fn test_replace_in_response_tool_use_id_already_call() {
        let body = json!({
            "id": "msg_123",
            "content": [
                {"type": "tool_use", "id": "call_abc123", "name": "Bash", "input": {}}
            ],
            "model": "claude-3"
        });

        let result = apply_keyword_replacement_to_response(body);
        assert_eq!(result["content"][0]["id"], "call_abc123");
    }

    #[test]
    fn test_replace_in_response_non_text_block_untouched_type() {
        let body = json!({
            "content": [
                {"type": "tool_use", "text": "Claude should not be replaced in tool_use blocks text field"}
            ]
        });

        let result = apply_keyword_replacement_to_response(body);
        // tool_use blocks still get replacement via the generic string walk
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("CodeBuddy"));
    }

    #[test]
    fn test_multiple_occurrences() {
        assert_eq!(
            apply_keyword_replacement("Claude and Anthropic make Claude Code"),
            "CodeBuddy and Tencent make CodeBuddy Code"
        );
    }

    #[test]
    fn test_try_replace_in_sse_data_text_delta() {
        let sse_data = "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Use Claude Code today\"}}\n\n";
        let result = try_replace_in_sse_data(sse_data).unwrap();
        assert!(result.contains("CodeBuddy Code"));
        assert!(!result.contains("\"text\":\"Use Claude Code today\""));
    }

    #[test]
    fn test_try_replace_in_sse_data_thinking_delta() {
        let sse_data = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Claude is thinking\"}}\n\n";
        let result = try_replace_in_sse_data(sse_data).unwrap();
        assert!(result.contains("CodeBuddy is thinking"));
    }

    #[test]
    fn test_try_replace_in_sse_data_no_replacement_needed() {
        let sse_data = "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello world\"}}\n\n";
        assert!(try_replace_in_sse_data(sse_data).is_none());
    }

    #[test]
    fn test_try_replace_in_sse_data_non_text_event() {
        let sse_data = "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\"}}\n\n";
        assert!(try_replace_in_sse_data(sse_data).is_none());
    }

    #[test]
    fn test_try_replace_in_sse_data_done_event() {
        let sse_data = "data: [DONE]\n\n";
        assert!(try_replace_in_sse_data(sse_data).is_none());
    }

    #[tokio::test]
    async fn test_create_keyword_replacement_stream() {
        use futures::StreamExt;

        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from("data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Use Claude Code\"}}\n\n")),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let stream = futures::stream::iter(chunks);
        let replaced = create_keyword_replacement_stream(stream);

        let results: Vec<_> = replaced.collect().await;
        assert_eq!(results.len(), 2);

        let first = String::from_utf8_lossy(&results[0].as_ref().unwrap());
        assert!(first.contains("CodeBuddy Code"));
        assert!(!first.contains("Use Claude Code"));
    }

    #[test]
    fn test_try_replace_in_sse_data_tool_use_id_conversion() {
        let sse_data = "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tooluse_abc123\",\"name\":\"Bash\"}}\n\n";
        let result = try_replace_in_sse_data(sse_data).unwrap();
        assert!(result.contains("call_abc123"));
        assert!(!result.contains("tooluse_abc123"));
    }

    #[test]
    fn test_try_replace_in_sse_data_tool_use_id_already_call() {
        let sse_data = "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call_abc123\",\"name\":\"Bash\"}}\n\n";
        // No tooluse_ prefix, so no change needed
        assert!(try_replace_in_sse_data(sse_data).is_none());
    }

    #[tokio::test]
    async fn test_create_keyword_replacement_stream_tool_id() {
        use futures::StreamExt;

        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from("data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tooluse_abc123\",\"name\":\"Bash\"}}\n\n")),
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let stream = futures::stream::iter(chunks);
        let replaced = create_keyword_replacement_stream(stream);

        let results: Vec<_> = replaced.collect().await;
        let first = String::from_utf8_lossy(&results[0].as_ref().unwrap());
        assert!(first.contains("call_abc123"));
        assert!(!first.contains("tooluse_abc123"));
    }
}
