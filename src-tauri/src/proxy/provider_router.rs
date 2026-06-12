//! 供应商路由器模块
//!
//! 负责选择和管理代理目标供应商，实现智能故障转移

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::provider::{apply_provider_key_to_config, Provider, ProviderKey};
use crate::proxy::circuit_breaker::{AllowResult, CircuitBreaker, CircuitBreakerConfig};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 供应商路由器
pub struct ProviderRouter {
    /// 数据库连接
    db: Arc<Database>,
    /// 熔断器管理器 - key 格式: "app_type:provider_id" 或 "app_type:provider_id:key_id"
    circuit_breakers: Arc<RwLock<HashMap<String, Arc<CircuitBreaker>>>>,
}

#[derive(Debug, Clone)]
pub struct ProviderAttempt {
    pub provider: Provider,
    pub key_id: Option<String>,
}

impl ProviderAttempt {
    /// 路由通道标识。没有 key 池时，一个 provider 即一个通道；
    /// 有 key 池时，每个 provider key 都是一条独立通道。
    pub fn channel_id(&self) -> String {
        match self.key_id.as_deref() {
            Some(key_id) => format!("{}:{key_id}", self.provider.id),
            None => self.provider.id.clone(),
        }
    }

    pub fn circuit_key(&self, app_type: &str) -> String {
        ProviderRouter::channel_circuit_key(app_type, &self.provider.id, self.key_id.as_deref())
    }
}

impl ProviderRouter {
    /// 创建新的供应商路由器
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 选择可用的供应商（支持故障转移）
    ///
    /// 返回按优先级排序的可用供应商列表：
    /// - 故障转移关闭时：仅返回当前供应商
    /// - 故障转移开启时：仅使用故障转移队列，按队列顺序依次尝试（P1 → P2 → ...）
    pub async fn select_providers(&self, app_type: &str) -> Result<Vec<ProviderAttempt>, AppError> {
        self.select_providers_for_session(app_type, None).await
    }

    pub async fn select_providers_for_session(
        &self,
        app_type: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<ProviderAttempt>, AppError> {
        let mut result = Vec::new();
        let mut total_providers = 0usize;
        let mut circuit_open_count = 0usize;
        let mut key_pool_unavailable_count = 0usize;

        // 检查该应用的自动故障转移开关是否开启（从 proxy_config 表读取）
        let auto_failover_enabled = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(config) => config.auto_failover_enabled,
            Err(e) => {
                log::error!("[{app_type}] 读取 proxy_config 失败: {e}，默认禁用故障转移");
                false
            }
        };

        if auto_failover_enabled {
            // 故障转移开启：按队列顺序依次尝试（P1 → P2 → ...）
            let all_providers = self.db.get_all_providers(app_type)?;

            // 使用 DAO 返回的排序结果，确保和前端展示一致
            let mut ordered_ids: Vec<String> = self
                .db
                .get_failover_queue(app_type)?
                .into_iter()
                .map(|item| item.provider_id)
                .collect();

            // 智能默认（ccLoad/cch 风格）：未自定义队列时，所有支持路由的
            // 供应商按首页顺序自动参与故障转移，无需手动维护队列。
            if ordered_ids.is_empty() {
                ordered_ids = self.db.get_default_failover_order(app_type)?;
            }

            total_providers = ordered_ids.len();

            for provider_id in ordered_ids {
                let Some(provider) = all_providers.get(&provider_id).cloned() else {
                    continue;
                };

                let attempts = self.expand_provider_attempts(app_type, provider)?;
                if attempts.is_empty() {
                    key_pool_unavailable_count += 1;
                    continue;
                }

                let mut provider_has_available_channel = false;
                for attempt in attempts {
                    let breaker = self
                        .get_or_create_circuit_breaker(&attempt.circuit_key(app_type))
                        .await;

                    if breaker.is_available().await {
                        provider_has_available_channel = true;
                        result.push(attempt);
                    }
                }

                if !provider_has_available_channel {
                    circuit_open_count += 1;
                }
            }
        } else {
            // 故障转移关闭：仅使用当前供应商，跳过熔断器检查
            let current_id = AppType::from_str(app_type)
                .ok()
                .and_then(|app_enum| {
                    crate::settings::get_effective_current_provider(&self.db, &app_enum)
                        .ok()
                        .flatten()
                })
                .or_else(|| self.db.get_current_provider(app_type).ok().flatten());

            if let Some(current_id) = current_id {
                if let Some(current) = self.db.get_provider_by_id(&current_id, app_type)? {
                    total_providers = 1;
                    let attempts = self.expand_provider_attempts(app_type, current)?;
                    if attempts.is_empty() {
                        key_pool_unavailable_count += 1;
                    }
                    result.extend(attempts);
                }
            }
        }

        if result.is_empty() {
            if total_providers > 0 && circuit_open_count == total_providers {
                log::warn!("[{app_type}] [FO-004] 所有供应商均已熔断");
                return Err(AppError::AllProvidersCircuitOpen);
            } else if total_providers > 0
                && key_pool_unavailable_count + circuit_open_count >= total_providers
            {
                log::warn!("[{app_type}] [FO-006] Provider Key Pool 中没有可用 Key");
                return Err(AppError::NoProviderKeysAvailable);
            } else {
                log::warn!("[{app_type}] [FO-005] 未配置供应商");
                return Err(AppError::NoProvidersConfigured);
            }
        }

        self.apply_working_channel_affinity(app_type, &mut result, auto_failover_enabled)?;

        if let Some(session_id) = session_id.filter(|session_id| !session_id.trim().is_empty()) {
            self.apply_session_affinity(app_type, session_id, &mut result)?;
        }

        Ok(result)
    }

    fn apply_working_channel_affinity(
        &self,
        app_type: &str,
        attempts: &mut Vec<ProviderAttempt>,
        preserve_provider_order: bool,
    ) -> Result<(), AppError> {
        let Some(binding) = self.db.get_working_channel_affinity(app_type)? else {
            return Ok(());
        };

        let Some(index) = attempts.iter().position(|attempt| {
            attempt.provider.id == binding.provider_id && attempt.key_id == binding.key_id
        }) else {
            return Ok(());
        };

        let target_index = if preserve_provider_order {
            attempts
                .iter()
                .position(|attempt| attempt.provider.id == binding.provider_id)
                .unwrap_or(index)
        } else {
            0
        };

        if index > target_index {
            let attempt = attempts.remove(index);
            attempts.insert(target_index, attempt);
        }
        Ok(())
    }

    fn apply_session_affinity(
        &self,
        app_type: &str,
        session_id: &str,
        attempts: &mut Vec<ProviderAttempt>,
    ) -> Result<(), AppError> {
        let Some(binding) = self.db.get_session_affinity(app_type, session_id)? else {
            return Ok(());
        };

        let Some(index) = attempts.iter().position(|attempt| {
            attempt.provider.id == binding.provider_id && attempt.key_id == binding.key_id
        }) else {
            return Ok(());
        };

        if index > 0 {
            let attempt = attempts.remove(index);
            attempts.insert(0, attempt);
        }
        Ok(())
    }

    fn expand_provider_attempts(
        &self,
        app_type: &str,
        provider: Provider,
    ) -> Result<Vec<ProviderAttempt>, AppError> {
        let mut keys = self.db.get_enabled_provider_keys(app_type, &provider.id)?;
        if keys.is_empty() {
            if self.db.has_provider_keys(app_type, &provider.id)? {
                return Ok(Vec::new());
            }
            return Ok(vec![ProviderAttempt {
                provider,
                key_id: None,
            }]);
        }

        Self::apply_key_order(&mut keys);

        Ok(keys
            .into_iter()
            .map(|key| ProviderAttempt {
                provider: apply_provider_key_to_config(app_type, &provider, &key),
                key_id: Some(key.id),
            })
            .collect())
    }

    /// 通道排序：优先级 → 连续失败少 → 最久未用（LRU 轮转）。
    /// 同优先级的 key 之间自然轮换，无需权重概念。
    fn apply_key_order(keys: &mut [ProviderKey]) {
        keys.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.consecutive_failures.cmp(&b.consecutive_failures))
                .then_with(|| {
                    a.last_used_at
                        .unwrap_or(0)
                        .cmp(&b.last_used_at.unwrap_or(0))
                })
                .then_with(|| a.created_at.cmp(&b.created_at))
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    /// 请求执行前获取熔断器“放行许可”
    ///
    /// - Closed：直接放行
    /// - Open：超时到达后切到 HalfOpen 并放行一次探测
    /// - HalfOpen：按限流规则放行探测
    ///
    /// 注意：调用方必须在请求结束后通过 `record_result()` 释放 HalfOpen 名额，
    /// 否则会导致该 Provider 长时间无法进入探测状态。
    #[allow(dead_code)]
    pub async fn allow_provider_request(&self, provider_id: &str, app_type: &str) -> AllowResult {
        self.allow_channel_request(provider_id, None, app_type)
            .await
    }

    pub async fn allow_channel_request(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        app_type: &str,
    ) -> AllowResult {
        let circuit_key = Self::channel_circuit_key(app_type, provider_id, key_id);
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;
        breaker.allow_request().await
    }

    /// 记录供应商请求结果
    #[allow(dead_code)]
    pub async fn record_result(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
    ) -> Result<(), AppError> {
        self.record_channel_result(
            provider_id,
            None,
            app_type,
            used_half_open_permit,
            success,
            error_msg,
        )
        .await
    }

    /// 记录通道请求结果。通道维度负责熔断；Provider 健康统计仍按 Provider 聚合。
    pub async fn record_channel_result(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
    ) -> Result<(), AppError> {
        // 1. 按应用独立获取熔断器配置
        let failure_threshold = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => app_config.circuit_failure_threshold,
            Err(_) => 5, // 默认值
        };

        // 2. 更新熔断器状态
        let circuit_key = Self::channel_circuit_key(app_type, provider_id, key_id);
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;

        if success {
            breaker.record_success(used_half_open_permit).await;
        } else {
            breaker.record_failure(used_half_open_permit).await;
        }

        // 3. 更新数据库健康状态（使用配置的阈值）
        //
        // 仅无 key 池的通道（provider 即通道）驱动 provider 级健康；
        // key 通道的失败只记在 provider_keys（冷却/退避），避免单个 key
        // 故障把整个 provider 标记为不健康（连坐）。
        if key_id.is_none() {
            self.db
                .update_provider_health_with_threshold(
                    provider_id,
                    app_type,
                    success,
                    error_msg.clone(),
                    failure_threshold,
                )
                .await?;
        }

        Ok(())
    }

    pub async fn record_key_success(
        &self,
        provider_id: &str,
        app_type: &str,
        key_id: &str,
    ) -> Result<(), AppError> {
        self.db
            .record_provider_key_success(app_type, provider_id, key_id)?;
        Ok(())
    }

    pub async fn record_key_failure(
        &self,
        provider_id: &str,
        app_type: &str,
        key_id: &str,
        cooldown_base_seconds: i64,
        cooldown_cap_seconds: i64,
        grace_failures: i64,
    ) -> Result<(), AppError> {
        self.db.record_provider_key_failure(
            app_type,
            provider_id,
            key_id,
            cooldown_base_seconds,
            cooldown_cap_seconds,
            grace_failures,
        )?;
        Ok(())
    }

    pub async fn bind_session_affinity(
        &self,
        app_type: &str,
        session_id: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        self.db
            .upsert_session_affinity(app_type, session_id, provider_id, key_id)?;
        Ok(())
    }

    pub async fn bind_working_channel_affinity(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        self.db
            .upsert_working_channel_affinity(app_type, provider_id, key_id)?;
        Ok(())
    }

    pub async fn clear_working_channel_affinity_if_matches(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        self.db
            .delete_working_channel_affinity_if_matches(app_type, provider_id, key_id)?;
        Ok(())
    }

    pub async fn clear_session_affinity_if_matches(
        &self,
        app_type: &str,
        provider_id: &str,
        key_id: Option<&str>,
    ) -> Result<(), AppError> {
        self.db
            .delete_session_affinity_if_matches(app_type, provider_id, key_id)?;
        Ok(())
    }

    /// 重置熔断器（手动恢复）
    pub async fn reset_circuit_breaker(&self, circuit_key: &str) {
        let breakers = self.circuit_breakers.read().await;
        if let Some(breaker) = breakers.get(circuit_key) {
            breaker.reset().await;
        }
    }

    /// 重置指定供应商的熔断器
    pub async fn reset_provider_breaker(&self, provider_id: &str, app_type: &str) {
        let exact_key = Self::channel_circuit_key(app_type, provider_id, None);
        let prefix = format!("{exact_key}:");
        let circuit_keys = {
            let breakers = self.circuit_breakers.read().await;
            breakers
                .keys()
                .filter(|key| key.as_str() == exact_key || key.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>()
        };

        for circuit_key in circuit_keys {
            self.reset_circuit_breaker(&circuit_key).await;
        }
    }

    /// 仅释放 HalfOpen permit，不影响健康统计（neutral 接口）
    ///
    /// 用于整流器等场景：请求结果不应计入 Provider 健康度，
    /// 但仍需释放占用的探测名额，避免 HalfOpen 状态卡死
    #[allow(dead_code)]
    pub async fn release_permit_neutral(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        self.release_channel_permit_neutral(provider_id, None, app_type, used_half_open_permit)
            .await;
    }

    pub async fn release_channel_permit_neutral(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        if !used_half_open_permit {
            return;
        }
        let circuit_key = Self::channel_circuit_key(app_type, provider_id, key_id);
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;
        breaker.release_half_open_permit();
    }

    /// 更新所有熔断器的配置（热更新）
    pub async fn update_all_configs(&self, config: CircuitBreakerConfig) {
        let breakers = self.circuit_breakers.read().await;
        for breaker in breakers.values() {
            breaker.update_config(config.clone()).await;
        }
    }

    /// 更新指定应用已创建熔断器的配置（热更新）
    pub async fn update_app_configs(&self, app_type: &str, config: CircuitBreakerConfig) {
        let prefix = format!("{app_type}:");
        let breakers = self.circuit_breakers.read().await;
        for (key, breaker) in breakers.iter() {
            if key.starts_with(&prefix) {
                breaker.update_config(config.clone()).await;
            }
        }
    }

    /// 获取熔断器状态
    #[allow(dead_code)]
    pub async fn get_circuit_breaker_stats(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Option<crate::proxy::circuit_breaker::CircuitBreakerStats> {
        let circuit_key = Self::channel_circuit_key(app_type, provider_id, None);
        let breakers = self.circuit_breakers.read().await;

        if let Some(breaker) = breakers.get(&circuit_key) {
            Some(breaker.get_stats().await)
        } else {
            None
        }
    }

    /// 获取或创建熔断器
    async fn get_or_create_circuit_breaker(&self, key: &str) -> Arc<CircuitBreaker> {
        // 先尝试读锁获取
        {
            let breakers = self.circuit_breakers.read().await;
            if let Some(breaker) = breakers.get(key) {
                return breaker.clone();
            }
        }

        // 如果不存在，获取写锁创建
        let mut breakers = self.circuit_breakers.write().await;

        // 双重检查，防止竞争条件
        if let Some(breaker) = breakers.get(key) {
            return breaker.clone();
        }

        // 从 key 中提取 app_type (格式: "app_type:channel_id")
        let app_type = key.split(':').next().unwrap_or("claude");

        // 按应用独立读取熔断器配置
        let config = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => crate::proxy::circuit_breaker::CircuitBreakerConfig {
                failure_threshold: app_config.circuit_failure_threshold,
                success_threshold: app_config.circuit_success_threshold,
                timeout_seconds: app_config.circuit_timeout_seconds as u64,
                error_rate_threshold: app_config.circuit_error_rate_threshold,
                min_requests: app_config.circuit_min_requests,
            },
            Err(_) => crate::proxy::circuit_breaker::CircuitBreakerConfig::default(),
        };

        let breaker = Arc::new(CircuitBreaker::new(config));
        breakers.insert(key.to_string(), breaker.clone());

        breaker
    }

    fn channel_circuit_key(app_type: &str, provider_id: &str, key_id: Option<&str>) -> String {
        match key_id {
            Some(key_id) => format!("{app_type}:{provider_id}:{key_id}"),
            None => format!("{app_type}:{provider_id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use serde_json::{json, Value};
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().expect("reload settings");

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }

            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }

            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_router_creation() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let router = ProviderRouter::new(db);

        let breaker = router.get_or_create_circuit_breaker("claude:test").await;
        assert!(breaker.allow_request().await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_disabled_uses_current_provider() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider.id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_key_pool_expands_attempts_and_injects_key() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            attempts[0]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-1")
        );
        assert_eq!(
            attempts[1]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-2")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_key_pool_uses_lru_schedule_with_same_priority() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let now = chrono::Utc::now().timestamp();

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let stale = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "stale".to_string(),
                    key_value: "sk-stale".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let fresh = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "fresh".to_string(),
                    key_value: "sk-fresh".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 4,
                },
            )
            .unwrap();

        let mut stale_key = db
            .get_provider_key("claude", "a", &stale.id)
            .unwrap()
            .unwrap();
        stale_key.last_used_at = Some(now - 60);
        db.save_provider_key(&stale_key).unwrap();

        let mut fresh_key = db
            .get_provider_key("claude", "a", &fresh.id)
            .unwrap()
            .unwrap();
        fresh_key.last_used_at = Some(now - 20);
        db.save_provider_key(&fresh_key).unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        // 同优先级按最久未用轮转，weight 不再参与调度
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(stale.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(fresh.id.as_str()));
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_with_key_pool_does_not_fallback_to_embedded_key_when_pool_unavailable() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        db.record_provider_key_failure("claude", "a", &key.id, 60, 60, 0)
            .unwrap();

        let router = ProviderRouter::new(db.clone());
        let err = router.select_providers("claude").await.unwrap_err();

        assert!(matches!(err, AppError::NoProviderKeysAvailable));
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_skips_provider_when_key_pool_unavailable() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-a"
                }
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "Provider B".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-b"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        let key_a = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-a".to_string(),
                    key_value: "sk-key-a".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        db.record_provider_key_failure("claude", "a", &key_a.id, 60, 60, 0)
            .unwrap();

        let key_b = db
            .add_provider_key(
                "claude",
                "b",
                &crate::provider::ProviderKeyInput {
                    name: "key-b".to_string(),
                    key_value: "sk-key-b".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].provider.id, "b");
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_b.id.as_str()));
        assert_eq!(
            attempts[0]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-b")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_key_pool_injects_app_specific_auth_fields() {
        let _home = TempHome::new();
        let cases = [
            ("codex", "/auth/OPENAI_API_KEY", None),
            ("gemini", "/env/GEMINI_API_KEY", None),
            ("openclaw", "/apiKey", None),
            ("hermes", "/api_key", None),
            ("opencode", "/options/apiKey", None),
            ("claude", "/auth/customToken", Some("auth.customToken")),
        ];

        for (app_type, pointer, auth_field) in cases {
            let db = Arc::new(Database::memory().unwrap());
            let provider =
                Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
            db.save_provider(app_type, &provider).unwrap();
            db.set_current_provider(app_type, "a").unwrap();
            let key = db
                .add_provider_key(
                    app_type,
                    "a",
                    &crate::provider::ProviderKeyInput {
                        name: "key-1".to_string(),
                        key_value: format!("sk-{app_type}"),
                        auth_field: auth_field.map(str::to_string),
                        enabled: true,
                        priority: 10,
                        weight: 1,
                    },
                )
                .unwrap();

            let router = ProviderRouter::new(db.clone());
            let attempts = router.select_providers(app_type).await.unwrap();
            let expected_key_value = format!("sk-{app_type}");

            assert_eq!(attempts.len(), 1, "app_type={app_type}");
            assert_eq!(attempts[0].key_id.as_deref(), Some(key.id.as_str()));
            assert_eq!(
                attempts[0]
                    .provider
                    .settings_config
                    .pointer(pointer)
                    .and_then(Value::as_str),
                Some(expected_key_value.as_str()),
                "app_type={app_type}, pointer={pointer}"
            );
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_config_key_does_not_override_key_pool_schedule() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();

        let mut provider = db.get_provider_by_id("a", "claude").unwrap().unwrap();
        provider
            .meta
            .get_or_insert_with(Default::default)
            .config_key_id = Some(key_2.id.clone());
        db.save_provider("claude", &provider).unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            attempts[0]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-1")
        );
        assert_eq!(
            attempts[1]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-2")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_session_affinity_prioritizes_bound_key_attempt() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();
        db.upsert_session_affinity("claude", "session-1", "a", Some(&key_2.id))
            .unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router
            .select_providers_for_session("claude", Some("session-1"))
            .await
            .unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(
            attempts[0]
                .provider
                .settings_config
                .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                .and_then(Value::as_str),
            Some("sk-key-2")
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_working_channel_affinity_prioritizes_last_successful_key() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();
        db.upsert_working_channel_affinity("claude", "a", Some(&key_2.id))
            .unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_1.id.as_str()));
    }

    #[tokio::test]
    #[serial]
    async fn test_session_affinity_overrides_working_channel_affinity() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();
        db.upsert_working_channel_affinity("claude", "a", Some(&key_2.id))
            .unwrap();
        db.upsert_session_affinity("claude", "session-1", "a", Some(&key_1.id))
            .unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router
            .select_providers_for_session("claude", Some("session-1"))
            .await
            .unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_2.id.as_str()));
    }

    #[tokio::test]
    #[serial]
    async fn test_working_channel_affinity_preserves_provider_order_in_failover_queue() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let mut provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        provider_a.sort_index = Some(1);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(2);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();
        db.upsert_working_channel_affinity("claude", "b", None)
            .unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].provider.id, "a");
        assert_eq!(attempts[1].provider.id, "b");
    }

    #[tokio::test]
    #[serial]
    async fn test_working_channel_affinity_reorders_keys_within_failover_provider() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let mut provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-a"
                }
            }),
            None,
        );
        provider_a.sort_index = Some(1);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(2);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();
        db.upsert_working_channel_affinity("claude", "a", Some(&key_2.id))
            .unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0].provider.id, "a");
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(attempts[1].provider.id, "a");
        assert_eq!(attempts[1].key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(attempts[2].provider.id, "b");
        assert_eq!(attempts[2].key_id, None);
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_order_ignoring_current() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // 设置 sort_index 来控制顺序：b=1, a=2
        let mut provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        provider_a.sort_index = Some(2);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        db.add_to_failover_queue("claude", "b").unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // 启用自动故障转移（使用新的 proxy_config API）
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 2);
        // 故障转移开启时：仅按队列顺序选择（忽略当前供应商）
        assert_eq!(providers[0].provider.id, "b");
        assert_eq!(providers[1].provider.id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_only_even_if_current_not_in_queue() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        // 只把 b 加入故障转移队列（模拟“当前供应商不在队列里”的常见配置）
        db.add_to_failover_queue("claude", "b").unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].provider.id, "b");
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_empty_queue_falls_back_to_home_order_excluding_official() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let mut provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        provider_a.sort_index = Some(2);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);
        let mut official = Provider::with_id(
            "official".to_string(),
            "Claude Official".to_string(),
            json!({}),
            None,
        );
        official.sort_index = Some(0);
        official.category = Some("official".to_string());

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.save_provider("claude", &official).unwrap();
        // 不维护队列：智能默认应按首页顺序使用全部支持路由的供应商

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].provider.id, "b");
        assert_eq!(providers[1].provider.id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_select_providers_does_not_consume_half_open_permit() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();

        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        // 启用自动故障转移（使用新的 proxy_config API）
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        router
            .record_result("b", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        let providers = router.select_providers("claude").await.unwrap();
        assert_eq!(providers.len(), 2);

        assert!(router.allow_provider_request("b", "claude").await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_channel_circuit_open_only_skips_that_provider_key() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 60,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Provider A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );

        db.save_provider("claude", &provider_a).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();
        let key_1 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "a",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                },
            )
            .unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        router
            .record_channel_result(
                "a",
                Some(&key_1.id),
                "claude",
                false,
                false,
                Some("fail".to_string()),
            )
            .await
            .unwrap();

        let attempts = router.select_providers("claude").await.unwrap();

        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].provider.id, "a");
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_2.id.as_str()));
    }

    #[tokio::test]
    #[serial]
    async fn test_release_permit_neutral_frees_half_open_slot() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // 配置熔断器：1 次失败即熔断，0 秒超时立即进入 HalfOpen
        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        db.save_provider("claude", &provider_a).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // 启用自动故障转移
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        // 触发熔断：1 次失败
        router
            .record_result("a", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        // 第一次请求：获取 HalfOpen 探测名额
        let first = router.allow_provider_request("a", "claude").await;
        assert!(first.allowed);
        assert!(first.used_half_open_permit);

        // 第二次请求应被拒绝（名额已被占用）
        let second = router.allow_provider_request("a", "claude").await;
        assert!(!second.allowed);

        // 使用 release_permit_neutral 释放名额（不影响健康统计）
        router
            .release_permit_neutral("a", "claude", first.used_half_open_permit)
            .await;

        // 第三次请求应被允许（名额已释放）
        let third = router.allow_provider_request("a", "claude").await;
        assert!(third.allowed);
        assert!(third.used_half_open_permit);
    }
}
