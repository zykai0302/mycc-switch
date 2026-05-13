//! CodeBuddy OAuth Device Flow
//!
//! 实现 CodeBuddy 的 OAuth 设备码认证流程：
//! 1. POST /v2/plugin/auth/state — 获取 auth_state 和 authUrl
//! 2. GET /v2/plugin/auth/token?state={auth_state} — 轮询获取 accessToken
//! 3. 解析 JWT payload 提取用户信息

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// CodeBuddy API 基础 URL
const CODEBUDDY_BASE_URL: &str = "https://unvcoding.copilot.qq.com";

/// 认证状态端点
const AUTH_STATE_ENDPOINT: &str = "/v2/plugin/auth/state";

/// 认证令牌端点
const AUTH_TOKEN_ENDPOINT: &str = "/v2/plugin/auth/token";

/// CodeBuddy OAuth 错误
#[derive(Debug, thiserror::Error)]
pub enum CodeBuddyOAuthError {
    #[error("等待用户授权中")]
    AuthorizationPending,

    #[error("认证状态已过期")]
    ExpiredState,

    #[error("认证失败: {0}")]
    AuthFailed(String),

    #[error("网络错误: {0}")]
    NetworkError(String),

    #[error("解析错误: {0}")]
    ParseError(String),
}

impl From<reqwest::Error> for CodeBuddyOAuthError {
    fn from(err: reqwest::Error) -> Self {
        CodeBuddyOAuthError::NetworkError(err.to_string())
    }
}

/// 认证启动响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyAuthStartResponse {
    /// 认证状态 ID
    pub auth_state: String,
    /// 用户需访问的认证 URL
    pub verification_uri_complete: String,
    /// 令牌轮询端点
    pub token_endpoint: String,
    /// 有效期（秒）
    pub expires_in: u64,
    /// 轮询间隔（秒）
    pub interval: u64,
}

/// 认证轮询结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyAuthPollResult {
    /// Bearer Token
    pub bearer_token: String,
    /// Token 类型
    pub token_type: Option<String>,
    /// 有效时长（秒）
    pub expires_in: Option<i64>,
    /// Refresh Token
    pub refresh_token: Option<String>,
    /// Session State
    pub session_state: Option<String>,
    /// Scope
    pub scope: Option<String>,
    /// Domain
    pub domain: Option<String>,
    /// 用户 ID（从 JWT 提取）
    pub user_id: String,
    /// 用户信息（从 JWT 提取）
    pub user_info: serde_json::Value,
}

/// CodeBuddy API 响应通用格式
#[derive(Debug, Deserialize)]
struct CodeBuddyApiResponse<T> {
    code: i64,
    #[allow(dead_code)]
    msg: Option<String>,
    data: Option<T>,
}

/// Auth state 数据
#[derive(Debug, Deserialize)]
struct AuthStateData {
    state: String,
    #[serde(rename = "authUrl")]
    auth_url: String,
}

/// Auth token 数据
#[derive(Debug, Deserialize)]
struct AuthTokenData {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "tokenType")]
    token_type: Option<String>,
    #[serde(rename = "expiresIn")]
    expires_in: Option<i64>,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "sessionState")]
    session_state: Option<String>,
    scope: Option<String>,
    domain: Option<String>,
}

/// 生成随机 hex 字符串（使用 UUID v4 作为熵源）
fn random_hex(len: usize) -> String {
    let mut hex = String::with_capacity(len * 2);
    while hex.len() < len * 2 {
        let uuid = uuid::Uuid::new_v4();
        for byte in uuid.as_bytes() {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").unwrap();
            if hex.len() >= len * 2 {
                break;
            }
        }
    }
    hex.truncate(len * 2);
    hex
}

/// 构建认证启动请求头
fn auth_start_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("Content-Type", "application/json"),
        ("X-No-Authorization", "true"),
        ("X-No-User-Id", "true"),
        ("X-No-Enterprise-Id", "true"),
        ("X-No-Department-Info", "true"),
        ("X-Product", "unvcoding"),
        ("X-IDE-Type", "VSCode"),
        ("X-IDE-Name", "VSCode"),
        ("X-Domain", "unvcoding.copilot.qq.com"),
        ("User-Agent", "CodeBuddyIDE/4.2.17163875"),
    ]
}

/// 构建认证轮询请求头
fn auth_poll_headers() -> Vec<(&'static str, String)> {
    let trace_id = uuid::Uuid::new_v4().to_string();
    let span_id = uuid::Uuid::new_v4().to_string()[..16].to_string();

    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("X-No-Authorization", "true".to_string()),
        ("X-No-User-Id", "true".to_string()),
        ("X-No-Enterprise-Id", "true".to_string()),
        ("X-No-Department-Info", "true".to_string()),
        ("X-Product", "unvcoding".to_string()),
        ("X-IDE-Type", "VSCode".to_string()),
        ("X-IDE-Name", "VSCode".to_string()),
        ("X-Domain", "unvcoding.copilot.qq.com".to_string()),
        ("User-Agent", "CodeBuddyIDE/4.2.17163875".to_string()),
        ("b3", format!("{trace_id}-{span_id}-1")),
        ("X-B3-TraceId", trace_id.clone()),
        ("X-B3-ParentSpanId", trace_id),
        ("X-B3-SpanId", span_id),
        ("X-B3-Sampled", "1".to_string()),
    ];

    // 稳定排序：b3 必须在最后（某些代理要求）
    headers.sort_by(|a, b| {
        if a.0 == "b3" {
            std::cmp::Ordering::Greater
        } else if b.0 == "b3" {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    });

    headers
}

/// 启动 CodeBuddy 设备认证流程
pub async fn start_device_flow(
    base_url: Option<&str>,
) -> Result<CodeBuddyAuthStartResponse, CodeBuddyOAuthError> {
    let url_base = base_url.unwrap_or(CODEBUDDY_BASE_URL);
    let nonce = random_hex(16);
    let url = format!("{url_base}{AUTH_STATE_ENDPOINT}?platform=VSCode&nonce={nonce}");

    let client = Client::new();
    let headers = auth_start_headers();

    let mut request = client.post(&url);
    for (name, value) in &headers {
        request = request.header(*name, *value);
    }

    let body = serde_json::json!({ "nonce": nonce });
    let response = request.json(&body).send().await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(CodeBuddyOAuthError::NetworkError(format!(
            "认证启动请求失败: {status} - {text}"
        )));
    }

    let api_response: CodeBuddyApiResponse<AuthStateData> = response
        .json()
        .await
        .map_err(|e| CodeBuddyOAuthError::ParseError(e.to_string()))?;

    if api_response.code != 0 {
        return Err(CodeBuddyOAuthError::AuthFailed(format!(
            "认证启动失败: code={}, msg={}",
            api_response.code,
            api_response.msg.unwrap_or_default()
        )));
    }

    let data = api_response
        .data
        .ok_or_else(|| CodeBuddyOAuthError::ParseError("响应缺少 data 字段".to_string()))?;

    Ok(CodeBuddyAuthStartResponse {
        auth_state: data.state,
        verification_uri_complete: data.auth_url,
        token_endpoint: format!("{url_base}{AUTH_TOKEN_ENDPOINT}"),
        expires_in: 1800,
        interval: 5,
    })
}

/// 轮询认证状态，获取 Token
pub async fn poll_for_token(
    base_url: Option<&str>,
    auth_state: &str,
) -> Result<CodeBuddyAuthPollResult, CodeBuddyOAuthError> {
    let url_base = base_url.unwrap_or(CODEBUDDY_BASE_URL);
    let url = format!("{url_base}{AUTH_TOKEN_ENDPOINT}?state={auth_state}");

    let client = Client::new();
    let headers = auth_poll_headers();

    let mut request = client.get(&url);
    for (name, value) in &headers {
        request = request.header(*name, value.as_str());
    }

    let response = request.send().await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(CodeBuddyOAuthError::NetworkError(format!(
            "认证轮询请求失败: {status} - {text}"
        )));
    }

    let api_response: CodeBuddyApiResponse<AuthTokenData> = response
        .json()
        .await
        .map_err(|e| CodeBuddyOAuthError::ParseError(e.to_string()))?;

    // code 11217 = 用户尚未完成授权
    if api_response.code == 11217 {
        return Err(CodeBuddyOAuthError::AuthorizationPending);
    }

    if api_response.code != 0 {
        return Err(CodeBuddyOAuthError::AuthFailed(format!(
            "认证失败: code={}, msg={}",
            api_response.code,
            api_response.msg.unwrap_or_default()
        )));
    }

    let data = api_response
        .data
        .ok_or_else(|| CodeBuddyOAuthError::ParseError("响应缺少 data 字段".to_string()))?;

    // 解析 JWT 提取用户信息
    let (user_id, user_info) = parse_jwt_user_info(&data.access_token)
        .unwrap_or_else(|| ("unknown".to_string(), serde_json::json!({})));

    Ok(CodeBuddyAuthPollResult {
        bearer_token: data.access_token,
        token_type: data.token_type,
        expires_in: data.expires_in,
        refresh_token: data.refresh_token,
        session_state: data.session_state,
        scope: data.scope,
        domain: data.domain,
        user_id,
        user_info,
    })
}

/// 解析 JWT payload 提取用户信息（不验证签名）
///
/// 优先级：email > preferred_username > sub
fn parse_jwt_user_info(token: &str) -> Option<(String, serde_json::Value)> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }

    let payload_part = parts[1];

    // 修复 Base64URL padding
    let mut padded = payload_part.to_string();
    let missing_padding = padded.len() % 4;
    if missing_padding != 0 {
        for _ in 0..(4 - missing_padding) {
            padded.push('=');
        }
    }

    let decoded = URL_SAFE_NO_PAD
        .decode(&padded)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(&padded))
        .ok()?;

    let payload: serde_json::Value =
        serde_json::from_slice(&decoded).ok()?;

    // 提取 user_id：优先级 email > preferred_username > sub
    let user_id = payload
        .get("email")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("preferred_username").and_then(|v| v.as_str()))
        .or_else(|| payload.get("sub").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();

    // 构建用户信息
    let user_info = serde_json::json!({
        "sub": payload.get("sub").and_then(|v| v.as_str()),
        "email": payload.get("email").and_then(|v| v.as_str()),
        "preferred_username": payload.get("preferred_username").and_then(|v| v.as_str()),
        "name": payload.get("name").and_then(|v| v.as_str()),
        "given_name": payload.get("given_name").and_then(|v| v.as_str()),
        "family_name": payload.get("family_name").and_then(|v| v.as_str()),
        "exp": payload.get("exp").and_then(|v| v.as_i64()),
        "iat": payload.get("iat").and_then(|v| v.as_i64()),
        "scope": payload.get("scope").and_then(|v| v.as_str()),
        "session_state": payload.get("sid").and_then(|v| v.as_str()),
    });

    // 过滤 None 值
    let user_info = filter_null_values(user_info);

    Some((user_id, user_info))
}

/// 过滤 JSON 对象中的 null 值
fn filter_null_values(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, filter_null_values(v)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jwt_user_info() {
        // 创建一个简单的 JWT payload
        let payload = serde_json::json!({
            "sub": "user123",
            "email": "test@example.com",
            "preferred_username": "testuser",
            "name": "Test User",
            "iat": 1700000000,
            "exp": 1700003600
        });

        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(&payload_bytes);

        // 构造 JWT 格式的 token
        let token = format!("header.{encoded}.signature");

        let result = parse_jwt_user_info(&token);
        assert!(result.is_some());

        let (user_id, user_info) = result.unwrap();
        assert_eq!(user_id, "test@example.com"); // email 优先
        assert_eq!(user_info.get("email").and_then(|v| v.as_str()), Some("test@example.com"));
        assert_eq!(user_info.get("name").and_then(|v| v.as_str()), Some("Test User"));
    }

    #[test]
    fn test_parse_jwt_user_info_fallback_to_sub() {
        let payload = serde_json::json!({
            "sub": "user456"
        });

        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let encoded = URL_SAFE_NO_PAD.encode(&payload_bytes);
        let token = format!("header.{encoded}.signature");

        let result = parse_jwt_user_info(&token);
        assert!(result.is_some());

        let (user_id, _) = result.unwrap();
        assert_eq!(user_id, "user456");
    }

    #[test]
    fn test_parse_jwt_invalid_format() {
        assert!(parse_jwt_user_info("invalid").is_none());
        assert!(parse_jwt_user_info("").is_none());
    }

    #[test]
    fn test_filter_null_values() {
        let input = serde_json::json!({
            "a": "hello",
            "b": null,
            "c": 42,
            "d": {"x": null, "y": "world"}
        });

        let result = filter_null_values(input);
        assert!(result.get("a").is_some());
        assert!(result.get("b").is_none());
        assert!(result.get("c").is_some());
        assert!(result.get("d").unwrap().get("x").is_none());
        assert!(result.get("d").unwrap().get("y").is_some());
    }

    #[test]
    fn test_auth_start_headers() {
        let headers = auth_start_headers();
        assert!(headers.iter().any(|(k, _)| *k == "X-No-Authorization"));
        assert!(headers.iter().any(|(k, _)| *k == "X-Product"));
    }

    #[test]
    fn test_auth_poll_headers_includes_tracing() {
        let headers = auth_poll_headers();
        assert!(headers.iter().any(|(k, _)| *k == "b3"));
        assert!(headers.iter().any(|(k, _)| *k == "X-B3-TraceId"));
        assert!(headers.iter().any(|(k, _)| *k == "X-B3-Sampled"));
    }
}
