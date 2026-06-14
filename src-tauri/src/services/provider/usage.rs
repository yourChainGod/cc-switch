//! Usage script execution
//!
//! Handles executing and formatting usage query results.

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::{UsageData, UsageResult, UsageScript};
use crate::settings;
use crate::store::AppState;
use crate::usage_script;

/// Execute usage script and format result (private helper method)
pub(crate) async fn execute_and_format_usage_result(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    timeout: u64,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<UsageResult, AppError> {
    match usage_script::execute_usage_script(
        script_code,
        api_key,
        base_url,
        timeout,
        access_token,
        user_id,
        template_type,
    )
    .await
    {
        Ok(data) => {
            let usage_list: Vec<UsageData> = if data.is_array() {
                serde_json::from_value(data).map_err(|e| {
                    AppError::localized(
                        "usage_script.data_format_error",
                        format!("数据格式错误: {e}"),
                        format!("Data format error: {e}"),
                    )
                })?
            } else {
                let single: UsageData = serde_json::from_value(data).map_err(|e| {
                    AppError::localized(
                        "usage_script.data_format_error",
                        format!("数据格式错误: {e}"),
                        format!("Data format error: {e}"),
                    )
                })?;
                vec![single]
            };

            Ok(UsageResult {
                success: true,
                data: Some(usage_list),
                error: None,
            })
        }
        Err(err) => {
            let lang = settings::get_settings()
                .language
                .unwrap_or_else(|| "zh".to_string());

            let msg = match err {
                AppError::Localized { zh, en, .. } => {
                    if lang == "en" {
                        en
                    } else {
                        zh
                    }
                }
                other => other.to_string(),
            };

            Ok(UsageResult {
                success: false,
                data: None,
                error: Some(msg),
            })
        }
    }
}

/// Resolve `(api_key, base_url)` for JS usage scripts.
///
/// Explicit non-empty script values win; otherwise fall back to the provider's
/// per-app credential resolver so Codex/auth+config.toml, Gemini env, Hermes,
/// OpenCode, etc. all match the values previewed in the UI.
fn resolve_script_credentials(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> (String, String) {
    let explicit_api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let explicit_base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_end_matches('/').to_owned());

    // 显式凭证齐全时直接返回，省去一次 provider 配置（Codex TOML / env）解析。
    if let (Some(api_key), Some(base_url)) = (&explicit_api_key, &explicit_base_url) {
        return (api_key.clone(), base_url.clone());
    }

    let (provider_base_url, provider_api_key) = provider.resolve_usage_credentials(app_type);
    let api_key = explicit_api_key.unwrap_or(provider_api_key);
    let base_url = explicit_base_url.unwrap_or(provider_base_url);

    (api_key, base_url)
}

/// Query provider usage (using saved script configuration)
pub async fn query_usage(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
) -> Result<UsageResult, AppError> {
    let (script_code, timeout, api_key, base_url, access_token, user_id, template_type) = {
        let providers = state.db.get_all_providers(app_type.as_str())?;
        let provider = providers.get(provider_id).ok_or_else(|| {
            AppError::localized(
                "provider.not_found",
                format!("供应商不存在: {provider_id}"),
                format!("Provider not found: {provider_id}"),
            )
        })?;

        let usage_script = provider
            .meta
            .as_ref()
            .and_then(|m| m.usage_script.as_ref())
            .ok_or_else(|| {
                AppError::localized(
                    "provider.usage.script.missing",
                    "未配置用量查询脚本",
                    "Usage script is not configured",
                )
            })?;
        if !usage_script.enabled {
            return Err(AppError::localized(
                "provider.usage.disabled",
                "用量查询未启用",
                "Usage query is disabled",
            ));
        }

        let (api_key, base_url) = resolve_script_credentials(
            &app_type,
            provider,
            usage_script.api_key.as_deref(),
            usage_script.base_url.as_deref(),
        );

        (
            usage_script.code.clone(),
            usage_script.timeout.unwrap_or(10),
            api_key,
            base_url,
            usage_script.access_token.clone(),
            usage_script.user_id.clone(),
            usage_script.template_type.clone(),
        )
    };

    execute_and_format_usage_result(
        &script_code,
        &api_key,
        &base_url,
        timeout,
        access_token.as_deref(),
        user_id.as_deref(),
        template_type.as_deref(),
    )
    .await
}

/// Query usage for a single key.
///
/// 凭证优先级：`usage_script.apiKey` → 该 key 的 `key_value`（核心：默认用该 key 自己的 token）。
/// Base URL 优先级：`usage_script.baseUrl` → 供应商 env 的 base_url。
pub async fn query_key_usage(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
    key_id: &str,
) -> Result<UsageResult, AppError> {
    let (script_code, timeout, api_key, base_url, access_token, user_id, template_type) = {
        let key = state
            .db
            .get_provider_key(app_type.as_str(), provider_id, key_id)?
            .ok_or_else(|| {
                AppError::localized(
                    "provider.key.not_found",
                    format!("Key 不存在: {key_id}"),
                    format!("Key not found: {key_id}"),
                )
            })?;

        let usage_script = key.usage_script.as_ref().ok_or_else(|| {
            AppError::localized(
                "provider.usage.script.missing",
                "未配置用量查询脚本",
                "Usage script is not configured",
            )
        })?;
        if !usage_script.enabled {
            return Err(AppError::localized(
                "provider.usage.disabled",
                "用量查询未启用",
                "Usage query is disabled",
            ));
        }

        // Base URL 回退到供应商配置；apiKey 仍默认用当前 key 自身的 key_value。
        let provider_base_url = {
            let providers = state.db.get_all_providers(app_type.as_str())?;
            providers
                .get(provider_id)
                .map(|provider| provider.resolve_usage_credentials(&app_type).0)
                .filter(|value| !value.is_empty())
        };

        // 凭证优先级：usage_script.apiKey → 该 key 的 key_value
        let api_key = usage_script
            .api_key
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| key.key_value.clone());

        let base_url = usage_script
            .base_url
            .clone()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .or(provider_base_url)
            .unwrap_or_default();

        (
            usage_script.code.clone(),
            usage_script.timeout.unwrap_or(10),
            api_key,
            base_url,
            usage_script.access_token.clone(),
            usage_script.user_id.clone(),
            usage_script.template_type.clone(),
        )
    };

    execute_and_format_usage_result(
        &script_code,
        &api_key,
        &base_url,
        timeout,
        access_token.as_deref(),
        user_id.as_deref(),
        template_type.as_deref(),
    )
    .await
}

/// Aggregate usage across all usage-enabled keys of a provider (求和后返回单个结果)。
///
/// 遍历 `enabled` 且 `usage_script.enabled == true` 的 key，并行查询各自用量，
/// 累加首套餐的 total/used/remaining（缺失或 < 0 跳过该项）；`unit` 取首个非空；
/// `planName = "{N} keys 合计"`；部分失败时在 `extra` 标注「x/N 失败」。
pub async fn aggregate_provider_usage(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
) -> Result<UsageResult, AppError> {
    use futures::future::join_all;

    let keys = state
        .db
        .get_enabled_provider_keys(app_type.as_str(), provider_id)?;
    let usage_keys: Vec<_> = keys
        .iter()
        .filter(|k| k.usage_script.as_ref().map(|s| s.enabled).unwrap_or(false))
        .collect();
    let n = usage_keys.len();
    if n == 0 {
        return Err(AppError::localized(
            "provider.usage.no_keys_enabled",
            "没有启用用量查询的 Key",
            "No keys have usage query enabled",
        ));
    }

    // 并行查询，避免 N 个 key 串行等待（最坏 N × timeout）
    let results = join_all(usage_keys.iter().map(|key| {
        let app_type = app_type.clone();
        let key_id = key.id.clone();
        async move { query_key_usage(state, app_type, provider_id, &key_id).await }
    }))
    .await;

    let mut sum_total = 0.0f64;
    let mut has_total = false;
    let mut sum_used = 0.0f64;
    let mut has_used = false;
    let mut sum_remaining = 0.0f64;
    let mut has_remaining = false;
    let mut unit: Option<String> = None;
    let mut success_count = 0usize;
    let mut first_error: Option<String> = None;

    for r in results {
        match r {
            Ok(usage) if usage.success => {
                if let Some(first) = usage.data.as_ref().and_then(|d| d.first()) {
                    success_count += 1;
                    if let Some(t) = first.total {
                        if t >= 0.0 {
                            sum_total += t;
                            has_total = true;
                        }
                    }
                    if let Some(u) = first.used {
                        if u >= 0.0 {
                            sum_used += u;
                            has_used = true;
                        }
                    }
                    if let Some(rm) = first.remaining {
                        if rm >= 0.0 {
                            sum_remaining += rm;
                            has_remaining = true;
                        }
                    }
                    if unit.is_none() {
                        if let Some(un) = first.unit.as_ref().filter(|s| !s.is_empty()) {
                            unit = Some(un.clone());
                        }
                    }
                } else if first_error.is_none() {
                    first_error = Some("empty usage data".to_string());
                }
            }
            Ok(usage) => {
                if first_error.is_none() {
                    first_error = usage.error.clone();
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e.to_string());
                }
            }
        }
    }

    // 全部失败：返回失败态（携带首个错误），让卡片显示失败而非空合计
    if success_count == 0 {
        return Ok(UsageResult {
            success: false,
            data: None,
            error: first_error.or_else(|| Some("全部 Key 用量查询失败".to_string())),
        });
    }

    let fail_count = n - success_count;
    let extra = (fail_count > 0).then(|| format!("{fail_count}/{n} 失败"));

    let aggregated = UsageData {
        plan_name: Some(format!("{n} keys 合计")),
        extra,
        is_valid: Some(true),
        invalid_message: None,
        total: has_total.then_some(sum_total),
        used: has_used.then_some(sum_used),
        remaining: has_remaining.then_some(sum_remaining),
        unit,
    };

    Ok(UsageResult {
        success: true,
        data: Some(vec![aggregated]),
        error: None,
    })
}

/// Test usage script (using temporary script content, not saved)
#[allow(clippy::too_many_arguments)]
pub async fn test_usage_script(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
    script_code: &str,
    timeout: u64,
    api_key: Option<&str>,
    base_url: Option<&str>,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<UsageResult, AppError> {
    let providers = state.db.get_all_providers(app_type.as_str())?;
    // provider 不存在时（如尚未保存的新供应商测试脚本），回退到直接使用传入凭证。
    let (api_key, base_url) = match providers.get(provider_id) {
        Some(provider) => resolve_script_credentials(&app_type, provider, api_key, base_url),
        None => (
            api_key.unwrap_or_default().trim().to_owned(),
            base_url
                .unwrap_or_default()
                .trim()
                .trim_end_matches('/')
                .to_owned(),
        ),
    };

    execute_and_format_usage_result(
        script_code,
        &api_key,
        &base_url,
        timeout,
        access_token,
        user_id,
        template_type,
    )
    .await
}

/// Validate UsageScript configuration (boundary checks)
pub(crate) fn validate_usage_script(script: &UsageScript) -> Result<(), AppError> {
    // Validate auto query interval (0-1440 minutes, max 24 hours)
    if let Some(interval) = script.auto_query_interval {
        if interval > 1440 {
            return Err(AppError::localized(
                "usage_script.interval_too_large",
                format!("自动查询间隔不能超过 1440 分钟（24小时），当前值: {interval}"),
                format!(
                    "Auto query interval cannot exceed 1440 minutes (24 hours), current: {interval}"
                ),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::resolve_script_credentials;
    use crate::app_config::AppType;
    use crate::provider::Provider;
    use serde_json::json;

    fn provider_with_settings(settings_config: serde_json::Value) -> Provider {
        Provider::with_id(
            "provider-1".to_string(),
            "Provider".to_string(),
            settings_config,
            None,
        )
    }

    #[test]
    fn script_values_override_provider_credentials() {
        let provider = provider_with_settings(json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "provider-key",
                "ANTHROPIC_BASE_URL": "https://provider.example.com/"
            }
        }));

        let (api_key, base_url) = resolve_script_credentials(
            &AppType::Claude,
            &provider,
            Some(" script-key "),
            Some(" https://script.example.com/ "),
        );
        assert_eq!(api_key, "script-key");
        assert_eq!(base_url, "https://script.example.com");
    }

    #[test]
    fn empty_script_values_fall_back_to_provider_credentials() {
        let provider = provider_with_settings(json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "provider-key",
                "ANTHROPIC_BASE_URL": "https://provider.example.com/"
            }
        }));

        let (api_key, base_url) =
            resolve_script_credentials(&AppType::Claude, &provider, Some(""), None);
        assert_eq!(api_key, "provider-key");
        assert_eq!(base_url, "https://provider.example.com");
    }

    #[test]
    fn codex_fallback_reads_auth_and_config_toml() {
        let provider = provider_with_settings(json!({
            "auth": {
                "OPENAI_API_KEY": "openai-key"
            },
            "config": r#"model_provider = "newapi"

[model_providers.newapi]
base_url = "https://newapi.example.com/v1/"

[model_providers.other]
base_url = "https://other.example.com/v1"
"#
        }));

        let (api_key, base_url) =
            resolve_script_credentials(&AppType::Codex, &provider, None, None);
        assert_eq!(api_key, "openai-key");
        assert_eq!(base_url, "https://newapi.example.com/v1");
    }
}
