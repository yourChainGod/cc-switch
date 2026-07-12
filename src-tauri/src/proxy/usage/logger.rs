//! Usage Logger - 记录 API 请求使用情况

use super::calculator::{CostBreakdown, CostCalculator, ModelPricing};
use super::parser::TokenUsage;
use crate::database::{Database, PRICING_SOURCE_REQUEST, PRICING_SOURCE_RESPONSE};
use crate::error::AppError;
use crate::services::usage_stats::{find_model_pricing_row, is_placeholder_pricing_model};
use rust_decimal::Decimal;
use std::str::FromStr;

/// 请求日志
#[derive(Debug, Clone)]
pub struct RequestLog {
    pub request_id: String,
    pub provider_id: String,
    pub provider_key_id: Option<String>,
    pub app_type: String,
    pub model: String,
    pub request_model: String,
    /// 写入时实际用于计价的模型名；错误行留空。
    pub pricing_model: String,
    pub usage: TokenUsage,
    pub cost: Option<CostBreakdown>,
    pub latency_ms: u64,
    pub first_token_ms: Option<u64>,
    pub status_code: u16,
    pub error_message: Option<String>,
    pub session_id: Option<String>,
    /// 供应商类型 (claude, claude_auth, codex, gemini, gemini_cli, openrouter)
    pub provider_type: Option<String>,
    /// 是否为流式请求
    pub is_streaming: bool,
    /// 成本倍数
    pub cost_multiplier: String,
    /// 决策链 JSON（仅多尝试请求写入；序列化自 forwarder 的 Vec<DecisionStep>）
    pub decision_trace: Option<String>,
    /// 失败时上游返回的错误响应体原文（仅失败请求写入）
    pub upstream_error_body: Option<String>,
}

/// 使用量记录器
pub struct UsageLogger<'a> {
    db: &'a Database,
}

impl<'a> UsageLogger<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 记录成功的请求
    pub fn log_request(&self, log: &RequestLog) -> Result<(), AppError> {
        let conn = crate::database::lock_conn!(self.db.conn);

        let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) =
            if let Some(cost) = &log.cost {
                (
                    cost.input_cost.to_string(),
                    cost.output_cost.to_string(),
                    cost.cache_read_cost.to_string(),
                    cost.cache_creation_cost.to_string(),
                    cost.total_cost.to_string(),
                )
            } else {
                (
                    "0".to_string(),
                    "0".to_string(),
                    "0".to_string(),
                    "0".to_string(),
                    "0".to_string(),
                )
            };

        let created_at = chrono::Utc::now().timestamp();

        conn.execute(
            "INSERT OR REPLACE INTO proxy_request_logs (
                request_id, provider_id, provider_key_id, app_type, model, request_model, pricing_model,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
                latency_ms, first_token_ms, status_code, error_message, session_id,
                provider_type, is_streaming, cost_multiplier, created_at,
                decision_trace, upstream_error_body
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
            rusqlite::params![
                log.request_id,
                log.provider_id,
                log.provider_key_id,
                log.app_type,
                log.model,
                log.request_model,
                log.pricing_model,
                log.usage.input_tokens,
                log.usage.output_tokens,
                log.usage.cache_read_tokens,
                log.usage.cache_creation_tokens,
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                log.latency_ms as i64,
                log.first_token_ms.map(|v| v as i64),
                log.status_code as i64,
                log.error_message,
                log.session_id,
                log.provider_type,
                log.is_streaming as i64,
                log.cost_multiplier,
                created_at,
                log.decision_trace,
                log.upstream_error_body,
            ],
        )
        .map_err(|e| AppError::Database(format!("记录请求日志失败: {e}")))?;

        drop(conn);

        // Keep this side effect after the log write: the request log remains the
        // source of truth, while 403-triggered key disablement is a best-effort
        // safety action that must not make usage logging fail.
        self.disable_matching_keys_after_forbidden(log);

        // 通知前端使用统计有更新（200ms 防抖合并，不阻塞写入路径）
        crate::usage_events::notify_log_recorded();

        Ok(())
    }

    fn disable_matching_keys_after_forbidden(&self, log: &RequestLog) {
        if log.status_code != 403 {
            return;
        }

        let Some(key_id) = log.provider_key_id.as_deref() else {
            return;
        };

        // `provider_key_id` points to the concrete key used for this request.
        // The DAO intentionally matches by key value across all app/provider
        // rows so duplicated keys in Claude, Codex, Gemini, or parallel
        // providers are disabled together.
        match self.db.disable_provider_keys_matching_key_id(key_id) {
            Ok(updated) if updated > 0 => {
                log::warn!(
                    "[USG-004] 403 request disabled matching provider keys: key_id={key_id}, updated={updated}"
                );
            }
            Ok(_) => {}
            Err(e) => {
                log::warn!(
                    "[USG-004] failed to disable matching provider keys after 403: key_id={key_id}, error={e}"
                );
            }
        }
    }

    /// 记录失败的请求
    ///
    /// 用于记录无法从上游获取 usage 信息的失败请求
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn log_error(
        &self,
        request_id: String,
        provider_id: String,
        app_type: String,
        model: String,
        status_code: u16,
        error_message: String,
        latency_ms: u64,
    ) -> Result<(), AppError> {
        let request_model = model.clone();
        let log = RequestLog {
            request_id,
            provider_id,
            provider_key_id: None,
            app_type,
            model,
            request_model,
            pricing_model: String::new(),
            usage: TokenUsage::default(),
            cost: None,
            latency_ms,
            first_token_ms: None,
            status_code,
            error_message: Some(error_message),
            session_id: None,
            provider_type: None,
            is_streaming: false,
            cost_multiplier: "1.0".to_string(),
            decision_trace: None,
            upstream_error_body: None,
        };

        self.log_request(&log)
    }

    /// 记录失败的请求（带更多上下文信息）
    ///
    /// 相比 log_error，这个方法接受更多参数以提供完整的请求上下文
    #[allow(clippy::too_many_arguments)]
    pub fn log_error_with_context(
        &self,
        request_id: String,
        provider_id: String,
        provider_key_id: Option<String>,
        app_type: String,
        model: String,
        status_code: u16,
        error_message: String,
        latency_ms: u64,
        is_streaming: bool,
        session_id: Option<String>,
        provider_type: Option<String>,
        decision_trace: Option<String>,
        upstream_error_body: Option<String>,
    ) -> Result<(), AppError> {
        let request_model = model.clone();
        let log = RequestLog {
            request_id,
            provider_id,
            provider_key_id,
            app_type,
            model,
            request_model,
            pricing_model: String::new(),
            usage: TokenUsage::default(),
            cost: None,
            latency_ms,
            first_token_ms: None,
            status_code,
            error_message: Some(error_message),
            session_id,
            provider_type,
            is_streaming,
            cost_multiplier: "1.0".to_string(),
            decision_trace,
            upstream_error_body,
        };

        self.log_request(&log)
    }

    /// 获取模型定价
    pub fn get_model_pricing(&self, model_id: &str) -> Result<Option<ModelPricing>, AppError> {
        let conn = crate::database::lock_conn!(self.db.conn);
        let row = find_model_pricing_row(&conn, model_id)?;
        match row {
            Some((input, output, cache_read, cache_creation)) => {
                ModelPricing::from_strings(&input, &output, &cache_read, &cache_creation)
                    .map(Some)
                    .map_err(|e| AppError::Database(format!("解析定价数据失败: {e}")))
            }
            None => Ok(None),
        }
    }

    /// 获取有效的倍率与计费模式来源（供应商优先，未配置则回退全局默认）
    pub async fn resolve_pricing_config(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> (Decimal, String) {
        let default_multiplier_raw = match self.db.get_default_cost_multiplier(app_type).await {
            Ok(value) => value,
            Err(e) => {
                log::warn!("[USG-003] 获取默认倍率失败 (app_type={app_type}): {e}");
                "1".to_string()
            }
        };
        let default_multiplier = match Decimal::from_str(&default_multiplier_raw) {
            Ok(value) => value,
            Err(e) => {
                log::warn!(
                    "[USG-003] 默认倍率解析失败 (app_type={app_type}): {default_multiplier_raw} - {e}"
                );
                Decimal::from(1)
            }
        };

        let default_pricing_source_raw = match self.db.get_pricing_model_source(app_type).await {
            Ok(value) => value,
            Err(e) => {
                log::warn!("[USG-003] 获取默认计费模式失败 (app_type={app_type}): {e}");
                PRICING_SOURCE_RESPONSE.to_string()
            }
        };
        let default_pricing_source = if default_pricing_source_raw == PRICING_SOURCE_RESPONSE
            || default_pricing_source_raw == PRICING_SOURCE_REQUEST
        {
            default_pricing_source_raw
        } else {
            log::warn!(
                "[USG-003] 默认计费模式无效 (app_type={app_type}): {default_pricing_source_raw}"
            );
            PRICING_SOURCE_RESPONSE.to_string()
        };

        let provider = self
            .db
            .get_provider_by_id(provider_id, app_type)
            .ok()
            .flatten();

        let (provider_multiplier, provider_pricing_source) = provider
            .as_ref()
            .and_then(|p| p.meta.as_ref())
            .map(|meta| {
                (
                    meta.cost_multiplier.as_deref(),
                    meta.pricing_model_source.as_deref(),
                )
            })
            .unwrap_or((None, None));

        let cost_multiplier = match provider_multiplier {
            Some(value) => match Decimal::from_str(value) {
                Ok(parsed) => parsed,
                Err(e) => {
                    log::warn!(
                        "[USG-003] 供应商倍率解析失败 (provider_id={provider_id}): {value} - {e}"
                    );
                    default_multiplier
                }
            },
            None => default_multiplier,
        };

        let pricing_model_source = match provider_pricing_source {
            Some(value) if value == PRICING_SOURCE_RESPONSE || value == PRICING_SOURCE_REQUEST => {
                value.to_string()
            }
            Some(value) => {
                log::warn!("[USG-003] 供应商计费模式无效 (provider_id={provider_id}): {value}");
                default_pricing_source.clone()
            }
            None => default_pricing_source.clone(),
        };

        (cost_multiplier, pricing_model_source)
    }

    /// 计算并记录请求
    #[allow(clippy::too_many_arguments)]
    pub fn log_with_calculation(
        &self,
        request_id: String,
        provider_id: String,
        provider_key_id: Option<String>,
        app_type: String,
        model: String,
        request_model: String,
        pricing_model: String,
        usage: TokenUsage,
        cost_multiplier: Decimal,
        latency_ms: u64,
        first_token_ms: Option<u64>,
        status_code: u16,
        session_id: Option<String>,
        provider_type: Option<String>,
        is_streaming: bool,
        decision_trace: Option<String>,
    ) -> Result<(), AppError> {
        let pricing = self.get_model_pricing(&pricing_model)?;

        let has_usage = usage.input_tokens > 0
            || usage.output_tokens > 0
            || usage.cache_read_tokens > 0
            || usage.cache_creation_tokens > 0;

        if pricing.is_none() && has_usage && !is_placeholder_pricing_model(&pricing_model) {
            log::warn!("[USG-002] 模型定价未找到，成本将记录为 0: {pricing_model}");
        }

        let cost = CostCalculator::try_calculate_for_app(
            &app_type,
            &usage,
            pricing.as_ref(),
            cost_multiplier,
        );

        let log = RequestLog {
            request_id,
            provider_id,
            provider_key_id,
            app_type,
            model,
            request_model,
            pricing_model,
            usage,
            cost,
            latency_ms,
            first_token_ms,
            status_code,
            error_message: None,
            session_id,
            provider_type,
            is_streaming,
            cost_multiplier: cost_multiplier.to_string(),
            decision_trace,
            upstream_error_body: None,
        };

        self.log_request(&log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{Provider, ProviderKeyInput, ProviderKeyStatus};
    use serde_json::json;

    fn save_provider_fixture(db: &Database, app_type: &str, provider_id: &str) {
        let provider = Provider {
            id: provider_id.to_string(),
            name: provider_id.to_string(),
            settings_config: json!({}),
            website_url: None,
            category: Some("third_party".to_string()),
            created_at: Some(1),
            sort_index: Some(1),
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        };
        db.save_provider(app_type, &provider)
            .expect("save provider fixture");
    }

    fn key_input(name: &str, key_value: &str) -> ProviderKeyInput {
        ProviderKeyInput {
            name: name.to_string(),
            key_value: key_value.to_string(),
            auth_field: Some("ANTHROPIC_API_KEY".to_string()),
            enabled: true,
            priority: 0,
            weight: 1,
            usage_script: None,
        }
    }

    #[test]
    fn test_log_request() -> Result<(), AppError> {
        let db = Database::memory()?;

        // 插入测试定价
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
                 VALUES ('test-model', 'Test Model', '3.0', '15.0')",
                [],
            )
            .unwrap();
        }

        let logger = UsageLogger::new(&db);

        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        logger.log_with_calculation(
            "req-123".to_string(),
            "provider-1".to_string(),
            Some("key-1".to_string()),
            "claude".to_string(),
            "test-model".to_string(),
            "req-model".to_string(),
            "test-model".to_string(),
            usage,
            Decimal::from(1),
            100,
            None,
            200,
            None,
            Some("claude".to_string()),
            false,
            None,
        )?;

        // 验证记录已插入
        let conn = crate::database::lock_conn!(db.conn);
        let (count, request_model, provider_key_id): (i64, String, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), request_model, provider_key_id FROM proxy_request_logs WHERE request_id = 'req-123'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(request_model, "req-model");
        assert_eq!(provider_key_id.as_deref(), Some("key-1"));
        drop(conn);

        let logs = db.get_request_logs(&Default::default(), 0, 10)?;
        assert_eq!(logs.total, 1);
        assert_eq!(logs.data[0].provider_key_id.as_deref(), Some("key-1"));

        let detail = db
            .get_request_detail("req-123")?
            .expect("request detail should exist");
        assert_eq!(detail.provider_key_id.as_deref(), Some("key-1"));
        Ok(())
    }

    #[test]
    fn test_log_error() -> Result<(), AppError> {
        let db = Database::memory()?;
        let logger = UsageLogger::new(&db);

        logger.log_error(
            "req-error".to_string(),
            "provider-1".to_string(),
            "claude".to_string(),
            "unknown-model".to_string(),
            500,
            "Internal Server Error".to_string(),
            50,
        )?;

        // 验证错误记录已插入
        let conn = crate::database::lock_conn!(db.conn);
        let (status, error): (i64, Option<String>) = conn
            .query_row(
                "SELECT status_code, error_message FROM proxy_request_logs WHERE request_id = 'req-error'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, 500);
        assert_eq!(error, Some("Internal Server Error".to_string()));
        Ok(())
    }

    #[test]
    fn log_403_disables_matching_provider_keys_across_providers() -> Result<(), AppError> {
        let db = Database::memory()?;
        save_provider_fixture(&db, "claude", "provider-a");
        save_provider_fixture(&db, "claude", "provider-b");
        save_provider_fixture(&db, "codex", "provider-c");
        save_provider_fixture(&db, "gemini", "provider-d");
        save_provider_fixture(&db, "codex", "provider-e");

        let failed = db.add_provider_key("claude", "provider-a", &key_input("a", "sk-shared"))?;
        let same_value =
            db.add_provider_key("claude", "provider-b", &key_input("b", "sk-shared"))?;
        let same_value_codex =
            db.add_provider_key("codex", "provider-c", &key_input("c", "sk-shared"))?;
        let same_value_gemini =
            db.add_provider_key("gemini", "provider-d", &key_input("d", "sk-shared"))?;
        let different_value =
            db.add_provider_key("codex", "provider-e", &key_input("e", "sk-other"))?;

        let logger = UsageLogger::new(&db);
        logger.log_error_with_context(
            "req-forbidden".to_string(),
            "provider-a".to_string(),
            Some(failed.id.clone()),
            "claude".to_string(),
            "claude-sonnet".to_string(),
            403,
            "Forbidden".to_string(),
            42,
            false,
            None,
            None,
            None,
            None,
        )?;

        let failed_after = db
            .get_provider_key("claude", "provider-a", &failed.id)?
            .expect("failed key exists");
        let same_value_after = db
            .get_provider_key("claude", "provider-b", &same_value.id)?
            .expect("matching key exists");
        let same_value_codex_after = db
            .get_provider_key("codex", "provider-c", &same_value_codex.id)?
            .expect("matching codex key exists");
        let same_value_gemini_after = db
            .get_provider_key("gemini", "provider-d", &same_value_gemini.id)?
            .expect("matching gemini key exists");
        let different_value_after = db
            .get_provider_key("codex", "provider-e", &different_value.id)?
            .expect("different key exists");

        assert!(!failed_after.enabled);
        assert_eq!(failed_after.status, ProviderKeyStatus::Disabled);
        assert!(!same_value_after.enabled);
        assert_eq!(same_value_after.status, ProviderKeyStatus::Disabled);
        assert!(!same_value_codex_after.enabled);
        assert_eq!(same_value_codex_after.status, ProviderKeyStatus::Disabled);
        assert!(!same_value_gemini_after.enabled);
        assert_eq!(same_value_gemini_after.status, ProviderKeyStatus::Disabled);
        assert!(different_value_after.enabled);
        assert_eq!(different_value_after.status, ProviderKeyStatus::Active);

        Ok(())
    }
}
