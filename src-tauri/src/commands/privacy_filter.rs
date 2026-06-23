//! 隐私过滤相关 Tauri 命令
//!
//! - 读取 / 保存隐私过滤配置（存于 settings KV）
//! - 测试：对给定文本按给定配置执行一次脱敏，供设置页「测试过滤」实时预览

use crate::proxy::types::PrivacyFilterConfig;

/// 获取隐私过滤配置
#[tauri::command]
pub async fn get_privacy_filter_config(
    state: tauri::State<'_, crate::AppState>,
) -> Result<PrivacyFilterConfig, String> {
    state
        .db
        .get_privacy_filter_config()
        .map_err(|e| e.to_string())
}

/// 保存隐私过滤配置
#[tauri::command]
pub async fn set_privacy_filter_config(
    state: tauri::State<'_, crate::AppState>,
    config: PrivacyFilterConfig,
) -> Result<bool, String> {
    state
        .db
        .set_privacy_filter_config(&config)
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// 测试结果（test_privacy_filter 返回）
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivacyFilterTestResult {
    /// 脱敏后的文本
    pub redacted: String,
    /// 命中的敏感片段数量
    pub count: usize,
}

/// 用给定配置对一段文本做脱敏测试（不落库，直接返回结果）。
#[tauri::command]
pub async fn test_privacy_filter(
    config: PrivacyFilterConfig,
    text: String,
) -> Result<PrivacyFilterTestResult, String> {
    let outcome = crate::privacy_filter::redact_text(&text, &config);
    Ok(PrivacyFilterTestResult {
        redacted: outcome.redacted,
        count: outcome.count,
    })
}
