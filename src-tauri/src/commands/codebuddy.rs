//! CodeBuddy Tauri Commands
//!
//! 提供 CodeBuddy OAuth 设备流和凭证管理的 Tauri 命令

use crate::proxy::providers::codebuddy_auth::{CodeBuddyCredential, CodeBuddyCredentialManager};
use crate::proxy::providers::codebuddy_oauth;
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// CodeBuddy 凭证管理器状态（注入到 Tauri app）
pub struct CodeBuddyCredentialState(pub Arc<RwLock<CodeBuddyCredentialManager>>);

/// 启动 CodeBuddy OAuth 设备认证流程
#[tauri::command]
pub async fn codebuddy_start_auth() -> Result<codebuddy_oauth::CodeBuddyAuthStartResponse, String> {
    codebuddy_oauth::start_device_flow(None)
        .await
        .map_err(|e| e.to_string())
}

/// 轮询 CodeBuddy OAuth 认证状态
#[tauri::command]
pub async fn codebuddy_poll_auth(
    auth_state: String,
    credential_state: State<'_, CodeBuddyCredentialState>,
) -> Result<serde_json::Value, String> {
    let result = codebuddy_oauth::poll_for_token(None, &auth_state)
        .await
        .map_err(|e| match e {
            codebuddy_oauth::CodeBuddyOAuthError::AuthorizationPending => {
                "authorization_pending".to_string()
            }
            other => other.to_string(),
        })?;

    // 保存凭证到管理器
    let manager = credential_state.0.read().await;
    let cred = CodeBuddyCredential {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: result.user_id.clone(),
        bearer_token: result.bearer_token.clone(),
        refresh_token: result.refresh_token.clone(),
        expires_in: result.expires_in,
        created_at: chrono::Utc::now().timestamp(),
        user_info: serde_json::to_string(&result.user_info).unwrap_or_default(),
        is_expired: false,
        use_count: 0,
        sort_index: 0,
        created_at_db: chrono::Utc::now().timestamp(),
    };

    manager
        .add_credential(&cred)
        .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "user_id": result.user_id,
        "token_type": result.token_type,
        "expires_in": result.expires_in,
        "domain": result.domain,
        "user_info": result.user_info,
    }))
}

/// 列出所有 CodeBuddy 凭证（脱敏）
#[tauri::command]
pub async fn codebuddy_list_credentials(
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<Vec<crate::proxy::providers::codebuddy_auth::CodeBuddyCredentialDisplay>, String> {
    let manager = state.0.read().await;
    manager.list_credentials().map_err(|e| e.to_string())
}

/// 删除 CodeBuddy 凭证
#[tauri::command]
pub async fn codebuddy_remove_credential(
    id: String,
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager.remove_credential(&id).map_err(|e| e.to_string())
}

/// 设置手动选择的 CodeBuddy 凭证
#[tauri::command]
pub async fn codebuddy_set_manual_credential(
    id: Option<String>,
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .set_manual_credential(id)
        .await
        .map_err(|e| e.to_string())
}

/// 获取 CodeBuddy 凭证状态
#[tauri::command]
pub async fn codebuddy_get_credential_status(
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<crate::proxy::providers::codebuddy_auth::CodeBuddyCredentialStatus, String> {
    let manager = state.0.read().await;
    Ok(manager.get_status().await)
}

/// 切换 CodeBuddy 自动轮换
#[tauri::command]
pub async fn codebuddy_toggle_auto_rotation(
    enabled: bool,
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager.toggle_auto_rotation(enabled).await;
    Ok(())
}

/// 从目录导入 CodeBuddy 凭证
#[tauri::command]
pub async fn codebuddy_import_from_directory(
    path: String,
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<serde_json::Value, String> {
    let manager = state.0.read().await;
    let imported = manager
        .import_from_directory(&path)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({ "imported_count": imported }))
}

/// 登出 CodeBuddy（删除所有凭证）
#[tauri::command]
pub async fn codebuddy_logout(
    state: State<'_, CodeBuddyCredentialState>,
) -> Result<(), String> {
    let manager = state.0.read().await;
    manager
        .delete_all_credentials()
        .map_err(|e| e.to_string())
}
