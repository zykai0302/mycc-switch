//! CodeBuddy (Tencent) Provider Adapter
//!
//! 支持腾讯 CodeBuddy API，使用 OpenAI Chat Completions 格式。
//!
//! ## 特性
//! - 凭证轮换：多账号 Bearer Token 轮流使用
//! - 专用请求头：X-Conversation-ID, X-Agent-Intent 等
//! - 消息格式转换：tool role → user role, toolUseId 校验
//! - 强制流式：CodeBuddy API 仅支持 stream=true
//! - 关键词替换：Claude → CodeBuddy, Anthropic → Tencent

use super::{AuthInfo, AuthStrategy, ProviderAdapter};
use crate::provider::Provider;
use crate::proxy::error::ProxyError;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::codebuddy_auth::CodeBuddyCredentialManager;

/// CodeBuddy 默认 API 端点
const CODEBUDDY_DEFAULT_BASE_URL: &str = "https://unvcoding.copilot.qq.com";

/// CodeBuddy API 路径
const CODEBUDDY_CHAT_PATH: &str = "/v2/chat/completions";

/// toolUseId 合法字符正则
static TOOL_USE_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"[^a-zA-Z0-9_-]").expect("invalid toolUseId regex")
});

/// CodeBuddy tool call ID 前缀 ("tooluse_" → "call_")
const CODEBUDDY_TOOL_ID_PREFIX: &str = "tooluse_";
const OPENAI_TOOL_ID_PREFIX: &str = "call_";

/// 转换 CodeBuddy 工具调用 ID 格式: tooluse_xxx → call_xxx
/// 与 Python 参考实现 OpenAICompatibilityConverter.convert_tool_call_id 一致
pub fn convert_tool_call_id(id: &str) -> String {
    if let Some(rest) = id.strip_prefix(CODEBUDDY_TOOL_ID_PREFIX) {
        format!("{OPENAI_TOOL_ID_PREFIX}{rest}")
    } else {
        id.to_string()
    }
}

/// CodeBuddy 适配器
pub struct CodeBuddyAdapter {
    pub credential_manager: Option<Arc<RwLock<CodeBuddyCredentialManager>>>,
}

impl CodeBuddyAdapter {
    pub fn new() -> Self {
        Self {
            credential_manager: None,
        }
    }

    pub fn with_credential_manager(
        mut self,
        manager: Arc<RwLock<CodeBuddyCredentialManager>>,
    ) -> Self {
        self.credential_manager = Some(manager);
        self
    }

    /// 从 Provider 配置中提取 base_url
    fn extract_base_url_from_config(&self, provider: &Provider) -> Option<String> {
        if let Some(env) = provider.settings_config.get("env") {
            if let Some(url) = env
                .get("CODEBUDDY_BASE_URL")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(url.trim_end_matches('/').to_string());
            }
            // 也检查 ANTHROPIC_BASE_URL（Claude 预设兼容）
            if let Some(url) = env
                .get("ANTHROPIC_BASE_URL")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                return Some(url.trim_end_matches('/').to_string());
            }
        }
        if let Some(url) = provider
            .settings_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(url.trim_end_matches('/').to_string());
        }
        None
    }

    /// 从凭证管理器获取下一个可用 token
    pub async fn get_next_token(&self) -> Option<String> {
        if let Some(manager) = &self.credential_manager {
            let mgr = manager.read().await;
            mgr.get_next_credential().await.map(|c| c.bearer_token.clone())
        } else {
            None
        }
    }

    /// 规范化消息格式（CodeBuddy 特有逻辑）
    ///
    /// 注意：此方法在 anthropic_to_openai 转换之后调用，因此 body 是 OpenAI 格式。
    /// OpenAI 格式中 tool result 消息使用 "tool" role + "tool_call_id" 字段。
    ///
    /// 1. tool role → user role 转换（CodeBuddy 不支持 tool role）
    /// 2. tool_call_id 格式修复：tooluse_xxx → call_xxx + 非法字符替换
    /// 3. 过滤含 API 错误文本的 assistant 消息
    /// 4. 确保至少 2 条消息
    /// 5. 强制 stream: true
    pub fn normalize_request(&self, mut body: Value) -> Value {
        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            let mut filtered_indices = Vec::new();

            for msg in messages.iter_mut() {
                // 1. tool role → user role
                if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
                    msg["role"] = Value::String("user".to_string());
                }

                // 2. tool_call_id 格式修复
                //    a) tooluse_xxx → call_xxx（CodeBuddy 特有格式转换）
                //    b) 非法字符替换
                if let Some(tool_call_id) = msg.get("tool_call_id").and_then(|v| v.as_str()) {
                    let converted = convert_tool_call_id(tool_call_id);
                    let fixed = TOOL_USE_ID_RE.replace_all(&converted, "_").to_string();
                    msg["tool_call_id"] = Value::String(fixed);
                }

                // 2b. assistant 消息中的 tool_calls ID 转换
                if let Some(tool_calls) = msg.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                    for tc in tool_calls.iter_mut() {
                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            let converted = convert_tool_call_id(id);
                            let fixed = TOOL_USE_ID_RE.replace_all(&converted, "_").to_string();
                            tc["id"] = Value::String(fixed);
                        }
                    }
                }

                // 3. 检测含 API 错误文本的 assistant 消息
                if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        let error_indicators = [
                            "overloaded",
                            "capacity",
                            "rate limit",
                            "api error",
                            "service unavailable",
                            "internal server error",
                        ];
                        let lower = content.to_lowercase();
                        if error_indicators
                            .iter()
                            .any(|indicator| lower.contains(indicator))
                            && content.len() < 200
                        {
                            filtered_indices.push(true);
                            continue;
                        }
                    }
                }
                filtered_indices.push(false);
            }

            // 移除被标记的消息
            let mut write_idx = 0;
            let msgs = std::mem::take(messages);
            for (i, mut msg) in msgs.into_iter().enumerate() {
                if i < filtered_indices.len() && filtered_indices[i] {
                    continue;
                }
                // 清理已转换消息中不需要的字段
                if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
                    msg.as_object_mut().map(|m| {
                        m.remove("tool_call_id");
                    });
                }
                messages.insert(write_idx, msg);
                write_idx += 1;
            }
            messages.truncate(write_idx);
        }

        // 4. 确保至少 2 条消息
        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            if messages.len() < 2 {
                while messages.len() < 2 {
                    messages.insert(
                        0,
                        serde_json::json!({
                            "role": "system",
                            "content": "You are a helpful coding assistant."
                        }),
                    );
                }
            }
        }

        // 5. 强制 stream: true
        body["stream"] = Value::Bool(true);

        body
    }
}

impl Default for CodeBuddyAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdapter for CodeBuddyAdapter {
    fn name(&self) -> &'static str {
        "CodeBuddy"
    }

    fn extract_base_url(&self, provider: &Provider) -> Result<String, ProxyError> {
        Ok(self
            .extract_base_url_from_config(provider)
            .unwrap_or_else(|| CODEBUDDY_DEFAULT_BASE_URL.to_string()))
    }

    fn extract_auth(&self, _provider: &Provider) -> Option<AuthInfo> {
        // CodeBuddy 使用凭证管理器动态提供 token
        // 返回占位符，实际 token 由 forwarder 从 credential_manager 获取后覆盖
        Some(AuthInfo::new(
            "codebuddy_placeholder".to_string(),
            AuthStrategy::CodeBuddy,
        ))
    }

    fn build_url(&self, base_url: &str, _endpoint: &str) -> String {
        format!(
            "{}{}",
            base_url.trim_end_matches('/'),
            CODEBUDDY_CHAT_PATH
        )
    }

    fn get_auth_headers(&self, auth: &AuthInfo) -> Vec<(http::HeaderName, http::HeaderValue)> {
        use http::{HeaderName, HeaderValue};

        let conversation_id = uuid::Uuid::new_v4().to_string();
        // 与 codebuddy2api 一致: X-Request-ID 和 X-Conversation-Message-ID 使用无横线 UUID
        let request_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        let message_id = uuid::Uuid::new_v4().to_string().replace('-', "");
        // 与 codebuddy2api 一致: X-Conversation-Request-ID 使用 secrets.token_hex(16) = 32位hex
        let conv_request_id = uuid::Uuid::new_v4().to_string().replace('-', "");

        let mut headers = vec![
            (
                HeaderName::from_static("authorization"),
                HeaderValue::from_str(&format!("Bearer {}", auth.api_key)).unwrap(),
            ),
            (
                HeaderName::from_static("x-conversation-id"),
                HeaderValue::from_str(&conversation_id).unwrap(),
            ),
            (
                HeaderName::from_static("x-conversation-request-id"),
                HeaderValue::from_str(&conv_request_id).unwrap(),
            ),
            (
                HeaderName::from_static("x-conversation-message-id"),
                HeaderValue::from_str(&message_id).unwrap(),
            ),
            (
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_str(&request_id).unwrap(),
            ),
            (
                HeaderName::from_static("x-agent-intent"),
                HeaderValue::from_static("craft"),
            ),
            (
                HeaderName::from_static("x-ide-type"),
                HeaderValue::from_static("CLI"),
            ),
            (
                HeaderName::from_static("x-ide-name"),
                HeaderValue::from_static("CLI"),
            ),
            (
                HeaderName::from_static("x-product"),
                HeaderValue::from_static("SaaS"),
            ),
            (
                HeaderName::from_static("user-agent"),
                HeaderValue::from_static("CLI/1.0.7 CodeBuddy/1.0.7"),
            ),
        ];

        // Stainless SDK 头 — 与 codebuddy2api Python 客户端保持一致
        let stainless_headers = [
            ("x-stainless-lang", "js"),
            ("x-stainless-package-version", "5.10.1"),
            ("x-stainless-os", "Windows"),
            ("x-stainless-arch", "x64"),
            ("x-stainless-runtime", "node"),
            ("x-stainless-runtime-version", "v22.13.1"),
            ("x-stainless-retry-count", "0"),
        ];

        for (name, value) in stainless_headers {
            headers.push((
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            ));
        }

        // X-Domain: 始终设为 CodeBuddy 主机名（与 codebuddy2api 一致）
        headers.push((
            HeaderName::from_static("x-domain"),
            HeaderValue::from_static("unvcoding.copilot.qq.com"),
        ));

        // X-User-Id: 优先从凭证中提取，否则使用默认值
        let user_id = auth
            .access_token
            .as_deref()
            .and_then(|extra| extra.split_once(':').map(|(_, uid)| uid.to_string()))
            .unwrap_or_else(|| "b5be3a67-237e-4ee6-9b9a-0b9ecd7b454b".to_string());
        headers.push((
            HeaderName::from_static("x-user-id"),
            HeaderValue::from_str(&user_id).unwrap(),
        ));

        headers
    }

    fn needs_transform(&self, _provider: &Provider) -> bool {
        true
    }

    fn transform_request(&self, body: Value, _provider: &Provider) -> Result<Value, ProxyError> {
        // CodeBuddy 需要 Anthropic → OpenAI Chat 格式转换
        // 1. 先将 Anthropic 格式转换为 OpenAI Chat 格式
        // 2. 再做 CodeBuddy 特有的规范化（tool role 转换、强制 stream 等）
        let openai_body = super::transform::anthropic_to_openai(body)?;
        Ok(self.normalize_request(openai_body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_codebuddy_default_base_url() {
        let adapter = CodeBuddyAdapter::new();
        assert_eq!(adapter.name(), "CodeBuddy");
    }

    #[test]
    fn test_build_url() {
        let adapter = CodeBuddyAdapter::new();
        let url = adapter.build_url("https://unvcoding.copilot.qq.com", "/v1/messages");
        assert_eq!(url, "https://unvcoding.copilot.qq.com/v2/chat/completions");
    }

    #[test]
    fn test_normalize_tool_role_to_user() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"},
                {"role": "tool", "content": "tool result", "tool_use_id": "call_123"}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        assert_eq!(messages[2]["role"], "user");
        assert!(result["stream"].as_bool().unwrap());
    }

    #[test]
    fn test_normalize_fix_tool_call_id() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "tool", "content": "result", "tool_call_id": "tooluse_abc!bad"}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        // tool role → user, tool_call_id removed from user messages after conversion
        // The tool_call_id conversion (tooluse_ → call_) happens during normalize,
        // then the field is stripped from user-role messages
        assert_eq!(messages[1]["role"], "user");
        // tool_call_id is stripped from user-role messages
        assert!(messages[1].get("tool_call_id").is_none());
    }

    #[test]
    fn test_normalize_tool_call_id_conversion_before_strip() {
        // Test that tooluse_ → call_ conversion works on assistant messages with tool_calls
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "tooluse_abc123", "type": "function", "function": {"name": "Bash"}}
                ]}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        // Assistant messages keep their tool_calls but with converted IDs
        let assistant_msg = &messages[1];
        assert_eq!(assistant_msg["role"], "assistant");
        assert_eq!(assistant_msg["tool_calls"][0]["id"], "call_abc123");
    }

    #[test]
    fn test_normalize_filter_error_messages() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "The API is overloaded. Please try again."},
                {"role": "assistant", "content": "I can help with that."}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"], "I can help with that.");
    }

    #[test]
    fn test_normalize_ensure_min_messages() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
    }

    #[test]
    fn test_normalize_force_stream() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "hi"}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        assert!(result["stream"].as_bool().unwrap());
    }

    #[test]
    fn test_convert_tool_call_id_tooluse_prefix() {
        assert_eq!(
            convert_tool_call_id("tooluse_abc123"),
            "call_abc123"
        );
    }

    #[test]
    fn test_convert_tool_call_id_no_prefix() {
        assert_eq!(
            convert_tool_call_id("call_abc123"),
            "call_abc123"
        );
    }

    #[test]
    fn test_convert_tool_call_id_already_call() {
        assert_eq!(
            convert_tool_call_id("call_xyz"),
            "call_xyz"
        );
    }

    #[test]
    fn test_normalize_tool_call_id_conversion_in_tool_result() {
        // tool role → user: tool_call_id is stripped from user-role messages,
        // but conversion happens first (important for processing before strip)
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "tool", "content": "result", "tool_call_id": "tooluse_abc123"}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        // tool role → user, tool_call_id stripped from user messages
        let tool_msg = &messages[1];
        assert_eq!(tool_msg["role"], "user");
        assert!(tool_msg.get("tool_call_id").is_none());
    }

    #[test]
    fn test_normalize_tool_call_id_conversion_in_assistant_tool_calls() {
        let adapter = CodeBuddyAdapter::new();
        let body = json!({
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "tooluse_abc123", "type": "function", "function": {"name": "Bash"}},
                    {"id": "tooluse_xyz789", "type": "function", "function": {"name": "Read"}}
                ]}
            ],
            "stream": false
        });

        let result = adapter.normalize_request(body);
        let messages = result.get("messages").unwrap().as_array().unwrap();

        let assistant_msg = &messages[1];
        assert_eq!(assistant_msg["tool_calls"][0]["id"], "call_abc123");
        assert_eq!(assistant_msg["tool_calls"][1]["id"], "call_xyz789");
    }
}
