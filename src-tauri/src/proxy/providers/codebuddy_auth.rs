//! CodeBuddy Credential Manager
//!
//! 管理 CodeBuddy API 的 Bearer Token 凭证，支持：
//! - 多账号凭证存储（SQLite）
//! - 轮转轮换（可配置轮换次数）
//! - Token 过期追踪（300s buffer）
//! - 手动选择凭证
//! - 从 .codebuddy_creds/ 目录导入（迁移支持）

use crate::database::Database;
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Token 过期提前量（秒）
const TOKEN_EXPIRY_BUFFER_SECS: i64 = 300;

/// CodeBuddy 凭证
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyCredential {
    /// 唯一 ID
    pub id: String,
    /// 用户 ID（email 或 preferred_username）
    pub user_id: String,
    /// Bearer Token
    pub bearer_token: String,
    /// Refresh Token
    pub refresh_token: Option<String>,
    /// Token 有效时长（秒）
    pub expires_in: Option<i64>,
    /// Token 创建时间（Unix 秒）
    pub created_at: i64,
    /// 用户信息 JSON
    pub user_info: String,
    /// 是否已过期
    pub is_expired: bool,
    /// 使用次数
    pub use_count: u32,
    /// 排序索引
    pub sort_index: i32,
    /// 数据库创建时间
    pub created_at_db: i64,
}

impl CodeBuddyCredential {
    /// 检查 Token 是否已过期（含 300s buffer）
    pub fn is_token_expired(&self) -> bool {
        if self.is_expired {
            return true;
        }
        match (self.expires_in, self.created_at) {
            (Some(expires_in), created_at) if created_at > 0 && expires_in > 0 => {
                let now = chrono::Utc::now().timestamp();
                now >= (created_at + expires_in - TOKEN_EXPIRY_BUFFER_SECS)
            }
            _ => false,
        }
    }

    /// 返回脱敏后的 bearer_token（用于前端展示）
    pub fn masked_token(&self) -> String {
        if self.bearer_token.chars().count() > 12 {
            let prefix: String = self.bearer_token.chars().take(6).collect();
            let suffix: String = self
                .bearer_token
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!("{prefix}...{suffix}")
        } else {
            "***".to_string()
        }
    }

    /// 返回给前端的脱敏凭证
    pub fn to_display(&self) -> CodeBuddyCredentialDisplay {
        CodeBuddyCredentialDisplay {
            id: self.id.clone(),
            user_id: self.user_id.clone(),
            bearer_token: self.masked_token(),
            refresh_token: self.refresh_token.as_ref().map(|_| "***".to_string()),
            expires_in: self.expires_in,
            created_at: self.created_at,
            user_info: self.user_info.clone(),
            is_expired: self.is_token_expired(),
            use_count: self.use_count,
            sort_index: self.sort_index,
        }
    }
}

/// 前端展示用的脱敏凭证
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyCredentialDisplay {
    pub id: String,
    pub user_id: String,
    pub bearer_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub created_at: i64,
    pub user_info: String,
    pub is_expired: bool,
    pub use_count: u32,
    pub sort_index: i32,
}

/// 凭证管理器状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeBuddyCredentialStatus {
    /// 凭证总数
    pub total_count: usize,
    /// 有效（未过期）凭证数
    pub active_count: usize,
    /// 当前使用索引
    pub current_index: usize,
    /// 自动轮换是否启用
    pub auto_rotation_enabled: bool,
    /// 轮换次数
    pub rotation_count: u32,
    /// 手动选择的凭证 ID
    pub manual_selected_id: Option<String>,
}

/// CodeBuddy 凭证管理器
pub struct CodeBuddyCredentialManager {
    db: Arc<Database>,
    current_index: Mutex<usize>,
    rotation_count: Mutex<u32>,
    auto_rotation_enabled: Mutex<bool>,
    manual_selected_id: Mutex<Option<String>>,
}

impl CodeBuddyCredentialManager {
    /// 创建新的凭证管理器
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            current_index: Mutex::new(0),
            rotation_count: Mutex::new(1),
            auto_rotation_enabled: Mutex::new(true),
            manual_selected_id: Mutex::new(None),
        }
    }

    /// 获取下一个可用凭证（轮转逻辑）
    pub async fn get_next_credential(&self) -> Option<CodeBuddyCredential> {
        let all_creds = match self.list_credentials_inner() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[CodeBuddyAuth] 查询凭证失败: {e}");
                return None;
            }
        };

        // 过滤未过期的凭证（收集 owned 值以便返回）
        let valid_creds: Vec<CodeBuddyCredential> =
            all_creds.into_iter().filter(|c| !c.is_token_expired()).collect();

        if valid_creds.is_empty() {
            log::warn!("[CodeBuddyAuth] 没有可用的有效凭证");
            return None;
        }

        // 手动选择优先
        let manual_id = self.manual_selected_id.lock().await.clone();
        if let Some(ref id) = manual_id {
            if let Some(cred) = valid_creds.iter().find(|c| c.id == *id) {
                return Some(cred.clone());
            }
            // 手动选择的已过期或不存在，清除选择
            log::warn!("[CodeBuddyAuth] 手动选择的凭证 {id} 不可用，清除选择");
            let mut mid = self.manual_selected_id.lock().await;
            *mid = None;
        }

        let auto_enabled = *self.auto_rotation_enabled.lock().await;
        let rotation_count = *self.rotation_count.lock().await;
        let mut current_index = self.current_index.lock().await;

        if !auto_enabled || rotation_count == 0 {
            // 不轮换：固定使用当前凭证
            let idx = *current_index % valid_creds.len();
            return Some(valid_creds[idx].clone());
        }

        // 轮换逻辑
        let valid_position = *current_index % valid_creds.len();
        let cred = valid_creds[valid_position].clone();

        // 更新使用次数
        let new_use_count = cred.use_count + 1;
        if let Err(e) = self.update_use_count(&cred.id, new_use_count) {
            log::warn!("[CodeBuddyAuth] 更新使用次数失败: {e}");
        }

        // 达到轮换次数，切换到下一个
        if new_use_count >= rotation_count {
            let next_position = (valid_position + 1) % valid_creds.len();
            *current_index = next_position;
            if let Err(e) = self.update_use_count(&cred.id, 0) {
                log::warn!("[CodeBuddyAuth] 重置使用次数失败: {e}");
            }
        }

        Some(cred)
    }

    /// 添加凭证
    pub fn add_credential(&self, cred: &CodeBuddyCredential) -> Result<(), AppError> {
        let conn = self.db.conn.lock().map_err(|e| {
            AppError::Database(format!("Mutex lock failed: {}", e))
        })?;

        conn.execute(
            "INSERT OR REPLACE INTO codebuddy_credentials
             (id, user_id, bearer_token, refresh_token, expires_in, created_at,
              user_info, is_expired, use_count, sort_index, created_at_db)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                cred.id,
                cred.user_id,
                cred.bearer_token,
                cred.refresh_token,
                cred.expires_in,
                cred.created_at,
                cred.user_info,
                cred.is_expired as i32,
                cred.use_count,
                cred.sort_index,
                cred.created_at_db,
            ],
        )
        .map_err(|e| AppError::Database(format!("添加 CodeBuddy 凭证失败: {e}")))?;

        log::info!("[CodeBuddyAuth] 添加凭证成功: {} ({})", cred.user_id, cred.id);
        Ok(())
    }

    /// 删除凭证
    pub fn remove_credential(&self, id: &str) -> Result<(), AppError> {
        let conn = self.db.conn.lock().map_err(|e| {
            AppError::Database(format!("Mutex lock failed: {}", e))
        })?;

        conn.execute(
            "DELETE FROM codebuddy_credentials WHERE id = ?1",
            params![id],
        )
        .map_err(|e| AppError::Database(format!("删除 CodeBuddy 凭证失败: {e}")))?;

        log::info!("[CodeBuddyAuth] 删除凭证: {id}");
        Ok(())
    }

    /// 列出所有凭证（脱敏）
    pub fn list_credentials(&self) -> Result<Vec<CodeBuddyCredentialDisplay>, AppError> {
        let creds = self.list_credentials_inner()?;
        Ok(creds.into_iter().map(|c| c.to_display()).collect())
    }

    /// 列出所有凭证（内部，含 token）
    fn list_credentials_inner(&self) -> Result<Vec<CodeBuddyCredential>, AppError> {
        let conn = self.db.conn.lock().map_err(|e| {
            AppError::Database(format!("Mutex lock failed: {}", e))
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT id, user_id, bearer_token, refresh_token, expires_in, created_at,
                        user_info, is_expired, use_count, sort_index, created_at_db
                 FROM codebuddy_credentials ORDER BY sort_index, created_at_db",
            )
            .map_err(|e| AppError::Database(format!("查询 CodeBuddy 凭证失败: {e}")))?;

        let creds = stmt
            .query_map([], |row| {
                Ok(CodeBuddyCredential {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    bearer_token: row.get(2)?,
                    refresh_token: row.get(3)?,
                    expires_in: row.get(4)?,
                    created_at: row.get(5)?,
                    user_info: row.get(6)?,
                    is_expired: row.get::<_, i32>(7)? != 0,
                    use_count: row.get::<_, u32>(8)?,
                    sort_index: row.get(9)?,
                    created_at_db: row.get(10)?,
                })
            })
            .map_err(|e| AppError::Database(format!("查询 CodeBuddy 凭证失败: {e}")))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(creds)
    }

    /// 更新使用次数
    fn update_use_count(&self, id: &str, count: u32) -> Result<(), AppError> {
        let conn = self.db.conn.lock().map_err(|e| {
            AppError::Database(format!("Mutex lock failed: {}", e))
        })?;

        conn.execute(
            "UPDATE codebuddy_credentials SET use_count = ?1 WHERE id = ?2",
            params![count, id],
        )
        .map_err(|e| AppError::Database(format!("更新 CodeBuddy 凭证使用次数失败: {e}")))?;

        Ok(())
    }

    /// 设置手动选择的凭证
    pub async fn set_manual_credential(&self, id: Option<String>) -> Result<(), AppError> {
        if let Some(ref cred_id) = id {
            // 验证凭证存在
            let creds = self.list_credentials_inner()?;
            if !creds.iter().any(|c| c.id == *cred_id) {
                return Err(AppError::Config(format!(
                    "凭证 {cred_id} 不存在"
                )));
            }
        }
        let mut mid = self.manual_selected_id.lock().await;
        *mid = id;
        Ok(())
    }

    /// 清除手动选择
    pub async fn clear_manual_selection(&self) {
        let mut mid = self.manual_selected_id.lock().await;
        *mid = None;
    }

    /// 切换自动轮换
    pub async fn toggle_auto_rotation(&self, enabled: bool) {
        let mut ae = self.auto_rotation_enabled.lock().await;
        *ae = enabled;
    }

    /// 设置轮换次数
    pub async fn set_rotation_count(&self, count: u32) {
        let mut rc = self.rotation_count.lock().await;
        *rc = count;
    }

    /// 获取凭证管理器状态
    pub async fn get_status(&self) -> CodeBuddyCredentialStatus {
        let all_creds = self.list_credentials_inner().unwrap_or_default();
        let active_count = all_creds.iter().filter(|c| !c.is_token_expired()).count();
        let current_index = *self.current_index.lock().await;
        let auto_rotation_enabled = *self.auto_rotation_enabled.lock().await;
        let rotation_count = *self.rotation_count.lock().await;
        let manual_selected_id = self.manual_selected_id.lock().await.clone();

        CodeBuddyCredentialStatus {
            total_count: all_creds.len(),
            active_count,
            current_index,
            auto_rotation_enabled,
            rotation_count,
            manual_selected_id,
        }
    }

    /// 删除所有凭证（登出）
    pub fn delete_all_credentials(&self) -> Result<(), AppError> {
        let conn = self.db.conn.lock().map_err(|e| {
            AppError::Database(format!("Mutex lock failed: {}", e))
        })?;

        conn.execute("DELETE FROM codebuddy_credentials", [])
            .map_err(|e| AppError::Database(format!("删除所有 CodeBuddy 凭证失败: {e}")))?;

        log::info!("[CodeBuddyAuth] 已删除所有凭证");
        Ok(())
    }

    /// 从目录导入凭证（迁移支持）
    ///
    /// 读取指定目录下的 .json 文件，每个文件包含一个 CodeBuddy 凭证。
    pub fn import_from_directory(&self, dir_path: &str) -> Result<usize, AppError> {
        let path = Path::new(dir_path);
        if !path.is_dir() {
            return Err(AppError::Config(format!(
                "目录不存在: {dir_path}"
            )));
        }

        let mut imported = 0;
        let entries = std::fs::read_dir(path).map_err(|e| {
            AppError::Config(format!("读取目录失败: {dir_path} - {e}"))
        })?;

        for entry in entries.filter_map(|e| e.ok()) {
            let entry_path = entry.path();
            if entry_path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = match std::fs::read_to_string(&entry_path) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!(
                        "[CodeBuddyAuth] 跳过无法读取的文件 {}: {e}",
                        entry_path.display()
                    );
                    continue;
                }
            };

            let json: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!(
                        "[CodeBuddyAuth] 跳过无效 JSON 文件 {}: {e}",
                        entry_path.display()
                    );
                    continue;
                }
            };

            let bearer_token = match json.get("bearer_token").and_then(|v| v.as_str()) {
                Some(t) => t.to_string(),
                None => {
                    log::warn!(
                        "[CodeBuddyAuth] 跳过缺少 bearer_token 的文件: {}",
                        entry_path.display()
                    );
                    continue;
                }
            };

            let user_id = json
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let refresh_token = json.get("refresh_token").and_then(|v| v.as_str()).map(String::from);
            let expires_in = json.get("expires_in").and_then(|v| v.as_i64());
            let created_at = json
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or_else(|| chrono::Utc::now().timestamp());
            let user_info = json
                .get("user_info")
                .cloned()
                .unwrap_or(serde_json::json!({}));

            // 使用 user_id 的 hash 作为 ID，避免重复导入
            let id = format!(
                "cb_{}",
                sha256_hex(&format!("{user_id}:{bearer_token}"))[..12].to_lowercase()
            );

            let now = chrono::Utc::now().timestamp();
            let cred = CodeBuddyCredential {
                id,
                user_id,
                bearer_token,
                refresh_token,
                expires_in,
                created_at,
                user_info: serde_json::to_string(&user_info).unwrap_or_default(),
                is_expired: false,
                use_count: 0,
                sort_index: imported as i32,
                created_at_db: now,
            };

            match self.add_credential(&cred) {
                Ok(()) => imported += 1,
                Err(e) => {
                    log::warn!(
                        "[CodeBuddyAuth] 导入凭证失败 {}: {e}",
                        entry_path.display()
                    );
                }
            }
        }

        log::info!("[CodeBuddyAuth] 从 {dir_path} 导入了 {imported} 个凭证");
        Ok(imported)
    }
}

/// 简单 SHA256 hex（用于生成凭证 ID）
#[allow(dead_code)]
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credential_expiry_check() {
        let now = chrono::Utc::now().timestamp();

        // 未过期（1 小时后过期）
        let cred = CodeBuddyCredential {
            id: "test1".to_string(),
            user_id: "user@test.com".to_string(),
            bearer_token: "test_token".to_string(),
            refresh_token: None,
            expires_in: Some(3600),
            created_at: now,
            user_info: "{}".to_string(),
            is_expired: false,
            use_count: 0,
            sort_index: 0,
            created_at_db: now,
        };
        assert!(!cred.is_token_expired());

        // 已过期（5 分钟前已过期，在 300s buffer 内）
        let cred = CodeBuddyCredential {
            id: "test2".to_string(),
            expires_in: Some(300),
            created_at: now - 600,
            ..cred.clone()
        };
        assert!(cred.is_token_expired());

        // 标记为过期
        let cred = CodeBuddyCredential {
            id: "test3".to_string(),
            is_expired: true,
            ..cred.clone()
        };
        assert!(cred.is_token_expired());

        // 无 expires_in 视为不过期
        let cred = CodeBuddyCredential {
            id: "test4".to_string(),
            expires_in: None,
            is_expired: false,
            ..cred.clone()
        };
        assert!(!cred.is_token_expired());
    }

    #[test]
    fn test_masked_token() {
        let cred = CodeBuddyCredential {
            id: "test".to_string(),
            user_id: "user@test.com".to_string(),
            bearer_token: "abcdefghijklmnop".to_string(),
            refresh_token: None,
            expires_in: None,
            created_at: 0,
            user_info: "{}".to_string(),
            is_expired: false,
            use_count: 0,
            sort_index: 0,
            created_at_db: 0,
        };
        assert_eq!(cred.masked_token(), "abcdef...mnop");

        let short_cred = CodeBuddyCredential {
            bearer_token: "short".to_string(),
            ..cred.clone()
        };
        assert_eq!(short_cred.masked_token(), "***");
    }

    #[test]
    fn test_to_display_masks_token() {
        let cred = CodeBuddyCredential {
            id: "test".to_string(),
            user_id: "user@test.com".to_string(),
            bearer_token: "abcdefghijklmnop".to_string(),
            refresh_token: Some("refresh_tok".to_string()),
            expires_in: Some(3600),
            created_at: 1700000000,
            user_info: r#"{"email":"user@test.com"}"#.to_string(),
            is_expired: false,
            use_count: 5,
            sort_index: 0,
            created_at_db: 1700000000,
        };

        let display = cred.to_display();
        assert_eq!(display.bearer_token, "abcdef...mnop");
        assert_eq!(display.refresh_token, Some("***".to_string()));
        assert_eq!(display.id, "test");
        assert_eq!(display.user_id, "user@test.com");
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex("test");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 produces 32 bytes = 64 hex chars
    }
}
