//! 请求转发器
//!
//! 负责将请求转发到上游Provider，支持故障转移

use super::hyper_client::ProxyResponse;
use super::{
    body_filter::filter_private_params_with_whitelist,
    error::*,
    failover_switch::FailoverSwitchManager,
    json_canonical::{canonicalize_value, short_value_hash},
    log_codes::fwd as log_fwd,
    provider_router::{ProviderAttempt, ProviderRouter},
    providers::{
        codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore, get_adapter,
        ProviderAdapter, ProviderType,
    },
    thinking_budget_rectifier::{rectify_thinking_budget, should_rectify_thinking_budget},
    thinking_rectifier::{
        normalize_thinking_type, rectify_anthropic_request, should_rectify_thinking_signature,
    },
    types::{OptimizerConfig, ProxyStatus, RectifierConfig},
    ProxyError,
};
use crate::{app_config::AppType, provider::Provider};
use futures::StreamExt;
use http::Extensions;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ForwardResult {
    pub response: ProxyResponse,
    pub provider: Provider,
    pub key_id: Option<String>,
    pub claude_api_format: Option<String>,
    /// 实际发往上游的模型名（模型映射/路由接管后的值）。
    pub outbound_model: Option<String>,
    /// 活跃连接 RAII guard：随响应一起流转到 response_processor / handle_claude_transform，
    /// 最终被 move 进流式 body future（或非流式响应作用域），覆盖整个响应生命周期。
    pub(crate) connection_guard: Option<ActiveConnectionGuard>,
}

pub struct ForwardError {
    pub error: ProxyError,
    pub provider: Option<Provider>,
    pub key_id: Option<String>,
}

/// 活跃连接 RAII guard
///
/// 构造时把 `ProxyStatus.active_connections` +1；Drop 时在 tokio runtime 上调度
/// 一个异步任务执行 -1，从而支持把 guard move 进流式 body future（stream 自然结束
/// 时 guard 与 future 一起 drop）。
///
/// 设计动机：之前在 `forward_with_retry` 出口处同步 -1，但流式响应的 body 实际
/// 在 `create_logged_passthrough_stream` 内还会继续 yield 字节流，导致 UI 的
/// `active_connections` 计数过早归零。RAII guard 让"减量"由 Rust 类型系统驱动，
/// 不需要每条出口路径都手动调用。
pub(crate) struct ActiveConnectionGuard {
    status: Arc<RwLock<ProxyStatus>>,
}

impl ActiveConnectionGuard {
    pub(crate) async fn acquire(status: Arc<RwLock<ProxyStatus>>) -> Self {
        {
            let mut s = status.write().await;
            s.active_connections = s.active_connections.saturating_add(1);
        }
        Self { status }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        // Drop 不能 await：把减量操作调度到 tokio runtime
        let status = self.status.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut s = status.write().await;
                s.active_connections = s.active_connections.saturating_sub(1);
            });
        }
        // 没有 runtime 时静默丢失计数（仅 UI 展示用，可接受最终一致性）
    }
}

pub struct RequestForwarder {
    /// 共享的 ProviderRouter（持有熔断器状态）
    router: Arc<ProviderRouter>,
    status: Arc<RwLock<ProxyStatus>>,
    current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
    gemini_shadow: Arc<GeminiShadowStore>,
    codex_chat_history: Arc<CodexChatHistoryStore>,
    /// 故障转移切换管理器
    failover_manager: Arc<FailoverSwitchManager>,
    /// AppHandle，用于发射事件和更新托盘
    app_handle: Option<tauri::AppHandle>,
    /// 请求开始时的"当前供应商 ID"（用于判断是否需要同步 UI/托盘）
    current_provider_id_at_start: String,
    /// 代理会话 ID（用于 Gemini Native shadow replay）
    session_id: String,
    /// Session ID 是否由客户端提供；生成值不能作为上游缓存身份。
    session_client_provided: bool,
    /// 整流器配置
    rectifier_config: RectifierConfig,
    /// 优化器配置
    optimizer_config: OptimizerConfig,
    /// 路由层模型映射配置（按客户端区分的 from→to 映射，命中即为最终上游模型）
    model_routing: super::model_routing::ModelRoutingConfig,
    /// 非流式请求超时（秒）
    non_streaming_timeout: std::time::Duration,
    /// 流式请求响应头等待超时（秒）
    streaming_first_byte_timeout: std::time::Duration,
    /// 单个客户端请求最多尝试的 provider 数。
    ///
    /// 由 `AppProxyConfig.max_retries` (UI: "请求失败时的重试次数, 0-10") 派生：
    /// `max_attempts = max_retries + 1`，所以 max_retries=0 表示仅尝试一家、
    /// max_retries=3（默认）表示最多 4 家。loop 同时受 providers.len() 自然限制。
    max_attempts: usize,
    /// anyrouter 429 同通道重试预算（次数 / 总等待毫秒）。
    /// 刻意不受 max_attempts/max_retries 约束，常量给足；字段化仅为测试可覆写。
    anyrouter_429_max_retries: usize,
    anyrouter_429_total_wait_ms: u64,
}

impl RequestForwarder {
    /// 预防式 media 降级：发送前对 text-only 模型把图片块替换为标记。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；其中"启发式模型名单预测"
    /// 再受 `request_media_heuristic` 单独管辖（显式声明 text-only 始终生效）。
    /// 返回被替换的图片块数量（0 = 未触发或开关关闭）。
    fn apply_media_prevention(&self, body: &mut Value, provider: &Provider) -> usize {
        if !(self.rectifier_config.enabled && self.rectifier_config.request_media_fallback) {
            return 0;
        }
        let replaced_images = super::media_sanitizer::replace_images_for_text_only_model(
            body,
            provider,
            self.rectifier_config.request_media_heuristic,
        );
        if replaced_images > 0 {
            let model = body.get("model").and_then(Value::as_str).unwrap_or("");
            log::info!(
                "[Media] Replaced {replaced_images} image block(s) with {} for text-only provider={}, model={}",
                super::media_sanitizer::UNSUPPORTED_IMAGE_MARKER,
                provider.id,
                model
            );
        }
        replaced_images
    }

    /// 反应式 media 重试判定：上游因图片输入报错后，是否应替换图片块并对同一供应商重试一次。
    ///
    /// 受 `enabled && request_media_fallback` 管辖；不涉及 `request_media_heuristic`——
    /// 这里是上游"实测"错误后的纯恢复，不是预测，故启发式开关与它无关。
    fn media_retry_should_trigger(
        &self,
        adapter_name: &str,
        already_retried: bool,
        provider_body: &Value,
        error: &ProxyError,
    ) -> bool {
        matches!(adapter_name, "Claude" | "Codex")
            && self.rectifier_config.enabled
            && self.rectifier_config.request_media_fallback
            && !already_retried
            && super::media_sanitizer::contains_image_blocks(provider_body)
            && super::media_sanitizer::is_unsupported_image_error(error)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        router: Arc<ProviderRouter>,
        non_streaming_timeout: u64,
        status: Arc<RwLock<ProxyStatus>>,
        current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
        gemini_shadow: Arc<GeminiShadowStore>,
        codex_chat_history: Arc<CodexChatHistoryStore>,
        failover_manager: Arc<FailoverSwitchManager>,
        app_handle: Option<tauri::AppHandle>,
        current_provider_id_at_start: String,
        session_id: String,
        session_client_provided: bool,
        streaming_first_byte_timeout: u64,
        _streaming_idle_timeout: u64,
        rectifier_config: RectifierConfig,
        optimizer_config: OptimizerConfig,
        model_routing: super::model_routing::ModelRoutingConfig,
        max_retries: u32,
    ) -> Self {
        // max_retries 是「失败后重试次数」语义，attempt 上限 = retries + 1。
        // saturating_add 防止 u32::MAX + 1 溢出。
        let max_attempts = (max_retries as usize).saturating_add(1);
        Self {
            router,
            status,
            current_providers,
            gemini_shadow,
            codex_chat_history,
            failover_manager,
            app_handle,
            current_provider_id_at_start,
            session_id,
            session_client_provided,
            rectifier_config,
            optimizer_config,
            model_routing,
            non_streaming_timeout: std::time::Duration::from_secs(non_streaming_timeout),
            streaming_first_byte_timeout: std::time::Duration::from_secs(
                streaming_first_byte_timeout,
            ),
            max_attempts,
            anyrouter_429_max_retries: ANYROUTER_RATE_LIMIT_MAX_RETRIES,
            anyrouter_429_total_wait_ms: ANYROUTER_RATE_LIMIT_TOTAL_WAIT_MS,
        }
    }

    /// 同通道"改体重试"成功后的统一记账：记录通道成功、刷新当前供应商、
    /// 维护成功率与故障转移切换通知，组装最终 ForwardResult。
    ///（媒体降级重试 / anyrouter Codex 兼容重试共用）
    #[allow(clippy::too_many_arguments)]
    async fn finalize_same_provider_retry_success(
        &self,
        response: ProxyResponse,
        claude_api_format: Option<String>,
        outbound_model: Option<String>,
        provider: &Provider,
        attempt_key_id: Option<String>,
        key_id: Option<&str>,
        app_type_str: &str,
        used_half_open_permit: bool,
    ) -> ForwardResult {
        self.record_success_result(&provider.id, key_id, app_type_str, used_half_open_permit)
            .await;

        {
            let mut current_providers = self.current_providers.write().await;
            current_providers.insert(
                app_type_str.to_string(),
                (provider.id.clone(), provider.name.clone()),
            );
        }

        {
            let mut status = self.status.write().await;
            status.success_requests += 1;
            status.last_error = None;
            let should_switch = self.current_provider_id_at_start.as_str() != provider.id.as_str();
            if should_switch {
                status.failover_count += 1;
                let fm = self.failover_manager.clone();
                let ah = self.app_handle.clone();
                let pid = provider.id.clone();
                let pname = provider.name.clone();
                let at = app_type_str.to_string();

                tokio::spawn(async move {
                    let _ = fm.try_switch(ah.as_ref(), &at, &pid, &pname).await;
                });
            }
            if status.total_requests > 0 {
                status.success_rate =
                    (status.success_requests as f32 / status.total_requests as f32) * 100.0;
            }
        }

        ForwardResult {
            response,
            provider: provider.clone(),
            key_id: attempt_key_id,
            claude_api_format,
            outbound_model,
            connection_guard: None,
        }
    }

    async fn record_success_result(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        // HalfOpen permit 释放必须同步完成：它归还熔断器的探测名额并推进
        // HalfOpen → Closed 状态机，挪到后台会让紧随其后的请求误判通道仍在
        // 探测中而被拒绝。
        if used_half_open_permit {
            if let Err(e) = self
                .router
                .record_channel_result(provider_id, key_id, app_type, true, true, None)
                .await
            {
                log::warn!(
                    "[{app_type}] 记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                );
            }
        }

        // 其余记账（亲和绑定、key 成功、通道健康度）都是事后簿记，不影响本次
        // 响应的正确性 —— 整体挪到后台，避免 2-3 次同步 DB 写叠加在流式响应
        // 的首包延迟上。
        let router = self.router.clone();
        let provider_id = provider_id.to_string();
        let key_id = key_id.map(str::to_string);
        let app_type = app_type.to_string();
        let session_id = self.session_id.clone();
        let session_client_provided = self.session_client_provided;
        tokio::spawn(async move {
            if let Err(e) = router
                .bind_working_channel_affinity(&app_type, &provider_id, key_id.as_deref())
                .await
            {
                log::warn!(
                    "[{app_type}] 记录工作通道偏好失败: provider_id={provider_id}, key_id={key_id:?}, error={e}"
                );
            }

            if session_client_provided {
                if let Err(e) = router
                    .bind_session_affinity(&app_type, &session_id, &provider_id, key_id.as_deref())
                    .await
                {
                    log::warn!(
                        "[{app_type}] 记录 Session Affinity 失败: provider_id={provider_id}, key_id={key_id:?}, session_id={session_id}, error={e}"
                    );
                }
            }

            if let Some(key_id) = key_id.as_deref() {
                if let Err(e) = router
                    .record_key_success(&provider_id, &app_type, key_id)
                    .await
                {
                    log::warn!(
                        "[{app_type}] 记录 Provider Key 成功结果失败: provider_id={provider_id}, key_id={key_id}, error={e}"
                    );
                }
            }

            if !used_half_open_permit {
                if let Err(e) = router
                    .record_channel_result(
                        &provider_id,
                        key_id.as_deref(),
                        &app_type,
                        false,
                        true,
                        None,
                    )
                    .await
                {
                    log::warn!(
                        "[{app_type}] 异步记录 Provider 成功结果失败: provider_id={provider_id}, error={e}"
                    );
                }
            }
        });
    }

    /// 整流（thinking signature 或 budget）重试失败后的统一收尾。
    ///
    /// `None` 表示已记录熔断器、累积 `last_error`/`last_provider`，
    /// 调用方应 `continue` 让下一家 provider 继续故障转移；
    /// `Some(ForwardError)` 表示是客户端错误，没有 provider 能修复，
    /// 调用方应直接 `return` 把错误返回给客户端。
    #[allow(clippy::too_many_arguments)]
    async fn handle_rectifier_retry_failure(
        &self,
        retry_err: ProxyError,
        provider: &Provider,
        key_id: Option<&str>,
        app_type_str: &str,
        used_half_open_permit: bool,
        rectifier_label: &str,
        last_error: &mut Option<ProxyError>,
        last_provider: &mut Option<Provider>,
    ) -> Option<ForwardError> {
        // Provider 错误：本家上游/网络确实出问题，下一家 provider 可能可用 → 继续故障转移。
        // 客户端错误：整流后请求仍违法，下一家也修不好 → 直接返回。
        let is_provider_error = match &retry_err {
            ProxyError::Timeout(_) | ProxyError::ForwardFailed(_) => true,
            ProxyError::UpstreamError { status, .. } => *status >= 500,
            _ => false,
        };

        if is_provider_error {
            if key_id.is_some() {
                // key 通道：失败只冷却该 key（指数退避），不污染 provider 健康度
                self.router
                    .release_channel_permit_neutral(
                        &provider.id,
                        key_id,
                        app_type_str,
                        used_half_open_permit,
                    )
                    .await;
                self.record_key_failure_for_error(provider, key_id, app_type_str, &retry_err)
                    .await;
            } else {
                let _ = self
                    .router
                    .record_channel_result(
                        &provider.id,
                        key_id,
                        app_type_str,
                        used_half_open_permit,
                        false,
                        Some(retry_err.to_string()),
                    )
                    .await;
            }
            {
                let mut status = self.status.write().await;
                status.last_error = Some(format!(
                    "Provider {} {rectifier_label}重试失败: {}",
                    provider.name, retry_err
                ));
            }
            *last_error = Some(retry_err);
            *last_provider = Some(provider.clone());
            return None;
        }

        self.router
            .release_channel_permit_neutral(
                &provider.id,
                key_id,
                app_type_str,
                used_half_open_permit,
            )
            .await;
        let mut status = self.status.write().await;
        status.failed_requests += 1;
        status.last_error = Some(retry_err.to_string());
        if status.total_requests > 0 {
            status.success_rate =
                (status.success_requests as f32 / status.total_requests as f32) * 100.0;
        }
        Some(ForwardError {
            error: retry_err,
            provider: Some(provider.clone()),
            key_id: key_id.map(str::to_string),
        })
    }

    async fn record_key_failure_for_error(
        &self,
        provider: &Provider,
        key_id: Option<&str>,
        app_type_str: &str,
        error: &ProxyError,
    ) {
        if let Err(e) = self
            .router
            .clear_working_channel_affinity_if_matches(app_type_str, &provider.id, key_id)
            .await
        {
            log::warn!(
                "[{app_type_str}] 清除工作通道偏好失败: provider_id={}, key_id={key_id:?}, error={}",
                provider.id,
                e
            );
        }
        if let Err(e) = self
            .router
            .clear_session_affinity_if_matches(app_type_str, &provider.id, key_id)
            .await
        {
            log::warn!(
                "[{app_type_str}] 清除 Session 通道偏好失败: provider_id={}, key_id={key_id:?}, error={}",
                provider.id,
                e
            );
        }

        let Some(key_id) = key_id else {
            return;
        };
        let (cooldown_base, cooldown_cap, grace_failures) = key_failure_cooldown(error);
        if let Err(e) = self
            .router
            .record_key_failure(
                &provider.id,
                app_type_str,
                key_id,
                cooldown_base,
                cooldown_cap,
                grace_failures,
            )
            .await
        {
            log::warn!(
                "[{app_type_str}] 记录 Provider Key 失败结果失败: provider_id={}, key_id={}, error={}",
                provider.id,
                key_id,
                e
            );
        }
    }

    /// 转发请求（带故障转移）
    ///
    /// 这是 thin wrapper：在客户端请求维度记一次 `total_requests` / 调整
    /// `active_connections` / 刷新 `last_request_at`，无论 inner 走哪条出口路径，
    /// 出口处都会把 `active_connections` 回收。Per-attempt 维度（成功/失败/熔断
    /// 等）仍由 inner 内自行更新 `success_requests` / `failed_requests`。
    #[allow(clippy::too_many_arguments)]
    pub async fn forward_with_retry(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: Value,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: Vec<ProviderAttempt>,
    ) -> Result<ForwardResult, ForwardError> {
        let guard = ActiveConnectionGuard::acquire(self.status.clone()).await;
        {
            let mut s = self.status.write().await;
            s.total_requests = s.total_requests.saturating_add(1);
            s.last_request_at = Some(chrono::Utc::now().to_rfc3339());
        }
        let result = self
            .forward_with_retry_inner(
                app_type, method, endpoint, body, headers, extensions, providers,
            )
            .await;
        // 把 guard 注入到 Ok 结果，让它随响应一起流转到 response_processor，
        // 在流式 body 的 future 内才真正 drop。
        // Err 路径：guard 在函数 scope 内随返回值落地时自动 drop。
        result.map(|mut fr| {
            fr.connection_guard = Some(guard);
            fr
        })
    }

    /// 实际转发逻辑（不包含客户端维度的入口/出口计数）
    ///
    /// # Arguments
    /// * `app_type` - 应用类型
    /// * `method` - 客户端请求的 HTTP 方法（透传给上游，支持 GET/POST 等）
    /// * `endpoint` - API 端点
    /// * `body` - 请求体
    /// * `headers` - 请求头
    /// * `providers` - 已选择的 Provider 列表（由 RequestContext 提供，避免重复调用 select_providers）
    #[allow(clippy::too_many_arguments)]
    async fn forward_with_retry_inner(
        &self,
        app_type: &AppType,
        method: http::Method,
        endpoint: &str,
        body: Value,
        headers: axum::http::HeaderMap,
        extensions: Extensions,
        providers: Vec<ProviderAttempt>,
    ) -> Result<ForwardResult, ForwardError> {
        // 获取适配器
        let adapter = get_adapter(app_type);
        let app_type_str = app_type.as_str();

        if providers.is_empty() {
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
                key_id: None,
            });
        }

        let mut last_error = None;
        let mut last_provider = None;
        let mut last_key_id = None;
        let mut attempted_channels = 0usize;
        let mut attempted_channel_ids = HashSet::new();
        let mut provider_blocked_by_failure = HashSet::new();
        // anyrouter 429 同通道重试预算：独立于 max_retries 设置，整个请求内共享
        //（多 key 轮转时各 key 的原地重试共用同一份预算，防止预算被放大 N 倍）。
        let mut anyrouter_429_retry_budget = self.anyrouter_429_max_retries;
        let mut anyrouter_429_wait_budget_ms = self.anyrouter_429_total_wait_ms;

        let total_channel_count = providers
            .iter()
            .map(|attempt| attempt.channel_id())
            .collect::<HashSet<_>>()
            .len();
        // 单通道场景下跳过熔断器检查（故障转移关闭时常见）。
        let bypass_circuit_breaker = total_channel_count == 1;

        // 依次尝试每个供应商 / Key。预算按通道去重统计；
        // 有 key 池时，每个 key 与单独 Provider 一样占用一次尝试预算。
        for attempt in providers.iter() {
            let provider = &attempt.provider;
            let key_id = attempt.key_id.as_deref();
            let channel_id = attempt.channel_id();
            if provider_blocked_by_failure.contains(&provider.id) {
                continue;
            }
            // 整流器重试标记：每个 provider 独立持有，避免标记跨 provider 短路故障转移
            // —— 首家 provider 整流后被 5xx/timeout 击落时，下家仍能用整流后的请求体走整流流程
            let mut rectifier_retried = false;
            let mut budget_rectifier_retried = false;
            let mut media_rectifier_retried = false;
            let mut anyrouter_codex_retried = false;

            // 上限检查：尊重用户在 AppProxyConfig.max_retries 上配置的「重试次数」。
            // 放在熔断器 allow 检查之前，避免在已经超限时还占用 HalfOpen 探测名额。
            let is_new_channel_attempt = !attempted_channel_ids.contains(&channel_id);
            let beyond_max_attempts =
                is_new_channel_attempt && attempted_channels >= self.max_attempts;

            if beyond_max_attempts {
                log::warn!(
                    "[{app_type_str}] 已达最大尝试次数上限 ({}/{}), 停止故障转移",
                    attempted_channels,
                    self.max_attempts
                );
                break;
            }

            // 发起请求前先获取熔断器放行许可（HalfOpen 会占用探测名额）
            // 单 Provider 场景下跳过此检查，避免熔断器阻塞所有请求
            let (allowed, used_half_open_permit) = if bypass_circuit_breaker {
                (true, false)
            } else {
                let permit = self
                    .router
                    .allow_channel_request(&provider.id, key_id, app_type_str)
                    .await;
                (permit.allowed, permit.used_half_open_permit)
            };

            if !allowed {
                continue;
            }

            // PRE-SEND 优化器：每个 provider 独立决定是否优化
            // clone body 以避免 Bedrock 优化字段泄漏到非 Bedrock provider（failover 场景）
            let mut provider_body =
                if self.optimizer_config.enabled && is_bedrock_provider(provider) {
                    let mut b = body.clone();
                    if self.optimizer_config.thinking_optimizer {
                        super::thinking_optimizer::optimize(&mut b, &self.optimizer_config);
                    }
                    if self.optimizer_config.cache_injection {
                        super::cache_injector::inject(&mut b, &self.optimizer_config);
                    }
                    b
                } else {
                    body.clone()
                };

            if attempted_channel_ids.insert(channel_id) {
                attempted_channels += 1;
            }

            // 更新状态中的当前 Provider 信息（per-attempt 维度的标识）
            //
            // total_requests / last_request_at / active_connections 已由
            // forward_with_retry wrapper 在客户端请求维度统一处理，这里只刷
            // 新「正在尝试哪个 provider」的展示字段。
            {
                let mut status = self.status.write().await;
                status.current_provider = Some(provider.name.clone());
                status.current_provider_id = Some(provider.id.clone());
            }

            // 转发请求（每个 Provider 只尝试一次，重试由客户端控制）
            match self
                .forward(
                    app_type,
                    &method,
                    provider,
                    endpoint,
                    &provider_body,
                    &headers,
                    &extensions,
                    adapter.as_ref(),
                )
                .await
            {
                Ok((response, claude_api_format, outbound_model)) => {
                    // 成功：普通闭合熔断状态异步记录，避免阻塞流式首包返回；
                    // HalfOpen 探测仍同步等待，保证 permit 与熔断状态及时释放。
                    self.record_success_result(
                        &provider.id,
                        key_id,
                        app_type_str,
                        used_half_open_permit,
                    )
                    .await;

                    // 更新当前应用类型使用的 provider
                    {
                        let mut current_providers = self.current_providers.write().await;
                        current_providers.insert(
                            app_type_str.to_string(),
                            (provider.id.clone(), provider.name.clone()),
                        );
                    }

                    // 更新成功统计
                    {
                        let mut status = self.status.write().await;
                        status.success_requests += 1;
                        status.last_error = None;
                        let should_switch =
                            self.current_provider_id_at_start.as_str() != provider.id.as_str();
                        if should_switch {
                            status.failover_count += 1;

                            // 异步触发供应商切换，更新 UI/托盘，并把“当前供应商”同步为实际使用的 provider
                            let fm = self.failover_manager.clone();
                            let ah = self.app_handle.clone();
                            let pid = provider.id.clone();
                            let pname = provider.name.clone();
                            let at = app_type_str.to_string();

                            tokio::spawn(async move {
                                let _ = fm.try_switch(ah.as_ref(), &at, &pid, &pname).await;
                            });
                        }
                        // 重新计算成功率
                        if status.total_requests > 0 {
                            status.success_rate = (status.success_requests as f32
                                / status.total_requests as f32)
                                * 100.0;
                        }
                    }

                    return Ok(ForwardResult {
                        response,
                        provider: provider.clone(),
                        key_id: attempt.key_id.clone(),
                        claude_api_format,
                        outbound_model,
                        connection_guard: None,
                    });
                }
                Err(mut e) => {
                    // anyrouter 429 限流特判：同通道高强度重试，预算独立于
                    // AppProxyConfig.max_retries（该设置只管"换几家"，不管这里）。
                    // 重试间隔尊重上游 Retry-After（封顶），否则短退避线性爬升。
                    // anyrouter 限流是分钟级窗口 + 后端轮转，冷却/熔断只会把
                    // 大概率马上恢复的通道踢出轮转，因此全程不标记冷却。
                    let is_anyrouter = is_anyrouter_channel(
                        &provider.name,
                        &adapter.extract_base_url(provider).unwrap_or_default(),
                    );
                    if is_anyrouter && is_rate_limited_upstream_error(&e) {
                        let mut retry_round: u64 = 0;
                        while anyrouter_429_retry_budget > 0 && is_rate_limited_upstream_error(&e)
                        {
                            let wait_ms = match &e {
                                ProxyError::UpstreamError {
                                    retry_after: Some(ra),
                                    ..
                                } => ra
                                    .saturating_mul(1000)
                                    .min(ANYROUTER_RATE_LIMIT_RETRY_AFTER_CAP_SECS * 1000),
                                _ => (ANYROUTER_RATE_LIMIT_RETRY_BASE_MS * (retry_round + 1))
                                    .min(ANYROUTER_RATE_LIMIT_RETRY_MAX_MS),
                            };
                            if wait_ms > anyrouter_429_wait_budget_ms {
                                log::warn!(
                                    "[{app_type_str}] [anyrouter] 429 重试等待预算耗尽，转入故障转移: provider={}",
                                    provider.name
                                );
                                break;
                            }
                            anyrouter_429_wait_budget_ms -= wait_ms;
                            anyrouter_429_retry_budget -= 1;
                            retry_round += 1;
                            log::info!(
                                "[{app_type_str}] [anyrouter] 上游 429 限流，{wait_ms}ms 后同通道重试（第 {retry_round} 次，剩余预算 {anyrouter_429_retry_budget}）: provider={}",
                                provider.name
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;

                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &provider_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format, outbound_model)) => {
                                    log::info!(
                                        "[{app_type_str}] [anyrouter] 429 重试成功（共重试 {retry_round} 次）: provider={}",
                                        provider.name
                                    );
                                    return Ok(self
                                        .finalize_same_provider_retry_success(
                                            response,
                                            claude_api_format,
                                            outbound_model,
                                            provider,
                                            attempt.key_id.clone(),
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                        )
                                        .await);
                                }
                                Err(retry_err) => {
                                    e = retry_err;
                                }
                            }
                        }
                        // 仍是 429：落入下方通用分类，但不标记冷却（见
                        // skip_failure_marking）；变成其他错误则按正常轨道处理。
                    }

                    // 检测是否需要触发整流器（仅 Claude/ClaudeAuth 供应商）
                    let provider_type = ProviderType::from_app_type_and_config(app_type, provider);
                    let is_anthropic_provider = matches!(
                        provider_type,
                        ProviderType::Claude | ProviderType::ClaudeAuth
                    );
                    let mut signature_rectifier_non_retryable_client_error = false;

                    if self.media_retry_should_trigger(
                        adapter.name(),
                        media_rectifier_retried,
                        &provider_body,
                        &e,
                    ) {
                        let mut media_body = provider_body.clone();
                        let replaced_images =
                            super::media_sanitizer::replace_image_blocks_with_marker(
                                &mut media_body,
                            );

                        if replaced_images > 0 {
                            let _ = std::mem::replace(&mut media_rectifier_retried, true);
                            let model = media_body
                                .get("model")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            log::info!(
                                "[{app_type_str}] [Media] Upstream rejected image input; retrying provider={} model={} with {replaced_images} image block(s) replaced by {}",
                                provider.id,
                                model,
                                super::media_sanitizer::UNSUPPORTED_IMAGE_MARKER
                            );

                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &media_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format, outbound_model)) => {
                                    log::info!(
                                        "[{app_type_str}] [Media] Unsupported-image retry succeeded"
                                    );
                                    return Ok(self
                                        .finalize_same_provider_retry_success(
                                            response,
                                            claude_api_format,
                                            outbound_model,
                                            provider,
                                            attempt.key_id.clone(),
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                        )
                                        .await);
                                }
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [Media] Unsupported-image retry still failed: {retry_err}"
                                    );
                                    if let Some(err) = self
                                        .handle_rectifier_retry_failure(
                                            retry_err,
                                            provider,
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                            "media 降级",
                                            &mut last_error,
                                            &mut last_provider,
                                        )
                                        .await
                                    {
                                        return Err(err);
                                    }
                                    if key_id.is_none() {
                                        provider_blocked_by_failure.insert(provider.id.clone());
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    // anyrouter Codex 兼容重试：上游 400 invalid_responses_request 表示
                    // 该端点不接受 OpenAI 的 encrypted reasoning content / tool_search_*，
                    // 剥掉这些字段后同通道重试一次（ccLoad 同款适配）
                    if !anyrouter_codex_retried
                        && adapter.name() == "Codex"
                        && is_invalid_responses_request_error(&e)
                        && is_anyrouter_channel(
                            &provider.name,
                            &adapter.extract_base_url(provider).unwrap_or_default(),
                        )
                    {
                        if let Some(stripped_body) =
                            codex_body_without_encrypted_and_tool_search(&provider_body)
                        {
                            let _ = std::mem::replace(&mut anyrouter_codex_retried, true);
                            log::info!(
                                "[{app_type_str}] [anyrouter] 上游拒绝 Responses 请求（invalid_responses_request），剥除 encrypted_content/tool_search 后重试: provider={}",
                                provider.name
                            );
                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &stripped_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format, outbound_model)) => {
                                    log::info!("[{app_type_str}] [anyrouter] Codex 兼容重试成功");
                                    return Ok(self
                                        .finalize_same_provider_retry_success(
                                            response,
                                            claude_api_format,
                                            outbound_model,
                                            provider,
                                            attempt.key_id.clone(),
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                        )
                                        .await);
                                }
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [anyrouter] Codex 兼容重试仍失败: {retry_err}"
                                    );
                                    if let Some(err) = self
                                        .handle_rectifier_retry_failure(
                                            retry_err,
                                            provider,
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                            "anyrouter Codex 兼容",
                                            &mut last_error,
                                            &mut last_provider,
                                        )
                                        .await
                                    {
                                        return Err(err);
                                    }
                                    if key_id.is_none() {
                                        provider_blocked_by_failure.insert(provider.id.clone());
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    if is_anthropic_provider {
                        let error_message = extract_error_message(&e);
                        if should_rectify_thinking_signature(
                            error_message.as_deref(),
                            &self.rectifier_config,
                        ) {
                            // 已经重试过：直接返回错误（不可重试客户端错误）
                            if rectifier_retried {
                                log::warn!("[{app_type_str}] [RECT-005] 整流器已触发过，不再重试");
                                // 释放 HalfOpen permit（不记录熔断器，这是客户端兼容性问题）
                                self.router
                                    .release_channel_permit_neutral(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                    key_id: attempt.key_id.clone(),
                                });
                            }

                            // 首次触发：整流请求体
                            let rectified = rectify_anthropic_request(&mut provider_body);

                            // 整流未生效：继续尝试 budget 整流路径，避免误判后短路
                            if !rectified.applied {
                                log::warn!(
                                    "[{app_type_str}] [RECT-006] thinking 签名整流器触发但无可整流内容，继续检查 budget；若 budget 也未命中则按客户端错误返回"
                                );
                                signature_rectifier_non_retryable_client_error = true;
                            } else {
                                log::info!(
                                    "[{}] [RECT-001] thinking 签名整流器触发, 移除 {} thinking blocks, {} redacted_thinking blocks, {} signature fields",
                                    app_type_str,
                                    rectified.removed_thinking_blocks,
                                    rectified.removed_redacted_thinking_blocks,
                                    rectified.removed_signature_fields
                                );

                                // 标记已重试（当前逻辑下重试后必定 return，保留标记以备将来扩展）
                                let _ = std::mem::replace(&mut rectifier_retried, true);

                                // 使用同一供应商重试（不计入熔断器）
                                match self
                                    .forward(
                                        app_type,
                                        &method,
                                        provider,
                                        endpoint,
                                        &provider_body,
                                        &headers,
                                        &extensions,
                                        adapter.as_ref(),
                                    )
                                    .await
                                {
                                    Ok((response, claude_api_format, outbound_model)) => {
                                        log::info!("[{app_type_str}] [RECT-002] 整流重试成功");
                                        self.record_success_result(
                                            &provider.id,
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                        )
                                        .await;

                                        // 更新当前应用类型使用的 provider
                                        {
                                            let mut current_providers =
                                                self.current_providers.write().await;
                                            current_providers.insert(
                                                app_type_str.to_string(),
                                                (provider.id.clone(), provider.name.clone()),
                                            );
                                        }

                                        // 更新成功统计
                                        {
                                            let mut status = self.status.write().await;
                                            status.success_requests += 1;
                                            status.last_error = None;
                                            let should_switch =
                                                self.current_provider_id_at_start.as_str()
                                                    != provider.id.as_str();
                                            if should_switch {
                                                status.failover_count += 1;

                                                // 异步触发供应商切换，更新 UI/托盘
                                                let fm = self.failover_manager.clone();
                                                let ah = self.app_handle.clone();
                                                let pid = provider.id.clone();
                                                let pname = provider.name.clone();
                                                let at = app_type_str.to_string();

                                                tokio::spawn(async move {
                                                    let _ = fm
                                                        .try_switch(ah.as_ref(), &at, &pid, &pname)
                                                        .await;
                                                });
                                            }
                                            if status.total_requests > 0 {
                                                status.success_rate = (status.success_requests
                                                    as f32
                                                    / status.total_requests as f32)
                                                    * 100.0;
                                            }
                                        }

                                        return Ok(ForwardResult {
                                            response,
                                            provider: provider.clone(),
                                            key_id: attempt.key_id.clone(),
                                            claude_api_format,
                                            outbound_model,
                                            connection_guard: None,
                                        });
                                    }
                                    Err(retry_err) => {
                                        log::warn!(
                                            "[{app_type_str}] [RECT-003] 整流重试仍失败: {retry_err}"
                                        );
                                        if let Some(err) = self
                                            .handle_rectifier_retry_failure(
                                                retry_err,
                                                provider,
                                                key_id,
                                                app_type_str,
                                                used_half_open_permit,
                                                "整流",
                                                &mut last_error,
                                                &mut last_provider,
                                            )
                                            .await
                                        {
                                            return Err(err);
                                        }
                                        if key_id.is_none() {
                                            provider_blocked_by_failure
                                                .insert(provider.id.clone());
                                        }
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    // 检测是否需要触发 budget 整流器（仅 Claude/ClaudeAuth 供应商）
                    if is_anthropic_provider {
                        let error_message = extract_error_message(&e);
                        if should_rectify_thinking_budget(
                            error_message.as_deref(),
                            &self.rectifier_config,
                        ) {
                            // 已经重试过：直接返回错误（不可重试客户端错误）
                            if budget_rectifier_retried {
                                log::warn!(
                                    "[{app_type_str}] [RECT-013] budget 整流器已触发过，不再重试"
                                );
                                self.router
                                    .release_channel_permit_neutral(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                    key_id: attempt.key_id.clone(),
                                });
                            }

                            let budget_rectified = rectify_thinking_budget(&mut provider_body);
                            if !budget_rectified.applied {
                                log::warn!(
                                    "[{app_type_str}] [RECT-014] budget 整流器触发但无可整流内容，不做无意义重试"
                                );
                                self.router
                                    .release_channel_permit_neutral(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                                return Err(ForwardError {
                                    error: e,
                                    provider: Some(provider.clone()),
                                    key_id: attempt.key_id.clone(),
                                });
                            }

                            log::info!(
                                "[{}] [RECT-010] thinking budget 整流器触发, before={:?}, after={:?}",
                                app_type_str,
                                budget_rectified.before,
                                budget_rectified.after
                            );

                            let _ = std::mem::replace(&mut budget_rectifier_retried, true);

                            // 使用同一供应商重试（不计入熔断器）
                            match self
                                .forward(
                                    app_type,
                                    &method,
                                    provider,
                                    endpoint,
                                    &provider_body,
                                    &headers,
                                    &extensions,
                                    adapter.as_ref(),
                                )
                                .await
                            {
                                Ok((response, claude_api_format, outbound_model)) => {
                                    log::info!("[{app_type_str}] [RECT-011] budget 整流重试成功");
                                    self.record_success_result(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;

                                    {
                                        let mut current_providers =
                                            self.current_providers.write().await;
                                        current_providers.insert(
                                            app_type_str.to_string(),
                                            (provider.id.clone(), provider.name.clone()),
                                        );
                                    }

                                    {
                                        let mut status = self.status.write().await;
                                        status.success_requests += 1;
                                        status.last_error = None;
                                        let should_switch =
                                            self.current_provider_id_at_start.as_str()
                                                != provider.id.as_str();
                                        if should_switch {
                                            status.failover_count += 1;
                                            let fm = self.failover_manager.clone();
                                            let ah = self.app_handle.clone();
                                            let pid = provider.id.clone();
                                            let pname = provider.name.clone();
                                            let at = app_type_str.to_string();
                                            tokio::spawn(async move {
                                                let _ = fm
                                                    .try_switch(ah.as_ref(), &at, &pid, &pname)
                                                    .await;
                                            });
                                        }
                                        if status.total_requests > 0 {
                                            status.success_rate = (status.success_requests as f32
                                                / status.total_requests as f32)
                                                * 100.0;
                                        }
                                    }

                                    return Ok(ForwardResult {
                                        response,
                                        provider: provider.clone(),
                                        key_id: attempt.key_id.clone(),
                                        claude_api_format,
                                        outbound_model,
                                        connection_guard: None,
                                    });
                                }
                                Err(retry_err) => {
                                    log::warn!(
                                        "[{app_type_str}] [RECT-012] budget 整流重试仍失败: {retry_err}"
                                    );
                                    if let Some(err) = self
                                        .handle_rectifier_retry_failure(
                                            retry_err,
                                            provider,
                                            key_id,
                                            app_type_str,
                                            used_half_open_permit,
                                            "budget 整流",
                                            &mut last_error,
                                            &mut last_provider,
                                        )
                                        .await
                                    {
                                        return Err(err);
                                    }
                                    if key_id.is_none() {
                                        provider_blocked_by_failure.insert(provider.id.clone());
                                    }
                                    continue;
                                }
                            }
                        }
                    }

                    if signature_rectifier_non_retryable_client_error {
                        self.router
                            .release_channel_permit_neutral(
                                &provider.id,
                                key_id,
                                app_type_str,
                                used_half_open_permit,
                            )
                            .await;
                        let mut status = self.status.write().await;
                        status.failed_requests += 1;
                        status.last_error = Some(e.to_string());
                        if status.total_requests > 0 {
                            status.success_rate = (status.success_requests as f32
                                / status.total_requests as f32)
                                * 100.0;
                        }
                        return Err(ForwardError {
                            error: e,
                            provider: Some(provider.clone()),
                            key_id: attempt.key_id.clone(),
                        });
                    }

                    // 先分类错误，决定是否计入 provider 健康度
                    // —— NonRetryable / ClientAbort 是客户端层错误，无论换哪家 provider 都会被拒绝，
                    //    不应污染熔断器和数据库健康度（与 release_permit_neutral 同语义）。
                    let category = self.categorize_proxy_error(&e);
                    let key_scoped_retry = key_id.is_some() && is_key_scoped_error(&e);
                    // anyrouter 429：限流不是通道/key 故障，不计入冷却与熔断健康度，
                    // 也不清除亲和（下一请求应继续优先命中该通道）。
                    let skip_failure_marking = is_anyrouter && is_rate_limited_upstream_error(&e);

                    match category {
                        ErrorCategory::Retryable if key_id.is_some() => {
                            // key 通道：任何可重试失败（401/429/5xx/超时）都只冷却该
                            // key（指数退避），同 provider 的其他 key 继续参与调度，
                            // 不连坐 provider（ccLoad 风格的渠道级隔离）。
                            self.router
                                .release_channel_permit_neutral(
                                    &provider.id,
                                    key_id,
                                    app_type_str,
                                    used_half_open_permit,
                                )
                                .await;
                            if skip_failure_marking {
                                log::info!(
                                    "[{app_type_str}] [anyrouter] 429 不计入 key 冷却: provider={}, key_id={key_id:?}",
                                    provider.name
                                );
                            } else {
                                self.record_key_failure_for_error(
                                    provider,
                                    key_id,
                                    app_type_str,
                                    &e,
                                )
                                .await;
                            }
                            {
                                let mut status = self.status.write().await;
                                status.last_error =
                                    Some(format!("Provider {} key failed: {}", provider.name, e));
                            }
                            last_error = Some(e);
                            last_provider = Some(provider.clone());
                            last_key_id = attempt.key_id.clone();
                            continue;
                        }
                        ErrorCategory::Retryable => {
                            // 可重试：真正的 provider 故障 → 记录失败并更新熔断器/DB 健康度
                            //（anyrouter 429 例外：限流不是通道故障，仅中性释放 permit）
                            if skip_failure_marking {
                                log::info!(
                                    "[{app_type_str}] [anyrouter] 429 不计入通道冷却/熔断: provider={}",
                                    provider.name
                                );
                                self.router
                                    .release_channel_permit_neutral(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                    )
                                    .await;
                            } else {
                                let _ = self
                                    .router
                                    .record_channel_result(
                                        &provider.id,
                                        key_id,
                                        app_type_str,
                                        used_half_open_permit,
                                        false,
                                        Some(e.to_string()),
                                    )
                                    .await;
                            }

                            {
                                let mut status = self.status.write().await;
                                status.last_error =
                                    Some(format!("Provider {} 失败: {}", provider.name, e));
                            }

                            let (log_code, log_message) = build_retryable_failure_log(
                                &provider.name,
                                attempted_channels,
                                total_channel_count,
                                &e,
                            );
                            log::warn!("[{app_type_str}] [{log_code}] {log_message}");

                            last_error = Some(e);
                            last_provider = Some(provider.clone());
                            last_key_id = attempt.key_id.clone();
                            provider_blocked_by_failure.insert(provider.id.clone());
                            // 继续尝试下一个供应商
                            continue;
                        }
                        ErrorCategory::NonRetryable if key_scoped_retry => {
                            self.router
                                .release_channel_permit_neutral(
                                    &provider.id,
                                    key_id,
                                    app_type_str,
                                    used_half_open_permit,
                                )
                                .await;
                            self.record_key_failure_for_error(provider, key_id, app_type_str, &e)
                                .await;
                            {
                                let mut status = self.status.write().await;
                                status.last_error =
                                    Some(format!("Provider {} key failed: {}", provider.name, e));
                            }
                            last_error = Some(e);
                            last_provider = Some(provider.clone());
                            last_key_id = attempt.key_id.clone();
                            continue;
                        }
                        ErrorCategory::NonRetryable | ErrorCategory::ClientAbort => {
                            // 不可重试：客户端层错误或客户端断连 → 不污染健康度，仅释放 HalfOpen permit
                            self.router
                                .release_channel_permit_neutral(
                                    &provider.id,
                                    key_id,
                                    app_type_str,
                                    used_half_open_permit,
                                )
                                .await;
                            {
                                let mut status = self.status.write().await;
                                status.failed_requests += 1;
                                status.last_error = Some(e.to_string());
                                if status.total_requests > 0 {
                                    status.success_rate = (status.success_requests as f32
                                        / status.total_requests as f32)
                                        * 100.0;
                                }
                            }
                            return Err(ForwardError {
                                error: e,
                                provider: Some(provider.clone()),
                                key_id: attempt.key_id.clone(),
                            });
                        }
                    }
                }
            }
        }

        if attempted_channels == 0 {
            // providers 列表非空，但全部通道都被熔断器拒绝（典型：HalfOpen 探测名额被占用）
            {
                let mut status = self.status.write().await;
                status.failed_requests += 1;
                status.last_error = Some("所有供应商暂时不可用（熔断器限制）".to_string());
                if status.total_requests > 0 {
                    status.success_rate =
                        (status.success_requests as f32 / status.total_requests as f32) * 100.0;
                }
            }
            return Err(ForwardError {
                error: ProxyError::NoAvailableProvider,
                provider: None,
                key_id: None,
            });
        }

        // 所有供应商都失败了
        {
            let mut status = self.status.write().await;
            status.failed_requests += 1;
            status.last_error = Some("所有供应商都失败".to_string());
            if status.total_requests > 0 {
                status.success_rate =
                    (status.success_requests as f32 / status.total_requests as f32) * 100.0;
            }
        }

        if let Some((log_code, log_message)) =
            build_terminal_failure_log(attempted_channels, total_channel_count, last_error.as_ref())
        {
            log::warn!("[{app_type_str}] [{log_code}] {log_message}");
        }

        Err(ForwardError {
            error: last_error.unwrap_or(ProxyError::MaxRetriesExceeded),
            provider: last_provider,
            key_id: last_key_id,
        })
    }

    /// 转发单个请求（使用适配器）
    ///
    /// 成功时返回 `(response, claude_api_format, outbound_model)`；`outbound_model`
    /// 是所有模型映射/请求体转换完成后实际发送给上游的模型名。
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        app_type: &AppType,
        method: &http::Method,
        provider: &Provider,
        endpoint: &str,
        body: &Value,
        headers: &axum::http::HeaderMap,
        extensions: &Extensions,
        adapter: &dyn ProviderAdapter,
    ) -> Result<(ProxyResponse, Option<String>, Option<String>), ProxyError> {
        // 使用适配器提取 base_url
        let base_url = adapter.extract_base_url(provider)?;

        // anyrouter 渠道检测：名称含 anyrouter，或 base_url 命中两个已知域名
        // （anyrouter.top / a-ocnfniawgw.cn-shanghai.fcapp.run）任一
        let is_anyrouter = is_anyrouter_channel(&provider.name, &base_url);

        let is_full_url = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.is_full_url)
            .unwrap_or(false);

        // 应用模型映射（独立于格式转换）
        // Claude Desktop proxy 模式必须先把 Desktop 可见的 claude-* route
        // 映射成真实上游模型名，并且未知 route 要直接报错，不能使用默认模型兜底。
        // 路由层模型映射在原始客户端模型上求解（见下方覆盖步骤），故先快照原始模型名。
        let original_request_model = body.get("model").and_then(|m| m.as_str()).map(str::to_string);
        let mapped_body = if matches!(app_type, AppType::ClaudeDesktop) {
            crate::claude_desktop_config::map_proxy_request_model(body.clone(), provider)
                .map_err(|e| ProxyError::InvalidRequest(e.to_string()))?
        } else {
            let (mapped_body, _original_model, _mapped_model) =
                super::model_mapper::apply_model_mapping(body.clone(), provider);
            mapped_body
        };

        // 与 CCH 对齐：请求前不做 thinking 主动改写（仅保留兼容入口）
        let mut mapped_body = normalize_thinking_type(mapped_body);

        mapped_body = super::model_mapper::strip_one_m_suffix_for_upstream_from_body(mapped_body);
        if matches!(app_type, AppType::Codex) {
            super::providers::apply_codex_upstream_model(provider, &mut mapped_body);
        }

        // 路由层模型映射（最终权威）：按客户端区分、对原始客户端模型求解，命中即用
        // 目标模型覆盖上面所有逻辑（catalog/env/pin）。未配置任何规则或未命中时不动，
        // 行为与现状完全一致。claude-desktop 走自身严格映射，不参与本步。
        if !matches!(app_type, AppType::ClaudeDesktop) {
            if let Some(original) = original_request_model.as_deref() {
                let rules =
                    super::model_routing::rules_for(&self.model_routing, app_type.as_str());
                if let Some(hit) = super::model_routing::resolve(rules, original) {
                    log::info!(
                        "[{}] [ModelRouting] {original} → {} (rule #{})",
                        app_type.as_str(),
                        hit.target,
                        hit.rule_index
                    );
                    mapped_body["model"] = serde_json::json!(hit.target);
                    mapped_body =
                        super::model_mapper::strip_one_m_suffix_for_upstream_from_body(mapped_body);
                }
            }
        }

        let resolved_claude_api_format = if adapter.name() == "Claude" {
            Some(super::providers::get_claude_api_format(provider).to_string())
        } else {
            None
        };
        if adapter.name() == "Claude" {
            if let Some(api_format) = resolved_claude_api_format.as_deref() {
                super::providers::normalize_anthropic_messages_for_provider(
                    &mut mapped_body,
                    provider,
                    api_format,
                );
                self.apply_media_prevention(&mut mapped_body, provider);

                // anyrouter：/v1/messages 未声明 thinking 时注入 adaptive
                //（上游要求显式 thinking.type=adaptive，缺失时行为不可预期）
                if is_anyrouter
                    && api_format == "anthropic"
                    && endpoint.split('?').next().unwrap_or(endpoint) == "/v1/messages"
                    && inject_adaptive_thinking_if_missing(&mut mapped_body)
                {
                    log::info!(
                        "[Claude] [anyrouter] 注入 thinking.type=adaptive: provider={}",
                        provider.name
                    );
                }
            }
        }
        let needs_transform = match resolved_claude_api_format.as_deref() {
            Some(api_format) => super::providers::claude_api_format_needs_transform(api_format),
            None => adapter.needs_transform(provider),
        };
        let codex_responses_to_chat = matches!(app_type, AppType::Codex)
            && super::providers::should_convert_codex_responses_to_chat(provider, endpoint);
        let (effective_endpoint, passthrough_query) = if codex_responses_to_chat {
            rewrite_codex_responses_endpoint_to_chat(endpoint)
        } else if needs_transform && adapter.name() == "Claude" {
            let api_format = resolved_claude_api_format
                .as_deref()
                .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
            rewrite_claude_transform_endpoint(endpoint, api_format, &mapped_body)
        } else {
            (
                endpoint.to_string(),
                split_endpoint_and_query(endpoint)
                    .1
                    .map(ToString::to_string),
            )
        };

        let codex_chat_base_is_full_endpoint = codex_responses_to_chat
            && base_url
                .trim_end_matches('/')
                .to_ascii_lowercase()
                .ends_with("/chat/completions");

        let url = if matches!(resolved_claude_api_format.as_deref(), Some("gemini_native")) {
            super::gemini_url::resolve_gemini_native_url(
                &base_url,
                &effective_endpoint,
                is_full_url,
            )
        } else if is_full_url || codex_chat_base_is_full_endpoint {
            append_query_to_full_url(&base_url, passthrough_query.as_deref())
        } else {
            adapter.build_url(&base_url, &effective_endpoint)
        };

        let mut outbound_model = mapped_body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
            .map(str::to_string);

        // 转换请求体（如果需要）
        let mut request_body = if codex_responses_to_chat {
            let mut mapped_body = mapped_body;
            let restored = self
                .codex_chat_history
                .enrich_request(&mut mapped_body)
                .await;
            if restored > 0 {
                log::debug!(
                    "[Codex] Restored {restored} cached function call(s) for Chat upstream"
                );
            }
            let reasoning_config =
                super::providers::resolve_codex_chat_reasoning_config(provider, &mapped_body);
            super::providers::transform_codex_chat::responses_to_chat_completions_with_reasoning(
                mapped_body,
                reasoning_config.as_ref(),
            )?
        } else if needs_transform {
            if adapter.name() == "Claude" {
                let api_format = resolved_claude_api_format
                    .as_deref()
                    .unwrap_or_else(|| super::providers::get_claude_api_format(provider));
                super::providers::transform_claude_request_for_api_format(
                    mapped_body,
                    provider,
                    api_format,
                    self.session_client_provided
                        .then_some(self.session_id.as_str()),
                    Some(self.gemini_shadow.as_ref()),
                )?
            } else {
                adapter.transform_request(mapped_body, provider)?
            }
        } else {
            mapped_body
        };

        // Codex /responses 路由到纯文本 OpenAI-chat 上游时，responses→chat 转换会把
        // input_image 变成上游拒绝的 image_url 块。Claude 适配器在转换前已做过预防性
        // 剥图（见上方），Codex 路径此前漏覆盖，故在转换后对最终请求体补一次。
        if matches!(app_type, AppType::Codex) {
            self.apply_media_prevention(&mut request_body, provider);
        }

        // 过滤私有参数（以 `_` 开头的字段），防止内部信息泄露到上游
        // 默认使用空白名单，过滤所有 _ 前缀字段
        let filtered_body = prepare_upstream_request_body(request_body);
        if let Some(model) = filtered_body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
        {
            outbound_model = Some(model.to_string());
        }
        log_prompt_cache_trace(
            app_type,
            provider,
            &effective_endpoint,
            resolved_claude_api_format.as_deref(),
            &filtered_body,
            self.session_client_provided,
        );
        let request_is_streaming =
            is_streaming_request(&effective_endpoint, &filtered_body, headers);
        let force_identity_encoding =
            needs_transform || codex_responses_to_chat || request_is_streaming;

        // 获取认证头（提前准备，用于内联替换）
        let auth_headers = if let Some(auth) = adapter.extract_auth(provider) {
            adapter.get_auth_headers(&auth)?
        } else {
            Vec::new()
        };

        // 预计算上游 host 值（用于在原位替换 host header）
        let upstream_host = url
            .parse::<http::Uri>()
            .ok()
            .and_then(|u| u.authority().map(|a| a.to_string()));

        let should_send_anthropic_headers = adapter.name() == "Claude"
            && matches!(resolved_claude_api_format.as_deref(), Some("anthropic"));

        // 预计算 anthropic-beta 值（仅 Claude）
        let anthropic_beta_value = if should_send_anthropic_headers {
            const CLAUDE_CODE_BETA: &str = "claude-code-20250219";
            Some(if let Some(beta) = headers.get("anthropic-beta") {
                if let Ok(beta_str) = beta.to_str() {
                    if beta_str.contains(CLAUDE_CODE_BETA) {
                        beta_str.to_string()
                    } else {
                        format!("{CLAUDE_CODE_BETA},{beta_str}")
                    }
                } else {
                    CLAUDE_CODE_BETA.to_string()
                }
            } else {
                CLAUDE_CODE_BETA.to_string()
            })
        } else {
            None
        };

        // anyrouter：确保 anthropic-beta 携带 context-1m flag（已有则不重复）
        let anthropic_beta_value = if should_send_anthropic_headers && is_anyrouter {
            Some(merge_anthropic_beta_token(
                anthropic_beta_value,
                ANYROUTER_CONTEXT_1M_BETA,
            ))
        } else {
            anthropic_beta_value
        };

        // ============================================================
        // 构建有序 HeaderMap — 内联替换，保持客户端原始顺序
        // ============================================================
        let mut ordered_headers = http::HeaderMap::new();
        let mut saw_auth = false;
        let mut saw_accept_encoding = false;
        let mut saw_anthropic_beta = false;
        let mut saw_anthropic_version = false;
        // RFC 7230：Connection 头点名的字段也是 hop-by-hop，须一并摘除
        let connection_tokens = connection_declared_header_tokens(headers);

        for (key, value) in headers {
            let key_str = key.as_str();

            // --- hop-by-hop（含 Connection 点名字段）— 无条件跳过 ---
            if is_hop_by_hop_request_header(key_str, &connection_tokens) {
                continue;
            }

            // --- host — 原位替换为上游 host（保持客户端原始位置） ---
            if key_str.eq_ignore_ascii_case("host") {
                if let Some(ref host_val) = upstream_host {
                    if let Ok(hv) = http::HeaderValue::from_str(host_val) {
                        ordered_headers.append(key.clone(), hv);
                    }
                }
                continue;
            }

            // --- 连接 / 追踪 / CDN 类 — 无条件跳过 ---
            if matches!(
                key_str,
                "content-length"
                    | "transfer-encoding"
                    | "x-forwarded-host"
                    | "x-forwarded-port"
                    | "x-forwarded-proto"
                    | "forwarded"
                    | "cf-connecting-ip"
                    | "cf-ipcountry"
                    | "cf-ray"
                    | "cf-visitor"
                    | "true-client-ip"
                    | "fastly-client-ip"
                    | "x-azure-clientip"
                    | "x-azure-fdid"
                    | "x-azure-ref"
                    | "akamai-origin-hop"
                    | "x-akamai-config-log-detail"
                    | "x-request-id"
                    | "x-correlation-id"
                    | "x-trace-id"
                    | "x-amzn-trace-id"
                    | "x-b3-traceid"
                    | "x-b3-spanid"
                    | "x-b3-parentspanid"
                    | "x-b3-sampled"
                    | "traceparent"
                    | "tracestate"
            ) {
                continue;
            }

            // --- 认证类 — 用 adapter 提供的认证头替换（在原始位置） ---
            if key_str.eq_ignore_ascii_case("authorization")
                || key_str.eq_ignore_ascii_case("x-api-key")
                || key_str.eq_ignore_ascii_case("x-goog-api-key")
            {
                if !saw_auth {
                    saw_auth = true;
                    for (ah_name, ah_value) in &auth_headers {
                        ordered_headers.append(ah_name.clone(), ah_value.clone());
                    }
                }
                continue;
            }

            // --- accept-encoding — transform / SSE 路径强制 identity，其余保留原值 ---
            if key_str.eq_ignore_ascii_case("accept-encoding") {
                if !saw_accept_encoding {
                    saw_accept_encoding = true;
                    if force_identity_encoding {
                        ordered_headers.append(
                            http::header::ACCEPT_ENCODING,
                            http::HeaderValue::from_static("identity"),
                        );
                    } else {
                        ordered_headers.append(key.clone(), value.clone());
                    }
                }
                continue;
            }

            // --- anthropic-beta — 用重建值替换（确保含 claude-code 标记） ---
            if key_str.eq_ignore_ascii_case("anthropic-beta") {
                if !saw_anthropic_beta {
                    saw_anthropic_beta = true;
                    if let Some(ref beta_val) = anthropic_beta_value {
                        if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                            ordered_headers.append("anthropic-beta", hv);
                        }
                    }
                }
                continue;
            }

            // --- anthropic-version — 透传客户端值 ---
            if key_str.eq_ignore_ascii_case("anthropic-version") {
                if should_send_anthropic_headers {
                    saw_anthropic_version = true;
                    ordered_headers.append(key.clone(), value.clone());
                }
                continue;
            }

            // --- anthropic-dangerous-direct-browser-access — Anthropic 专属头，
            //     非 Anthropic 格式上游一律丢弃（与 version/beta 同等对待） ---
            if key_str.eq_ignore_ascii_case("anthropic-dangerous-direct-browser-access") {
                if should_send_anthropic_headers {
                    ordered_headers.append(key.clone(), value.clone());
                }
                continue;
            }

            // --- 默认：透传 ---
            ordered_headers.append(key.clone(), value.clone());
        }

        // 如果原始请求中没有认证头，在末尾追加
        if !saw_auth && !auth_headers.is_empty() {
            for (ah_name, ah_value) in &auth_headers {
                ordered_headers.append(ah_name.clone(), ah_value.clone());
            }
        }

        // transform / SSE 路径在缺失时补 identity；普通透传不主动补 accept-encoding
        if !saw_accept_encoding && force_identity_encoding {
            ordered_headers.append(
                http::header::ACCEPT_ENCODING,
                http::HeaderValue::from_static("identity"),
            );
        }

        // 如果原始请求中没有 anthropic-beta 且有值需要添加，追加
        if !saw_anthropic_beta {
            if let Some(ref beta_val) = anthropic_beta_value {
                if let Ok(hv) = http::HeaderValue::from_str(beta_val) {
                    ordered_headers.append("anthropic-beta", hv);
                }
            }
        }

        // anthropic-version：仅在缺失时补充默认值
        if should_send_anthropic_headers && !saw_anthropic_version {
            ordered_headers.append(
                "anthropic-version",
                http::HeaderValue::from_static("2023-06-01"),
            );
        }

        // 供应商级自定义请求头规则：最后应用，可改写除认证头外的任意头
        if let Some(meta) = provider.meta.as_ref() {
            if !meta.header_rules.is_empty() {
                apply_provider_header_rules(&mut ordered_headers, &meta.header_rules);
            }
        }

        // 序列化请求体。GET/HEAD 是 idempotent/safe 方法，按 HTTP 语义不应携带 body；
        // 强行附带 JSON body 会让某些上游（如 Google Gemini 的 models.list）拒绝请求。
        let body_bytes = if matches!(method, &http::Method::GET | &http::Method::HEAD) {
            Vec::new()
        } else {
            serde_json::to_vec(&filtered_body).map_err(|e| {
                ProxyError::Internal(format!("Failed to serialize request body: {e}"))
            })?
        };

        // 确保 content-type 存在
        if !ordered_headers.contains_key(http::header::CONTENT_TYPE) {
            ordered_headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static("application/json"),
            );
        }

        // 输出请求信息日志
        let tag = adapter.name();
        let request_model = filtered_body
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("<none>");
        log::info!("[{tag}] >>> 请求 URL: {url} (model={request_model})");
        if log::log_enabled!(log::Level::Debug) {
            if let Ok(body_str) = serde_json::to_string(&filtered_body) {
                log::debug!(
                    "[{tag}] >>> 请求体内容 ({}字节): {}",
                    body_str.len(),
                    body_str
                );
            }
        }

        // 确定超时
        let timeout = if self.non_streaming_timeout.is_zero() {
            std::time::Duration::from_secs(600) // 默认 600 秒
        } else {
            self.non_streaming_timeout
        };

        // 获取全局代理 URL
        let upstream_proxy_url: Option<String> = super::http_client::get_current_proxy_url();

        // SOCKS5 代理不支持 CONNECT 隧道，需要用 reqwest
        let is_socks_proxy = upstream_proxy_url
            .as_deref()
            .map(|u| u.starts_with("socks5"))
            .unwrap_or(false);

        let preserve_exact_header_case = should_preserve_exact_header_case(
            adapter.name(),
            provider,
            resolved_claude_api_format.as_deref(),
        );

        // 发送请求
        let response = if is_socks_proxy || !preserve_exact_header_case {
            // OpenAI / Codex 类后端不依赖原始 header 大小写；走 reqwest
            // 连接池，避免 raw TCP/TLS path 每次请求都重新握手。SOCKS5 也只能走 reqwest。
            log::debug!(
                "[Forwarder] Using pooled reqwest client (preserve_exact_header_case={preserve_exact_header_case}, socks_proxy={is_socks_proxy})"
            );
            let client = super::http_client::get();
            let mut request = client.request(method.clone(), &url);
            if request_is_streaming {
                // reqwest 的 timeout 是整请求超时；流式请求交给 response_processor
                // 的首包/静默期超时控制，避免长流被总时长误杀。
                request = request.timeout(std::time::Duration::from_secs(24 * 60 * 60));
            } else if !self.non_streaming_timeout.is_zero() {
                request = request.timeout(self.non_streaming_timeout);
            }
            for (key, value) in &ordered_headers {
                request = request.header(key, value);
            }
            let send = request.body(body_bytes).send();
            let send_result = if request_is_streaming {
                let header_timeout = if self.streaming_first_byte_timeout.is_zero() {
                    timeout
                } else {
                    self.streaming_first_byte_timeout
                };
                tokio::time::timeout(header_timeout, send)
                    .await
                    .map_err(|_| {
                        ProxyError::Timeout(format!(
                            "流式响应首包超时: {}s（上游未返回响应头）",
                            header_timeout.as_secs()
                        ))
                    })?
            } else {
                send.await
            };
            let reqwest_resp = send_result.map_err(map_reqwest_send_error)?;
            ProxyResponse::Reqwest(reqwest_resp)
        } else {
            // HTTP 代理或直连：走 hyper raw write（保持 header 大小写）
            // 如果有 HTTP 代理，hyper_client 会用 CONNECT 隧道穿过代理
            let uri: http::Uri = url
                .parse()
                .map_err(|e| ProxyError::ForwardFailed(format!("Invalid URL '{url}': {e}")))?;
            super::hyper_client::send_request(
                uri,
                method.clone(),
                ordered_headers,
                extensions.clone(),
                body_bytes,
                timeout,
                upstream_proxy_url.as_deref(),
            )
            .await?
        };

        // 检查响应状态
        let status = response.status();

        if status.is_success() {
            let response = self
                .prepare_success_response_for_failover(response, request_is_streaming)
                .await?;
            Ok((response, resolved_claude_api_format, outbound_model))
        } else {
            let status_code = status.as_u16();
            let retry_after = parse_retry_after_header(response.headers());
            let body_text = String::from_utf8(response.bytes().await?.to_vec()).ok();

            Err(ProxyError::UpstreamError {
                status: status_code,
                body: body_text,
                retry_after,
            })
        }
    }

    /// 故障转移开启时，成功不能只看上游响应头。
    ///
    /// - 非流式：先把完整 body 读到内存，读超时/连接中断会回到 retry loop 尝试下一家。
    /// - 流式：至少等首个 chunk 到达，避免上游返回 200 后一直不吐 SSE 时被误记成功。
    async fn prepare_success_response_for_failover(
        &self,
        response: ProxyResponse,
        request_is_streaming: bool,
    ) -> Result<ProxyResponse, ProxyError> {
        if request_is_streaming {
            return self.prime_streaming_response(response).await;
        }

        if self.non_streaming_timeout.is_zero() {
            return Ok(response);
        }

        let status = response.status();
        let headers = response.headers().clone();
        let body_timeout = self.non_streaming_timeout;
        let body = tokio::time::timeout(body_timeout, response.bytes())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                    body_timeout.as_secs()
                ))
            })??;

        Ok(ProxyResponse::buffered(status, headers, body))
    }

    async fn prime_streaming_response(
        &self,
        response: ProxyResponse,
    ) -> Result<ProxyResponse, ProxyError> {
        if self.streaming_first_byte_timeout.is_zero() {
            return Ok(response);
        }

        let status = response.status();
        let headers = response.headers().clone();
        let timeout = self.streaming_first_byte_timeout;
        let mut stream = Box::pin(response.bytes_stream());

        let first = tokio::time::timeout(timeout, stream.next())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "流式响应首包超时: {}s（上游已返回响应头但未返回数据）",
                    timeout.as_secs()
                ))
            })?;

        let Some(first) = first else {
            return Err(ProxyError::ForwardFailed(
                "流式响应在首包到达前结束".to_string(),
            ));
        };

        let first =
            first.map_err(|e| ProxyError::ForwardFailed(format!("读取流式响应首包失败: {e}")))?;

        let replay = futures::stream::once(async move { Ok(first) }).chain(stream);
        Ok(ProxyResponse::streamed(status, headers, replay))
    }

    fn categorize_proxy_error(&self, error: &ProxyError) -> ErrorCategory {
        match error {
            // 网络和上游错误：都应该尝试下一个供应商
            ProxyError::Timeout(_) => ErrorCategory::Retryable,
            ProxyError::ForwardFailed(_) => ErrorCategory::Retryable,
            ProxyError::ProviderUnhealthy(_) => ErrorCategory::Retryable,
            // 上游 HTTP 错误：按状态码分桶。
            //
            // 客户端请求自身有问题的状态码无论换哪个 provider 都会被拒绝，
            // 继续轮询只会放大错误率、污染熔断器健康度、浪费配额：
            //   400 Bad Request / 422 Unprocessable Entity   ← 请求体格式或语义错误
            //   405 Method Not Allowed / 406 Not Acceptable  ← 方法或 Accept 错误
            //   413 Payload Too Large / 414 URI Too Long     ← 客户端构造超限
            //   415 Unsupported Media Type                    ← Content-Type 错误
            //   501 Not Implemented                           ← 上游协议确实不支持
            //
            // 其他 4xx（401/403/404/408/409/429/451 等）和全部 5xx 都保留
            // Retryable —— 换一家 provider 可能持有不同的 key、配额、地域或模型映射。
            ProxyError::UpstreamError { status, .. } => match *status {
                400 | 405 | 406 | 413 | 414 | 415 | 422 | 501 => ErrorCategory::NonRetryable,
                _ => ErrorCategory::Retryable,
            },
            // Provider 级配置/转换问题：换一个 Provider 可能就能成功
            ProxyError::ConfigError(_) => ErrorCategory::Retryable,
            ProxyError::TransformError(_) => ErrorCategory::Retryable,
            ProxyError::AuthError(_) => ErrorCategory::Retryable,
            ProxyError::StreamIdleTimeout(_) => ErrorCategory::Retryable,
            // 无可用供应商：所有供应商都试过了，无法重试
            ProxyError::NoAvailableProvider => ErrorCategory::NonRetryable,
            // 其他错误（数据库/内部错误等）：不是换供应商能解决的问题
            _ => ErrorCategory::NonRetryable,
        }
    }
}

/// 从 ProxyError 中提取错误消息
fn extract_error_message(error: &ProxyError) -> Option<String> {
    match error {
        ProxyError::UpstreamError { body, .. } => body.clone(),
        _ => Some(error.to_string()),
    }
}

/// 检测 Provider 是否为 Bedrock（通过 CLAUDE_CODE_USE_BEDROCK 环境变量判断）
fn is_bedrock_provider(provider: &Provider) -> bool {
    provider
        .settings_config
        .get("env")
        .and_then(|e| e.get("CLAUDE_CODE_USE_BEDROCK"))
        .and_then(|v| v.as_str())
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn build_retryable_failure_log(
    provider_name: &str,
    attempted_channels: usize,
    total_channels: usize,
    error: &ProxyError,
) -> (&'static str, String) {
    let error_summary = summarize_proxy_error(error);

    if total_channels <= 1 {
        (
            log_fwd::SINGLE_PROVIDER_FAILED,
            format!("Provider {provider_name} 请求失败: {error_summary}"),
        )
    } else {
        (
            log_fwd::PROVIDER_FAILED_RETRY,
            format!(
                "Provider {provider_name} 失败，继续尝试下一个通道 ({attempted_channels}/{total_channels}): {error_summary}"
            ),
        )
    }
}

fn build_terminal_failure_log(
    attempted_channels: usize,
    total_channels: usize,
    last_error: Option<&ProxyError>,
) -> Option<(&'static str, String)> {
    if total_channels <= 1 {
        return None;
    }

    let error_summary = last_error
        .map(summarize_proxy_error)
        .unwrap_or_else(|| "未知错误".to_string());

    Some((
        log_fwd::ALL_PROVIDERS_FAILED,
        format!(
            "已尝试 {attempted_channels}/{total_channels} 条通道，均失败。最后错误: {error_summary}"
        ),
    ))
}

fn summarize_proxy_error(error: &ProxyError) -> String {
    match error {
        ProxyError::UpstreamError { status, body, .. } => {
            let body_summary = body
                .as_deref()
                .map(summarize_upstream_body)
                .filter(|summary| !summary.is_empty());

            match body_summary {
                Some(summary) => format!("上游 HTTP {status}: {summary}"),
                None => format!("上游 HTTP {status}"),
            }
        }
        ProxyError::Timeout(message) => {
            format!("请求超时: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ForwardFailed(message) => {
            format!("请求转发失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::TransformError(message) => {
            format!("响应转换失败: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::ConfigError(message) => {
            format!("配置错误: {}", summarize_text_for_log(message, 180))
        }
        ProxyError::AuthError(message) => {
            format!("认证失败: {}", summarize_text_for_log(message, 180))
        }
        _ => summarize_text_for_log(&error.to_string(), 180),
    }
}

fn summarize_upstream_body(body: &str) -> String {
    if let Ok(json_body) = serde_json::from_str::<Value>(body) {
        if let Some(message) = extract_json_error_message(&json_body) {
            return summarize_text_for_log(&message, 180);
        }

        if let Ok(compact_json) = serde_json::to_string(&json_body) {
            return summarize_text_for_log(&compact_json, 180);
        }
    }

    summarize_text_for_log(body, 180)
}

fn extract_json_error_message(body: &Value) -> Option<String> {
    let candidates = [
        body.pointer("/error/message"),
        body.pointer("/message"),
        body.pointer("/detail"),
        body.pointer("/error"),
    ];

    candidates
        .into_iter()
        .flatten()
        .find_map(|value| value.as_str().map(ToString::to_string))
}

fn split_endpoint_and_query(endpoint: &str) -> (&str, Option<&str>) {
    endpoint
        .split_once('?')
        .map_or((endpoint, None), |(path, query)| (path, Some(query)))
}

fn strip_beta_query(query: Option<&str>) -> Option<String> {
    let filtered = query.map(|query| {
        query
            .split('&')
            .filter(|pair| !pair.is_empty() && !pair.starts_with("beta="))
            .collect::<Vec<_>>()
            .join("&")
    });

    match filtered.as_deref() {
        Some("") | None => None,
        Some(_) => filtered,
    }
}

fn is_claude_messages_path(path: &str) -> bool {
    matches!(path, "/v1/messages" | "/claude/v1/messages")
}

fn rewrite_codex_responses_endpoint_to_chat(endpoint: &str) -> (String, Option<String>) {
    let (_path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = query.map(ToString::to_string);
    let target_path = "/chat/completions";
    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn rewrite_claude_transform_endpoint(
    endpoint: &str,
    api_format: &str,
    body: &Value,
) -> (String, Option<String>) {
    let (path, query) = split_endpoint_and_query(endpoint);
    let passthrough_query = if is_claude_messages_path(path) {
        strip_beta_query(query)
    } else {
        query.map(ToString::to_string)
    };

    if !is_claude_messages_path(path) {
        return (endpoint.to_string(), passthrough_query);
    }

    if api_format == "gemini_native" {
        let model =
            super::providers::transform_gemini::extract_gemini_model(body).unwrap_or("unknown");
        // Accept both bare ids (`gemini-2.5-pro`) and the resource-name
        // form (`models/gemini-2.5-pro`) that Gemini SDKs emit. See
        // `normalize_gemini_model_id` for rationale.
        let model = super::gemini_url::normalize_gemini_model_id(model);
        let is_stream = body
            .get("stream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let target_path = if is_stream {
            format!("/v1beta/models/{model}:streamGenerateContent")
        } else {
            format!("/v1beta/models/{model}:generateContent")
        };

        let rewritten_query = merge_query_params(
            passthrough_query.as_deref(),
            if is_stream { Some("alt=sse") } else { None },
        );

        let rewritten = match rewritten_query.as_deref() {
            Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
            _ => target_path,
        };

        return (rewritten, rewritten_query);
    }

    let target_path = if api_format == "openai_responses" {
        "/v1/responses"
    } else {
        "/v1/chat/completions"
    };

    let rewritten = match passthrough_query.as_deref() {
        Some(query) if !query.is_empty() => format!("{target_path}?{query}"),
        _ => target_path.to_string(),
    };

    (rewritten, passthrough_query)
}

fn merge_query_params(base_query: Option<&str>, extra_param: Option<&str>) -> Option<String> {
    let mut params: Vec<String> = base_query
        .into_iter()
        .flat_map(|query| query.split('&'))
        .filter(|pair| !pair.is_empty())
        .filter(|pair| !pair.starts_with("alt="))
        .map(ToString::to_string)
        .collect();

    if let Some(extra_param) = extra_param {
        params.push(extra_param.to_string());
    }

    if params.is_empty() {
        None
    } else {
        Some(params.join("&"))
    }
}

fn append_query_to_full_url(base_url: &str, query: Option<&str>) -> String {
    match query {
        Some(query) if !query.is_empty() => {
            if base_url.contains('?') {
                format!("{base_url}&{query}")
            } else {
                format!("{base_url}?{query}")
            }
        }
        _ => base_url.to_string(),
    }
}

fn should_preserve_exact_header_case(
    adapter_name: &str,
    _provider: &Provider,
    resolved_claude_api_format: Option<&str>,
) -> bool {
    if matches!(adapter_name, "Codex" | "Gemini") {
        return false;
    }

    matches!(resolved_claude_api_format, None | Some("anthropic"))
}

fn is_streaming_request(endpoint: &str, body: &Value, headers: &axum::http::HeaderMap) -> bool {
    if body
        .get("stream")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        return true;
    }

    if endpoint.contains("streamGenerateContent") || endpoint.contains("alt=sse") {
        return true;
    }

    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("text/event-stream"))
        .unwrap_or(false)
}

/// RFC 7230 hop-by-hop 请求头：代理必须摘除、不得转发到上游
/// （content-length / transfer-encoding 在主跳过清单单独处理；响应侧见 response_processor）。
const HOP_BY_HOP_REQUEST_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "trailers",
    "upgrade",
];

/// 收集客户端 `Connection` 头点名的动态 hop-by-hop 字段（RFC 7230 §6.1）。
fn connection_declared_header_tokens(headers: &axum::http::HeaderMap) -> Vec<String> {
    headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_hop_by_hop_request_header(name: &str, connection_tokens: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    HOP_BY_HOP_REQUEST_HEADERS.contains(&lower.as_str()) || connection_tokens.contains(&lower)
}

/// 自定义头规则禁止触碰的认证头（由 adapter 统一注入，规则改写会破坏鉴权）。
const HEADER_RULE_AUTH_BLACKLIST: &[&str] = &["authorization", "x-api-key", "x-goog-api-key"];

/// 按配置顺序应用供应商级自定义请求头规则（ccLoad 风格）。
///
/// - override：覆盖整头；append：追加一条值
/// - remove：value 为空删除整头；非空时按 CSV token 精确摘除
///   （典型用例：从 anthropic-beta 里只摘掉某一个 flag、保留其余）
/// - 认证头黑名单内的规则静默忽略并告警
fn apply_provider_header_rules(
    headers: &mut http::HeaderMap,
    rules: &[crate::provider::CustomHeaderRule],
) {
    for (idx, rule) in rules.iter().enumerate() {
        let name = rule.name.trim();
        if name.is_empty() {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        if HEADER_RULE_AUTH_BLACKLIST.contains(&lower.as_str()) {
            log::warn!("[HeaderRules] 规则 #{idx} 试图改写认证头 {name}，已忽略");
            continue;
        }
        let Ok(header_name) = http::HeaderName::from_bytes(lower.as_bytes()) else {
            log::warn!("[HeaderRules] 规则 #{idx} 头名称非法: {name:?}，已忽略");
            continue;
        };
        match rule.action.as_str() {
            "override" => {
                if let Ok(value) = http::HeaderValue::from_str(rule.value.trim()) {
                    headers.insert(header_name, value);
                } else {
                    log::warn!("[HeaderRules] 规则 #{idx} 值非法，已忽略");
                }
            }
            "append" => {
                if let Ok(value) = http::HeaderValue::from_str(rule.value.trim()) {
                    headers.append(header_name, value);
                } else {
                    log::warn!("[HeaderRules] 规则 #{idx} 值非法，已忽略");
                }
            }
            "remove" => {
                let target = rule.value.trim();
                if target.is_empty() {
                    headers.remove(&header_name);
                } else {
                    remove_header_csv_token(headers, &header_name, target);
                }
            }
            other => {
                log::warn!("[HeaderRules] 规则 #{idx} 动作未知: {other:?}，已忽略");
            }
        }
    }
}

/// 从（可能多条的）头值里按 CSV token 精确摘除 `target`；
/// 某条值的 token 摘空则丢弃该条，全部摘空则整头删除。
fn remove_header_csv_token(headers: &mut http::HeaderMap, name: &http::HeaderName, target: &str) {
    let values: Vec<String> = headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect();
    if values.is_empty() {
        return;
    }
    let mut kept_values = Vec::with_capacity(values.len());
    for value in values {
        let kept: Vec<&str> = value
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty() && *token != target)
            .collect();
        if !kept.is_empty() {
            kept_values.push(kept.join(", "));
        }
    }
    headers.remove(name);
    for value in kept_values {
        if let Ok(hv) = http::HeaderValue::from_str(&value) {
            headers.append(name, hv);
        }
    }
}

/// anyrouter 的两个已知接入域名：主站 + 阿里云函数计算备用端点。
/// 渠道名含 "anyrouter" 或 base_url 命中任一域名（含子域）即按 anyrouter 适配。
const ANYROUTER_KNOWN_HOSTS: &[&str] = &["anyrouter.top", "a-ocnfniawgw.cn-shanghai.fcapp.run"];

/// anyrouter 要求显式携带的 1M 上下文 beta flag（ccLoad 同款适配）。
const ANYROUTER_CONTEXT_1M_BETA: &str = "context-1m-2025-08-07";

/// anyrouter 429 限流的同通道重试预算（每个客户端请求共享，跨 key 计数）。
///
/// anyrouter 的 429 来自分钟级滚动窗口 + 后端账号轮转，稍候重试大概率
/// 落到可用后端，因此预算刻意不受 AppProxyConfig.max_retries 约束、给足量；
/// 同时 429 不计入 key/通道冷却与熔断健康度（见 forward_with_retry_inner）。
const ANYROUTER_RATE_LIMIT_MAX_RETRIES: usize = 50;
/// anyrouter 429 重试间隔：线性爬升（250ms、500ms、…）封顶 2.5s；
/// 上游显式 Retry-After 优先，单次封顶 30s。
const ANYROUTER_RATE_LIMIT_RETRY_BASE_MS: u64 = 250;
const ANYROUTER_RATE_LIMIT_RETRY_MAX_MS: u64 = 2_500;
const ANYROUTER_RATE_LIMIT_RETRY_AFTER_CAP_SECS: u64 = 30;
/// anyrouter 429 重试的总等待预算（纯 sleep 时间，不含请求本身耗时），
/// 防止上游持续回大 Retry-After 时把单个请求挂死。
const ANYROUTER_RATE_LIMIT_TOTAL_WAIT_MS: u64 = 120_000;

/// 上游 429 限流错误（anyrouter 特判用）。
fn is_rate_limited_upstream_error(error: &ProxyError) -> bool {
    matches!(error, ProxyError::UpstreamError { status: 429, .. })
}

fn is_anyrouter_channel(provider_name: &str, base_url: &str) -> bool {
    if provider_name.to_ascii_lowercase().contains("anyrouter") {
        return true;
    }
    let Some(host) = base_url
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(|h| h.to_ascii_lowercase()))
    else {
        return false;
    };
    ANYROUTER_KNOWN_HOSTS
        .iter()
        .any(|known| host == *known || host.ends_with(&format!(".{known}")))
}

/// 把 `token` 并入 CSV 形式的 anthropic-beta 值（已存在则原样返回）。
fn merge_anthropic_beta_token(existing: Option<String>, token: &str) -> String {
    match existing {
        Some(value) if value.split(',').any(|t| t.trim() == token) => value,
        Some(value) if !value.trim().is_empty() => format!("{value},{token}"),
        _ => token.to_string(),
    }
}

/// anyrouter /v1/messages：body 未声明 thinking 时注入 adaptive
/// （anyrouter 上游要求显式 thinking.type=adaptive 才启用自适应思考）。
fn inject_adaptive_thinking_if_missing(body: &mut Value) -> bool {
    let Some(obj) = body.as_object_mut() else {
        return false;
    };
    if obj.contains_key("thinking") {
        return false;
    }
    obj.insert(
        "thinking".to_string(),
        serde_json::json!({"type": "adaptive"}),
    );
    true
}

/// anyrouter Codex 端点的 400 invalid_responses_request：
/// 该上游不接受 OpenAI 的 encrypted reasoning content 与 tool_search_* 输入项。
fn is_invalid_responses_request_error(error: &ProxyError) -> bool {
    let ProxyError::UpstreamError {
        status: 400,
        body: Some(body),
        ..
    } = error
    else {
        return false;
    };
    body.to_ascii_lowercase().contains("invalid_responses_request")
}

/// 剥掉 Codex 请求体里所有 encrypted_content 字段（递归）与顶层 input[]
/// 中 type 前缀为 tool_search_ 的项；没有任何改动时返回 None（不值得重试）。
fn codex_body_without_encrypted_and_tool_search(body: &Value) -> Option<Value> {
    let mut cloned = body.clone();
    let mut removed = remove_encrypted_content_fields(&mut cloned);
    if let Some(input) = cloned.get_mut("input").and_then(Value::as_array_mut) {
        let before = input.len();
        input.retain(|item| {
            !item
                .get("type")
                .and_then(Value::as_str)
                .map(|t| t.starts_with("tool_search_"))
                .unwrap_or(false)
        });
        if input.len() != before {
            removed = true;
        }
    }
    removed.then_some(cloned)
}

fn remove_encrypted_content_fields(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            let mut removed = map.remove("encrypted_content").is_some();
            for child in map.values_mut() {
                removed |= remove_encrypted_content_fields(child);
            }
            removed
        }
        Value::Array(items) => {
            let mut removed = false;
            for item in items {
                removed |= remove_encrypted_content_fields(item);
            }
            removed
        }
        _ => false,
    }
}

fn is_key_scoped_error(error: &ProxyError) -> bool {
    match error {
        ProxyError::AuthError(_) => true,
        ProxyError::UpstreamError { status, body, .. } => {
            // key 维度状态码：401/403 认证、402 计费、429 限流 —— 换 key 通常有效
            if matches!(*status, 401 | 402 | 403 | 429) {
                return true;
            }
            // 其他状态码（含 NonRetryable 的 400/422 等）仅在 body 明确指向
            // API key 本身时才算 key 维度问题。不能用 "quota"/"balance"/
            // "credit"/"unauthorized" 等宽泛词：400 "max_tokens exceeds your
            // credit plan limit" 这类请求错误会被误判成 key 失效，
            // 换 key 重试 + 冷却逐个打满整个 key 池。
            body.as_deref()
                .map(|body| {
                    let lower = body.to_ascii_lowercase();
                    lower.contains("invalid_api_key")
                        || lower.contains("invalid api key")
                        || lower.contains("incorrect api key")
                        || lower.contains("api key not valid")
                        || lower.contains("api key expired")
                        || lower.contains("api_key_invalid")
                        || lower.contains("account_deactivated")
                })
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// key 通道失败的冷却策略：(初始冷却秒数, 退避上限秒数, 宽限次数)。
///
/// 实际冷却 = min(初始 × 2^(连续失败数 - 宽限), 上限)，由 DAO 计算（ccLoad 风格）；
/// 连续失败数未达到宽限次数前不冷却，只标 Degraded（留在轮转中、组内降序靠后）。
/// 任何可重试错误都只作用于当前 key 通道，不连坐同 provider 的其他 key。
///
/// 优先级：
/// 1. 429/5xx 且上游给了 Retry-After → 恰好冷却该秒数（上游最清楚自己何时恢复）
/// 2. 配额/余额耗尽（含 429 配额型）→ 配额轨道，等待重置周期
/// 3. 瞬时限流 429 → 前 3 次连续失败不冷却（限流多为分钟级窗口，应多重试；
///    请求内本来就会切到下一个 key），之后才进入 30s 起步的短冷却
/// 4. 认证 / 5xx / 其他 → 维持原有分轨
fn key_failure_cooldown(error: &ProxyError) -> (i64, i64, i64) {
    const MINUTE: i64 = 60;
    /// 瞬时 429 的冷却宽限：连续失败满 3 次才开始冷却
    const RATE_LIMIT_GRACE_FAILURES: i64 = 3;

    match error {
        // 认证失效：起步 10 分钟，指数爬升到 24 小时（坏 key 快速退出轮转，但可自愈）
        ProxyError::AuthError(_) => (10 * MINUTE, 24 * 60 * MINUTE, 0),
        ProxyError::UpstreamError { status, .. } if matches!(*status, 401 | 403) => {
            (10 * MINUTE, 24 * 60 * MINUTE, 0)
        }
        ProxyError::UpstreamError {
            status,
            body,
            retry_after,
        } => {
            let quota_exhausted = body
                .as_deref()
                .map(|body| {
                    let lower = body.to_ascii_lowercase();
                    lower.contains("quota")
                        || lower.contains("insufficient")
                        || lower.contains("balance")
                        || lower.contains("credit")
                })
                .unwrap_or(false);

            // 上游显式 Retry-After（仅限 429/5xx 这类"稍后重试"语义的状态码）：
            // 冷却恰好该时长，不做指数放大；上限 1 小时防御异常值
            if *status == 429 || *status >= 500 {
                if let Some(ra) = retry_after {
                    let secs = (*ra as i64).clamp(1, 60 * MINUTE);
                    return (secs, secs, 0);
                }
            }

            if quota_exhausted {
                // 配额/余额：等待重置周期（429 配额型也归入此轨，不再被短轨道截胡）
                (5 * MINUTE, 60 * MINUTE, 0)
            } else if *status == 429 {
                // 瞬时限流：多重试少冷却 —— 宽限期内只降权不下场，
                // 连续超限后用 30s→10min 的短退避保护上游
                (30, 10 * MINUTE, RATE_LIMIT_GRACE_FAILURES)
            } else if *status >= 500 {
                // 上游 5xx：该 key 对应的上游通道故障
                (2 * MINUTE, 30 * MINUTE, 0)
            } else {
                (MINUTE, 30 * MINUTE, 0)
            }
        }
        // 超时/网络：最短冷却
        _ => (MINUTE, 30 * MINUTE, 0),
    }
}

/// 解析上游响应的 Retry-After 头（RFC 7231：delta-seconds 或 HTTP-date），
/// 统一折算成秒。无头、非法值、过去时间均返回 None。
fn parse_retry_after_header(headers: &axum::http::HeaderMap) -> Option<u64> {
    let raw = headers
        .get(axum::http::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if raw.is_empty() {
        return None;
    }

    // delta-seconds（部分上游会给小数，向上取整）
    if let Ok(secs) = raw.parse::<f64>() {
        if !secs.is_finite() || secs <= 0.0 {
            return None;
        }
        return Some(secs.ceil() as u64);
    }

    // HTTP-date（IMF-fixdate，chrono 的 RFC 2822 解析器接受 GMT 时区名）
    let target = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let delta = target.timestamp() - chrono::Utc::now().timestamp();
    u64::try_from(delta).ok().filter(|secs| *secs > 0)
}

#[cfg(test)]
fn should_force_identity_encoding(
    endpoint: &str,
    body: &Value,
    headers: &axum::http::HeaderMap,
) -> bool {
    is_streaming_request(endpoint, body, headers)
}

fn map_reqwest_send_error(error: reqwest::Error) -> ProxyError {
    if error.is_timeout() {
        ProxyError::Timeout(format!("请求超时: {error}"))
    } else if error.is_connect() {
        ProxyError::ForwardFailed(format!("连接失败: {error}"))
    } else {
        ProxyError::ForwardFailed(error.to_string())
    }
}

fn summarize_text_for_log(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = normalized.trim();

    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let truncated: String = trimmed.chars().take(max_chars).collect();
    let truncated = truncated.trim_end();
    format!("{truncated}...")
}

fn prepare_upstream_request_body(request_body: Value) -> Value {
    canonicalize_value(filter_private_params_with_whitelist(request_body, &[]))
}

fn log_prompt_cache_trace(
    app_type: &AppType,
    provider: &Provider,
    endpoint: &str,
    api_format: Option<&str>,
    body: &Value,
    session_client_provided: bool,
) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }

    let prompt_cache_key = body
        .get("prompt_cache_key")
        .and_then(|value| value.as_str())
        .map(|key| format!("present(len={})", key.len()))
        .unwrap_or_else(|| "absent".to_string());
    let store = body
        .get("store")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());
    let stream = body
        .get("stream")
        .map(value_for_log)
        .unwrap_or_else(|| "absent".to_string());

    log::debug!(
        "[CacheTrace] app={}, provider={}, endpoint={}, api_format={}, session_client_provided={}, prompt_cache_key={}, store={}, stream={}, instructions_hash={}, tools_hash={}, input_hash={}, include_hash={}, body_hash={}",
        app_type.as_str(),
        provider.id,
        endpoint,
        api_format.unwrap_or("native"),
        session_client_provided,
        prompt_cache_key,
        store,
        stream,
        short_value_hash(body.get("instructions")),
        short_value_hash(body.get("tools")),
        short_value_hash(body.get("input")),
        short_value_hash(body.get("include")),
        short_value_hash(Some(body)),
    );
}

fn value_for_log(value: &Value) -> String {
    match value {
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Array(values) => format!("array(len={})", values.len()),
        Value::Object(values) => format!("object(len={})", values.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use axum::extract::State;
    use axum::http::header::{HeaderValue, ACCEPT};
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::Router;
    use bytes::Bytes;
    use http::StatusCode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    fn upstream_error(status: u16, body: Option<&str>, retry_after: Option<u64>) -> ProxyError {
        ProxyError::UpstreamError {
            status,
            body: body.map(ToString::to_string),
            retry_after,
        }
    }

    #[test]
    fn parse_retry_after_accepts_delta_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("120"),
        );
        assert_eq!(parse_retry_after_header(&headers), Some(120));
    }

    #[test]
    fn parse_retry_after_ceils_fractional_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_static("0.5"),
        );
        assert_eq!(parse_retry_after_header(&headers), Some(1));
    }

    #[test]
    fn parse_retry_after_rejects_zero_negative_and_garbage() {
        for raw in ["0", "-5", "soon", ""] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from_str(raw).unwrap(),
            );
            assert_eq!(parse_retry_after_header(&headers), None, "raw={raw:?}");
        }
        assert_eq!(parse_retry_after_header(&HeaderMap::new()), None);
    }

    #[test]
    fn parse_retry_after_accepts_future_http_date_and_rejects_past() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(90);
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_str(&future.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
                .unwrap(),
        );
        let secs = parse_retry_after_header(&headers).expect("future date parses");
        assert!((85..=90).contains(&secs), "≈90s, got {secs}");

        let past = chrono::Utc::now() - chrono::Duration::seconds(90);
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::RETRY_AFTER,
            HeaderValue::from_str(&past.format("%a, %d %b %Y %H:%M:%S GMT").to_string()).unwrap(),
        );
        assert_eq!(parse_retry_after_header(&headers), None);
    }

    #[test]
    fn cooldown_respects_upstream_retry_after_exactly() {
        // 429/5xx 带 Retry-After：恰好冷却该秒数（base=cap、无宽限）
        assert_eq!(
            key_failure_cooldown(&upstream_error(429, None, Some(300))),
            (300, 300, 0)
        );
        assert_eq!(
            key_failure_cooldown(&upstream_error(503, None, Some(45))),
            (45, 45, 0)
        );
        // 异常大的值被钳到 1 小时
        assert_eq!(
            key_failure_cooldown(&upstream_error(429, None, Some(86400))),
            (3600, 3600, 0)
        );
        // 401/403 不吃 Retry-After，保持认证轨道
        assert_eq!(
            key_failure_cooldown(&upstream_error(401, None, Some(5))),
            (600, 86400, 0)
        );
    }

    #[test]
    fn cooldown_routes_quota_429_to_quota_track() {
        // 429 配额型：不再被瞬时限流的短轨道截胡
        assert_eq!(
            key_failure_cooldown(&upstream_error(
                429,
                Some(r#"{"error":{"message":"You exceeded your monthly quota"}}"#),
                None
            )),
            (300, 3600, 0)
        );
        // Retry-After 优先于配额关键词（上游显式时间最准）
        assert_eq!(
            key_failure_cooldown(&upstream_error(429, Some("quota exceeded"), Some(120))),
            (120, 120, 0)
        );
    }

    #[test]
    fn cooldown_gives_transient_429_grace_before_short_backoff() {
        // 瞬时限流：3 次宽限 + 30s→10min 短退避（多重试少冷却）
        assert_eq!(
            key_failure_cooldown(&upstream_error(429, Some("rate limit exceeded"), None)),
            (30, 600, 3)
        );
        assert_eq!(
            key_failure_cooldown(&upstream_error(429, None, None)),
            (30, 600, 3)
        );
    }

    #[test]
    fn cooldown_keeps_legacy_tracks_for_other_errors() {
        assert_eq!(
            key_failure_cooldown(&ProxyError::AuthError("bad".into())),
            (600, 86400, 0)
        );
        assert_eq!(
            key_failure_cooldown(&upstream_error(500, None, None)),
            (120, 1800, 0)
        );
        assert_eq!(
            key_failure_cooldown(&upstream_error(
                402,
                Some("insufficient balance"),
                None
            )),
            (300, 3600, 0)
        );
        assert_eq!(
            key_failure_cooldown(&ProxyError::Timeout("t".into())),
            (60, 1800, 0)
        );
    }

    #[test]
    fn key_scoped_error_matches_key_dimension_statuses() {
        assert!(is_key_scoped_error(&ProxyError::AuthError("bad".into())));
        for status in [401, 402, 403, 429] {
            assert!(
                is_key_scoped_error(&upstream_error(status, None, None)),
                "status {status} should be key-scoped"
            );
        }
    }

    #[test]
    fn key_scoped_error_rejects_generic_client_errors() {
        // 400 "credit plan limit" 是请求维度问题（max_tokens 超限），不是 key 失效；
        // 旧版宽泛词 "credit"/"quota"/"balance" 会把这类错误误判成换 key 可解。
        assert!(!is_key_scoped_error(&upstream_error(
            400,
            Some("max_tokens exceeds your credit plan limit"),
            None
        )));
        assert!(!is_key_scoped_error(&upstream_error(
            422,
            Some("quota of tools exceeded"),
            None
        )));
        assert!(!is_key_scoped_error(&upstream_error(
            500,
            Some("internal balance service error"),
            None
        )));
        assert!(!is_key_scoped_error(&upstream_error(404, None, None)));
        assert!(!is_key_scoped_error(&ProxyError::Timeout("t".into())));
    }

    #[test]
    fn key_scoped_error_accepts_explicit_key_phrases_on_other_statuses() {
        assert!(is_key_scoped_error(&upstream_error(
            400,
            Some(r#"{"error":{"code":"invalid_api_key","message":"bad key"}}"#),
            None
        )));
        assert!(is_key_scoped_error(&upstream_error(
            400,
            Some("API key not valid. Please pass a valid API key."),
            None
        )));
    }

    fn test_provider() -> Provider {
        Provider {
            id: "provider-1".to_string(),
            name: "Provider 1".to_string(),
            settings_config: json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    fn test_forwarder(
        non_streaming_timeout: Duration,
        streaming_first_byte_timeout: Duration,
    ) -> RequestForwarder {
        let db = Arc::new(Database::memory().expect("memory db"));

        test_forwarder_with_db(db, non_streaming_timeout, streaming_first_byte_timeout)
    }

    fn test_forwarder_with_db(
        db: Arc<Database>,
        non_streaming_timeout: Duration,
        streaming_first_byte_timeout: Duration,
    ) -> RequestForwarder {
        RequestForwarder {
            router: Arc::new(ProviderRouter::new(db.clone())),
            status: Arc::new(RwLock::new(ProxyStatus::default())),
            current_providers: Arc::new(RwLock::new(HashMap::new())),
            gemini_shadow: Arc::new(GeminiShadowStore::new()),
            codex_chat_history: Arc::new(CodexChatHistoryStore::default()),
            failover_manager: Arc::new(FailoverSwitchManager::new(db)),
            app_handle: None,
            current_provider_id_at_start: String::new(),
            session_id: String::new(),
            session_client_provided: false,
            rectifier_config: RectifierConfig::default(),
            optimizer_config: OptimizerConfig::default(),
            model_routing: crate::proxy::model_routing::ModelRoutingConfig::default(),
            non_streaming_timeout,
            streaming_first_byte_timeout,
            max_attempts: 1,
            anyrouter_429_max_retries: ANYROUTER_RATE_LIMIT_MAX_RETRIES,
            anyrouter_429_total_wait_ms: ANYROUTER_RATE_LIMIT_TOTAL_WAIT_MS,
        }
    }

    async fn key_failover_mock_handler(
        State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
        headers: HeaderMap,
        _body: Bytes,
    ) -> (StatusCode, &'static str) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        seen_auth.lock().await.push(auth.clone());

        if auth == "Bearer sk-key-1" {
            (
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"rate limit"}}"#,
            )
        } else {
            (StatusCode::OK, r#"{"ok":true}"#)
        }
    }

    /// 前两次请求回 429，之后回 200（anyrouter 限流重试用）。
    async fn anyrouter_rate_limit_mock_handler(
        State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
        headers: HeaderMap,
        _body: Bytes,
    ) -> (StatusCode, &'static str) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut seen = seen_auth.lock().await;
        seen.push(auth);

        if seen.len() <= 2 {
            (
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"rate limit"}}"#,
            )
        } else {
            (StatusCode::OK, r#"{"ok":true}"#)
        }
    }

    /// 永远回 429（anyrouter 不冷却断言用）。
    async fn always_rate_limited_mock_handler(
        State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
        headers: HeaderMap,
        _body: Bytes,
    ) -> (StatusCode, &'static str) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        seen_auth.lock().await.push(auth);
        (
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"rate limit"}}"#,
        )
    }

    async fn provider_routing_mock_handler(
        State(seen_auth): State<Arc<Mutex<Vec<String>>>>,
        headers: HeaderMap,
        _body: Bytes,
    ) -> (StatusCode, &'static str) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        seen_auth.lock().await.push(auth.clone());

        match auth.as_str() {
            "Bearer sk-provider-error-1" | "Bearer sk-provider-error-2" => (
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":{"message":"provider unavailable"}}"#,
            ),
            "Bearer sk-key-rate-limited-1" | "Bearer sk-key-rate-limited-2" => (
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"rate limit"}}"#,
            ),
            "Bearer sk-provider-2" => (StatusCode::OK, r#"{"ok":true}"#),
            _ => (
                StatusCode::UNAUTHORIZED,
                r#"{"error":{"message":"unknown key"}}"#,
            ),
        }
    }

    fn install_test_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    /// 等待后台成功记账任务落库（record_success_result 异步化后，亲和绑定 /
    /// key 成功记录在 spawn 出的任务里完成，测试断言前需轮询等待）。
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("后台记账未在 1s 内落库");
    }

    #[test]
    fn single_provider_retryable_log_uses_single_provider_code() {
        let error = ProxyError::UpstreamError {
            status: 429,
            body: Some(r#"{"error":{"message":"rate limit exceeded"}}"#.to_string()),
            retry_after: None,
        };

        let (code, message) = build_retryable_failure_log("PackyCode-response", 1, 1, &error);

        assert_eq!(code, log_fwd::SINGLE_PROVIDER_FAILED);
        assert!(message.contains("Provider PackyCode-response 请求失败"));
        assert!(message.contains("上游 HTTP 429"));
        assert!(message.contains("rate limit exceeded"));
        assert!(!message.contains("切换下一个"));
    }

    #[test]
    fn multi_provider_retryable_log_keeps_failover_wording() {
        let error = ProxyError::Timeout("upstream timed out after 30s".to_string());

        let (code, message) = build_retryable_failure_log("primary", 1, 3, &error);

        assert_eq!(code, log_fwd::PROVIDER_FAILED_RETRY);
        assert!(message.contains("继续尝试下一个通道 (1/3)"));
        assert!(message.contains("请求超时"));
    }

    #[test]
    fn single_provider_has_no_terminal_all_failed_log() {
        assert!(build_terminal_failure_log(1, 1, None).is_none());
    }

    #[test]
    fn multi_provider_terminal_log_contains_last_error_summary() {
        let error = ProxyError::ForwardFailed("connection reset by peer".to_string());

        let (code, message) =
            build_terminal_failure_log(2, 2, Some(&error)).expect("expected terminal log");

        assert_eq!(code, log_fwd::ALL_PROVIDERS_FAILED);
        assert!(message.contains("已尝试 2/2 条通道，均失败"));
        assert!(message.contains("connection reset by peer"));
    }

    #[test]
    fn summarize_upstream_body_prefers_json_message() {
        let body = json!({
            "error": {
                "message": "invalid_request_error: unsupported field"
            },
            "request_id": "req_123"
        });

        let summary = summarize_upstream_body(&body.to_string());

        assert_eq!(summary, "invalid_request_error: unsupported field");
    }

    #[test]
    fn summarize_text_for_log_collapses_whitespace_and_truncates() {
        let summary = summarize_text_for_log("line1\n\n line2   line3", 12);

        assert_eq!(summary, "line1 line2...");
    }

    #[test]
    fn canonical_json_sorts_object_keys_for_cache_trace_hashes() {
        let left = json!({
            "tools": [
                {
                    "parameters": {
                        "properties": {
                            "b": {"type": "string"},
                            "a": {"type": "number"}
                        },
                        "type": "object"
                    },
                    "name": "lookup"
                }
            ]
        });
        let right = json!({
            "tools": [
                {
                    "name": "lookup",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "a": {"type": "number"},
                            "b": {"type": "string"}
                        }
                    }
                }
            ]
        });

        assert_eq!(
            crate::proxy::json_canonical::canonical_json_string(&left),
            crate::proxy::json_canonical::canonical_json_string(&right)
        );
        assert_eq!(
            short_value_hash(Some(&left)),
            short_value_hash(Some(&right))
        );
    }

    #[test]
    fn prepare_upstream_request_body_filters_private_fields_and_canonicalizes_order() {
        let body = json!({
            "z": 1,
            "_internal": "drop",
            "tools": [
                {
                    "name": "lookup",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "_id": {
                                "_private_note": "drop",
                                "type": "string"
                            },
                            "b": {"type": "number"},
                            "a": {"type": "string"}
                        }
                    }
                }
            ],
            "a": 2
        });

        let prepared = prepare_upstream_request_body(body);

        assert!(prepared.get("_internal").is_none());
        assert!(prepared["tools"][0]["parameters"]["properties"]
            .get("_id")
            .is_some());
        assert!(prepared["tools"][0]["parameters"]["properties"]["_id"]
            .get("_private_note")
            .is_none());
        assert_eq!(
            serde_json::to_string(&prepared).unwrap(),
            r#"{"a":2,"tools":[{"name":"lookup","parameters":{"properties":{"_id":{"type":"string"},"a":{"type":"string"},"b":{"type":"number"}},"type":"object"}}],"z":1}"#
        );
    }

    #[tokio::test]
    async fn non_streaming_success_is_buffered_before_marking_provider_successful() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"{\"ok\":true}"))
            }),
        );

        let prepared = forwarder
            .prepare_success_response_for_failover(response, false)
            .await
            .expect("response should be buffered");

        assert_eq!(
            prepared.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
    }

    #[tokio::test]
    async fn key_scoped_upstream_failure_fails_over_to_next_key() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-key-1"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-1").unwrap();

        let key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("second key should succeed: {}", err.error),
        };

        assert_eq!(result.key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
        assert_eq!(
            *seen_auth.lock().await,
            vec!["Bearer sk-key-1".to_string(), "Bearer sk-key-2".to_string()]
        );

        let key_1_after = db
            .get_provider_key("claude", "provider-1", &key_1.id)
            .unwrap()
            .unwrap();
        // 成功记账（key 成功 + 亲和绑定）已异步化，轮询等待落库
        wait_until(|| {
            db.get_provider_key("claude", "provider-1", &key_2.id)
                .unwrap()
                .unwrap()
                .last_success_at
                .is_some()
        })
        .await;
        let key_2_after = db
            .get_provider_key("claude", "provider-1", &key_2.id)
            .unwrap()
            .unwrap();

        // 瞬时 429 在宽限期内：只标 Degraded、不冷却，key 仍留在轮转中
        // （"429 多重试少冷却"——请求内已切到 key-2，下一请求亲和也偏向 key-2）
        assert_eq!(
            key_1_after.status,
            crate::provider::ProviderKeyStatus::Degraded
        );
        assert_eq!(key_1_after.consecutive_failures, 1);
        assert!(key_1_after.cooldown_until.is_none());
        assert_eq!(
            key_2_after.status,
            crate::provider::ProviderKeyStatus::Active
        );
        assert_eq!(key_2_after.consecutive_failures, 0);
        assert!(key_2_after.last_success_at.is_some());

        wait_until(|| db.get_working_channel_affinity("claude").unwrap().is_some()).await;
        let working_channel = db
            .get_working_channel_affinity("claude")
            .unwrap()
            .expect("successful key should become preferred working channel");
        assert_eq!(working_channel.provider_id, "provider-1");
        assert_eq!(working_channel.key_id.as_deref(), Some(key_2.id.as_str()));
    }

    #[tokio::test]
    async fn anyrouter_429_retries_same_channel_beyond_max_attempts_setting() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(anyrouter_rate_limit_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "anyrouter-1".to_string(),
            "AnyRouter 主力".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-anyrouter"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "anyrouter-1").unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        // max_retries=0 语义（仅尝试一家、无故障转移）：anyrouter 的 429
        // 原地重试预算独立于该设置，两次 429 后第三次仍应成功。
        forwarder.max_attempts = 1;
        forwarder.current_provider_id_at_start = "anyrouter-1".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let result = forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
            .unwrap_or_else(|err| panic!("anyrouter 429 retry should succeed: {}", err.error));

        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
        // 两次 429 + 一次成功，全部打在同一通道上
        assert_eq!(seen_auth.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn anyrouter_429_does_not_mark_key_cooldown_or_degraded() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(always_rate_limited_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "anyrouter-1".to_string(),
            "anyrouter 免费池".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-key-1"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "anyrouter-1").unwrap();

        let key_1 = db
            .add_provider_key(
                "claude",
                "anyrouter-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "anyrouter-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        // 预算清零：跳过原地重试，直接验证"429 落入通用分类但不标记冷却"
        forwarder.anyrouter_429_max_retries = 0;
        forwarder.current_provider_id_at_start = "anyrouter-1".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let err = forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
            .err()
            .expect("all keys rate limited, request should fail");
        assert!(matches!(
            err.error,
            ProxyError::UpstreamError { status: 429, .. }
        ));

        // 两个 key 都被尝试过，但既不 Degraded 也无冷却 —— anyrouter 429 不计入健康度
        //（对照 key_scoped_upstream_failure_fails_over_to_next_key：普通渠道同样场景
        //  会标 Degraded 且 consecutive_failures=1）
        assert_eq!(seen_auth.lock().await.len(), 2);
        for key in [&key_1, &key_2] {
            let after = db
                .get_provider_key("claude", "anyrouter-1", &key.id)
                .unwrap()
                .unwrap();
            assert_eq!(after.status, crate::provider::ProviderKeyStatus::Active);
            assert_eq!(after.consecutive_failures, 0);
            assert!(after.cooldown_until.is_none());
        }
    }

    #[tokio::test]
    async fn successful_key_is_reused_first_on_followup_request() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-1").unwrap();

        let _key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        forwarder.current_provider_id_at_start = "provider-1".to_string();

        let first_attempts = forwarder.router.select_providers("claude").await.unwrap();
        let first = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                first_attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!(
                "second key should succeed after first key fails: {}",
                err.error
            ),
        };
        assert_eq!(first.key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            first.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );

        // 工作通道亲和绑定已异步化，等待落库后再做第二次路由
        wait_until(|| {
            db.get_working_channel_affinity("claude")
                .unwrap()
                .is_some_and(|binding| binding.key_id.as_deref() == Some(key_2.id.as_str()))
        })
        .await;

        let second_attempts = forwarder.router.select_providers("claude").await.unwrap();
        assert_eq!(
            second_attempts[0].key_id.as_deref(),
            Some(key_2.id.as_str())
        );

        let second = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "again"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                second_attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!(
                "working key should be reused and succeed directly: {}",
                err.error
            ),
        };
        assert_eq!(second.key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            second.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );

        assert_eq!(
            *seen_auth.lock().await,
            vec![
                "Bearer sk-key-1".to_string(),
                "Bearer sk-key-2".to_string(),
                "Bearer sk-key-2".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn key_scoped_retry_counts_as_channel_when_retries_are_disabled() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-1").unwrap();

        let key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        db.add_provider_key(
            "claude",
            "provider-1",
            &crate::provider::ProviderKeyInput {
                name: "key-2".to_string(),
                key_value: "sk-key-2".to_string(),
                auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                enabled: true,
                priority: 20,
                weight: 1,
                usage_script: None,
            },
        )
        .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(0), Duration::from_secs(0));
        forwarder.max_attempts = 1;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let err = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(_) => panic!("second key should not be tried when only one channel is allowed"),
            Err(err) => err,
        };

        assert_eq!(
            err.provider.as_ref().map(|provider| provider.id.as_str()),
            Some("provider-1")
        );
        assert_eq!(err.key_id.as_deref(), Some(key_1.id.as_str()));
        assert_eq!(*seen_auth.lock().await, vec!["Bearer sk-key-1".to_string()]);
    }

    #[tokio::test]
    async fn key_scoped_retry_limit_does_not_cross_to_next_provider_when_retries_disabled() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider_1 = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-key-1"
                }
            }),
            None,
        );
        let provider_2 = Provider::with_id(
            "provider-2".to_string(),
            "Provider 2".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "sk-key-2"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider_1).unwrap();
        db.save_provider("claude", &provider_2).unwrap();
        db.add_to_failover_queue("claude", "provider-1").unwrap();
        db.add_to_failover_queue("claude", "provider-2").unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(0), Duration::from_secs(0));
        forwarder.max_attempts = 1;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let attempts = vec![
            ProviderAttempt {
                provider: provider_1,
                key_id: None,
            },
            ProviderAttempt {
                provider: provider_2,
                key_id: None,
            },
        ];

        let err = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(_) => panic!("provider-level retry should remain disabled"),
            Err(err) => err,
        };

        assert!(matches!(
            err.error,
            ProxyError::UpstreamError { status: 429, .. }
        ));
        assert_eq!(
            err.provider.as_ref().map(|provider| provider.id.as_str()),
            Some("provider-1")
        );
        assert_eq!(*seen_auth.lock().await, vec!["Bearer sk-key-1".to_string()]);
    }

    #[tokio::test]
    async fn retryable_failure_only_cools_down_failed_key_and_tries_next_key() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(provider_routing_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider_1 = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-1"
                }
            }),
            None,
        );
        let provider_2 = Provider::with_id(
            "provider-2".to_string(),
            "Provider 2".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-2"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider_1).unwrap();
        db.save_provider("claude", &provider_2).unwrap();
        db.add_to_failover_queue("claude", "provider-1").unwrap();
        db.add_to_failover_queue("claude", "provider-2").unwrap();

        let key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "provider-error-1".to_string(),
                    key_value: "sk-provider-error-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "provider-error-2".to_string(),
                    key_value: "sk-provider-error-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_3 = db
            .add_provider_key(
                "claude",
                "provider-2",
                &crate::provider::ProviderKeyInput {
                    name: "provider-2".to_string(),
                    key_value: "sk-provider-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 3;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("second provider should succeed: {}", err.error),
        };

        assert_eq!(result.key_id.as_deref(), Some(key_3.id.as_str()));
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
        // 5xx 不再连坐 provider：同 provider 的下一个 key 仍被尝试
        assert_eq!(
            *seen_auth.lock().await,
            vec![
                "Bearer sk-provider-error-1".to_string(),
                "Bearer sk-provider-error-2".to_string(),
                "Bearer sk-provider-2".to_string(),
            ]
        );

        // 失败的 key 各自进入冷却（指数退避），互不影响
        let now = chrono::Utc::now().timestamp();
        let key_1_after = db
            .get_provider_key("claude", "provider-1", &key_1.id)
            .unwrap()
            .unwrap();
        let key_2_after = db
            .get_provider_key("claude", "provider-1", &key_2.id)
            .unwrap()
            .unwrap();
        assert_eq!(key_1_after.consecutive_failures, 1);
        assert_eq!(key_2_after.consecutive_failures, 1);
        assert!(key_1_after.cooldown_until.unwrap_or(0) > now);
        assert!(key_2_after.cooldown_until.unwrap_or(0) > now);

        // provider 级健康度不被 key 通道失败污染
        let health = db.get_provider_health("provider-1", "claude").await.unwrap();
        assert!(health.is_healthy);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn key_scoped_failures_consume_channel_failover_budget() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(provider_routing_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider_1 = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-1"
                }
            }),
            None,
        );
        let provider_2 = Provider::with_id(
            "provider-2".to_string(),
            "Provider 2".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-2"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider_1).unwrap();
        db.save_provider("claude", &provider_2).unwrap();
        db.add_to_failover_queue("claude", "provider-1").unwrap();
        db.add_to_failover_queue("claude", "provider-2").unwrap();

        let _key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "rate-limited-1".to_string(),
                    key_value: "sk-key-rate-limited-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let _key_2 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "rate-limited-2".to_string(),
                    key_value: "sk-key-rate-limited-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_3 = db
            .add_provider_key(
                "claude",
                "provider-2",
                &crate::provider::ProviderKeyInput {
                    name: "provider-2".to_string(),
                    key_value: "sk-provider-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 3;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("channel budget should allow provider 2: {}", err.error),
        };

        assert_eq!(result.key_id.as_deref(), Some(key_3.id.as_str()));
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
        assert_eq!(
            *seen_auth.lock().await,
            vec![
                "Bearer sk-key-rate-limited-1".to_string(),
                "Bearer sk-key-rate-limited-2".to_string(),
                "Bearer sk-provider-2".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn interleaved_attempt_order_still_reaches_next_provider() {
        // 回归：旧 pending 跳过机制下，交错顺序 [A:k2, B, A:k1] 中 A:k2 失败
        // 会设置 pending=A 并 continue 跳过 B；走到 A:k1 清除 pending 后迭代器
        // 已越过 B 永不回头 —— A 全部 key 失败时 B 根本没被尝试。
        // 删除 pending 机制后，循环按顺序逐个尝试，B 必然被命中。
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(provider_routing_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider_a = Provider::with_id(
            "provider-a".to_string(),
            "Provider A".to_string(),
            json!({
                "base_url": base_url,
                "env": { "ANTHROPIC_AUTH_TOKEN": "legacy-a" }
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "provider-b".to_string(),
            "Provider B".to_string(),
            json!({
                "base_url": base_url,
                "env": { "ANTHROPIC_AUTH_TOKEN": "sk-provider-2" }
            }),
            None,
        );
        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();

        let key_a1 = db
            .add_provider_key(
                "claude",
                "provider-a",
                &crate::provider::ProviderKeyInput {
                    name: "a-1".to_string(),
                    key_value: "sk-key-rate-limited-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_a2 = db
            .add_provider_key(
                "claude",
                "provider-a",
                &crate::provider::ProviderKeyInput {
                    name: "a-2".to_string(),
                    key_value: "sk-key-rate-limited-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 3;
        forwarder.current_provider_id_at_start = "provider-a".to_string();

        // 手工构造曾触发 bug 的交错顺序（affinity 单点置顶的产物）
        let expanded = forwarder.router.select_providers("claude").await;
        drop(expanded); // 仅为确保路由可用，顺序由下方手工指定
        let mut provider_a_k2 = provider_a.clone();
        provider_a_k2.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"] =
            json!("sk-key-rate-limited-2");
        let mut provider_a_k1 = provider_a.clone();
        provider_a_k1.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"] =
            json!("sk-key-rate-limited-1");
        let attempts = vec![
            ProviderAttempt {
                provider: provider_a_k2,
                key_id: Some(key_a2.id.clone()),
            },
            ProviderAttempt {
                provider: provider_b,
                key_id: None,
            },
            ProviderAttempt {
                provider: provider_a_k1,
                key_id: Some(key_a1.id.clone()),
            },
        ];

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("provider B must be reached: {}", err.error),
        };

        assert_eq!(result.provider.id, "provider-b");
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );
        // A:k2 失败后下一个 attempt（B）立即被尝试，而非被跳过
        assert_eq!(
            *seen_auth.lock().await,
            vec![
                "Bearer sk-key-rate-limited-2".to_string(),
                "Bearer sk-provider-2".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn key_scoped_failure_does_not_increment_provider_health_failures() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-1").unwrap();

        db.add_provider_key(
            "claude",
            "provider-1",
            &crate::provider::ProviderKeyInput {
                name: "key-1".to_string(),
                key_value: "sk-key-1".to_string(),
                auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                enabled: true,
                priority: 10,
                weight: 1,
                usage_script: None,
            },
        )
        .unwrap();
        db.add_provider_key(
            "claude",
            "provider-1",
            &crate::provider::ProviderKeyInput {
                name: "key-2".to_string(),
                key_value: "sk-key-2".to_string(),
                auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                enabled: true,
                priority: 20,
                weight: 1,
                usage_script: None,
            },
        )
        .unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("second key should succeed: {}", err.error),
        };
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );

        let health = db
            .get_provider_health("provider-1", "claude")
            .await
            .expect("provider health should be readable");
        assert!(health.is_healthy);
        assert_eq!(health.consecutive_failures, 0);
        assert_eq!(
            *seen_auth.lock().await,
            vec!["Bearer sk-key-1".to_string(), "Bearer sk-key-2".to_string()]
        );
    }

    #[tokio::test]
    async fn key_scoped_failure_rebinds_session_affinity_to_successful_key() {
        install_test_crypto_provider();

        let seen_auth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/messages", post(key_failover_mock_handler))
            .with_state(seen_auth.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("mock upstream server");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-1".to_string(),
            "Provider 1".to_string(),
            json!({
                "base_url": base_url,
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "legacy-key"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-1").unwrap();

        let key_1 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-1".to_string(),
                    key_value: "sk-key-1".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 10,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        let key_2 = db
            .add_provider_key(
                "claude",
                "provider-1",
                &crate::provider::ProviderKeyInput {
                    name: "key-2".to_string(),
                    key_value: "sk-key-2".to_string(),
                    auth_field: Some("env.ANTHROPIC_AUTH_TOKEN".to_string()),
                    enabled: true,
                    priority: 20,
                    weight: 1,
                    usage_script: None,
                },
            )
            .unwrap();
        db.upsert_session_affinity("claude", "session-1", "provider-1", Some(&key_1.id))
            .expect("seed session affinity");

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        forwarder.current_provider_id_at_start = "provider-1".to_string();
        forwarder.session_id = "session-1".to_string();
        forwarder.session_client_provided = true;
        let attempts = forwarder
            .router
            .select_providers_for_session("claude", Some("session-1"))
            .await
            .unwrap();
        assert_eq!(attempts[0].key_id.as_deref(), Some(key_1.id.as_str()));

        let result = match forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-3-5-sonnet-latest",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await
        {
            Ok(result) => result,
            Err(err) => panic!("second key should succeed: {}", err.error),
        };
        assert_eq!(result.key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            result.response.bytes().await.unwrap(),
            Bytes::from_static(b"{\"ok\":true}")
        );

        // Session 亲和重绑定已异步化，轮询等待落库
        wait_until(|| {
            db.get_session_affinity("claude", "session-1")
                .unwrap()
                .is_some_and(|binding| binding.key_id.as_deref() == Some(key_2.id.as_str()))
        })
        .await;
        let affinity = db
            .get_session_affinity("claude", "session-1")
            .expect("get session affinity")
            .expect("successful key should be rebound to session");
        assert_eq!(affinity.provider_id, "provider-1");
        assert_eq!(affinity.key_id.as_deref(), Some(key_2.id.as_str()));
        assert_eq!(
            *seen_auth.lock().await,
            vec!["Bearer sk-key-1".to_string(), "Bearer sk-key-2".to_string()]
        );
    }

    #[tokio::test]
    async fn non_streaming_body_read_error_is_retryable_before_success_record() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("body boom"))
            }),
        );

        let err = match forwarder
            .prepare_success_response_for_failover(response, false)
            .await
        {
            Ok(_) => panic!("body read errors should fail the attempt"),
            Err(err) => err,
        };

        assert!(matches!(err, ProxyError::ForwardFailed(_)));
    }

    #[tokio::test]
    async fn streaming_success_primes_first_chunk_and_replays_it() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::iter(vec![
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"first")),
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"second")),
            ]),
        );

        let prepared = forwarder
            .prepare_success_response_for_failover(response, true)
            .await
            .expect("stream should be primed");

        assert_eq!(
            prepared.bytes().await.unwrap(),
            Bytes::from_static(b"firstsecond")
        );
    }

    #[tokio::test]
    async fn streaming_first_chunk_error_is_retryable_before_success_record() {
        let forwarder = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        let response = ProxyResponse::streamed(
            StatusCode::OK,
            HeaderMap::new(),
            futures::stream::once(async {
                Err::<Bytes, std::io::Error>(std::io::Error::other("first chunk boom"))
            }),
        );

        let err = match forwarder
            .prepare_success_response_for_failover(response, true)
            .await
        {
            Ok(_) => panic!("first chunk errors should fail the attempt"),
            Err(err) => err,
        };

        assert!(matches!(err, ProxyError::ForwardFailed(_)));
    }





    #[test]
    fn exact_header_case_preserved_for_native_claude_only() {
        let provider = test_provider();

        assert!(should_preserve_exact_header_case(
            "Claude",
            &provider,
            Some("anthropic")
        ));
        assert!(!should_preserve_exact_header_case(
            "Claude",
            &provider,
            Some("openai_responses")
        ));
        assert!(!should_preserve_exact_header_case("Codex", &provider, None));
        assert!(!should_preserve_exact_header_case(
            "Gemini", &provider, None
        ));
    }


    #[test]
    fn rewrite_claude_transform_endpoint_strips_beta_for_chat_completions() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&foo=bar",
            "openai_chat",
            &json!({ "model": "gpt-5.4" }),
        );

        assert_eq!(endpoint, "/v1/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_claude_transform_endpoint_strips_beta_for_responses() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/claude/v1/messages?beta=true&x-id=1",
            "openai_responses",
            &json!({ "model": "gpt-5.4" }),
        );

        assert_eq!(endpoint, "/v1/responses?x-id=1");
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    #[test]
    fn rewrite_codex_responses_endpoint_to_chat_preserves_query() {
        let (endpoint, passthrough_query) =
            rewrite_codex_responses_endpoint_to_chat("/v1/responses?foo=bar");

        assert_eq!(endpoint, "/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }

    #[test]
    fn rewrite_codex_responses_compact_endpoint_to_chat_preserves_query() {
        let (endpoint, passthrough_query) =
            rewrite_codex_responses_endpoint_to_chat("/v1/responses/compact?foo=bar");

        assert_eq!(endpoint, "/chat/completions?foo=bar");
        assert_eq!(passthrough_query.as_deref(), Some("foo=bar"));
    }



    #[test]
    fn rewrite_claude_transform_endpoint_maps_gemini_generate_content() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true&x-id=1",
            "gemini_native",
            &json!({ "model": "gemini-2.5-pro" }),
        );

        assert_eq!(
            endpoint,
            "/v1beta/models/gemini-2.5-pro:generateContent?x-id=1"
        );
        assert_eq!(passthrough_query.as_deref(), Some("x-id=1"));
    }

    /// Regression: body.model arriving as the resource-name form
    /// `models/gemini-2.5-pro` must not produce a doubled
    /// `/v1beta/models/models/...` path.
    #[test]
    fn rewrite_claude_transform_endpoint_strips_gemini_model_resource_prefix() {
        let (endpoint, _) = rewrite_claude_transform_endpoint(
            "/v1/messages",
            "gemini_native",
            &json!({ "model": "models/gemini-2.5-pro" }),
        );

        assert_eq!(endpoint, "/v1beta/models/gemini-2.5-pro:generateContent");
    }

    #[test]
    fn rewrite_claude_transform_endpoint_maps_gemini_streaming() {
        let (endpoint, passthrough_query) = rewrite_claude_transform_endpoint(
            "/v1/messages?beta=true",
            "gemini_native",
            &json!({ "model": "gemini-2.5-flash", "stream": true }),
        );

        assert_eq!(
            endpoint,
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(passthrough_query.as_deref(), Some("alt=sse"));
    }

    #[test]
    fn append_query_to_full_url_preserves_existing_query_string() {
        let url = append_query_to_full_url("https://relay.example/api?foo=bar", Some("x-id=1"));

        assert_eq!(url, "https://relay.example/api?foo=bar&x-id=1");
    }

    #[test]
    fn build_gemini_native_url_uses_origin_when_base_ends_with_v1beta() {
        let url = crate::proxy::gemini_url::build_gemini_native_url(
            "https://generativelanguage.googleapis.com/v1beta",
            "/v1beta/models/gemini-2.5-pro:generateContent",
        );

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn build_gemini_native_url_uses_origin_when_base_already_contains_models_prefix() {
        let url = crate::proxy::gemini_url::build_gemini_native_url(
            "https://generativelanguage.googleapis.com/v1beta/models",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
        );

        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn resolve_gemini_native_url_keeps_opaque_full_url_as_is() {
        let url = crate::proxy::gemini_url::resolve_gemini_native_url(
            "https://relay.example/custom/generate-content",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            true,
        );

        assert_eq!(url, "https://relay.example/custom/generate-content?alt=sse");
    }

    #[test]
    fn force_identity_for_stream_flag_requests() {
        let headers = HeaderMap::new();

        assert!(should_force_identity_encoding(
            "/v1/responses",
            &json!({ "stream": true }),
            &headers
        ));
    }

    #[test]
    fn force_identity_for_gemini_stream_endpoints() {
        let headers = HeaderMap::new();

        assert!(should_force_identity_encoding(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            &json!({ "model": "gemini-2.5-pro" }),
            &headers
        ));
    }

    #[test]
    fn streaming_request_detects_gemini_sse_without_body_stream_flag() {
        let headers = HeaderMap::new();

        assert!(is_streaming_request(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            &json!({ "model": "gemini-2.5-pro" }),
            &headers
        ));
    }

    #[test]
    fn force_identity_for_sse_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        assert!(should_force_identity_encoding(
            "/v1/responses",
            &json!({ "model": "gpt-5" }),
            &headers
        ));
    }

    #[test]
    fn non_streaming_requests_allow_automatic_compression() {
        let headers = HeaderMap::new();

        assert!(!should_force_identity_encoding(
            "/v1/responses",
            &json!({ "model": "gpt-5" }),
            &headers
        ));
    }





    // ===== P3: forwarder 层 media 开关回归测试 =====
    // 验证 gate 在 forwarder 这一层的"接线"，而非 media_sanitizer 纯函数本身。

    fn forwarder_with_rectifier(config: RectifierConfig) -> RequestForwarder {
        let mut fwd = test_forwarder(Duration::from_secs(1), Duration::from_secs(1));
        fwd.rectifier_config = config;
        fwd
    }

    fn provider_with_settings(settings_config: Value) -> Provider {
        let mut p = test_provider();
        p.settings_config = settings_config;
        p
    }

    fn body_with_image(model: &str) -> Value {
        json!({
            "model": model,
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "abc" } }
                ]
            }]
        })
    }

    fn body_with_codex_input_image(model: &str) -> Value {
        json!({
            "model": model,
            "input": [{
                "role": "user",
                "content": [
                    { "type": "input_image", "image_url": "data:image/png;base64,abc" }
                ]
            }]
        })
    }

    fn image_unsupported_error() -> ProxyError {
        ProxyError::UpstreamError {
            status: 400,
            body: Some(
                r#"{"error":{"message":"This model does not support image input"}}"#.to_string(),
            ),
            retry_after: None,
        }
    }
    #[test]
    fn prevention_replaces_when_all_switches_on_and_model_in_heuristic_list() {
        let fwd = forwarder_with_rectifier(RectifierConfig::default());
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        let replaced = fwd.apply_media_prevention(&mut body, &provider);

        assert_eq!(replaced, 1, "默认全开 + 名单内模型应预替换");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn prevention_skipped_when_media_fallback_off() {
        // 关闭 request_media_fallback：即使名单命中也不预替换。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_fallback: false,
            ..RectifierConfig::default()
        });
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        let replaced = fwd.apply_media_prevention(&mut body, &provider);

        assert_eq!(replaced, 0);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
    }

    #[test]
    fn prevention_skipped_when_master_switch_off() {
        let fwd = forwarder_with_rectifier(RectifierConfig {
            enabled: false,
            ..RectifierConfig::default()
        });
        let provider = provider_with_settings(json!({}));
        let mut body = body_with_image("deepseek-v4-pro");

        assert_eq!(fwd.apply_media_prevention(&mut body, &provider), 0);
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
    }

    #[test]
    fn prevention_heuristic_off_skips_list_but_keeps_explicit_text_only() {
        // 关闭 request_media_heuristic：名单预测失效，但显式声明 text-only 仍预替换。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_heuristic: false,
            ..RectifierConfig::default()
        });

        // (a) 名单内模型、无显式声明 → 不再预替换
        let bare_provider = provider_with_settings(json!({}));
        let mut list_body = body_with_image("deepseek-v4-pro");
        assert_eq!(
            fwd.apply_media_prevention(&mut list_body, &bare_provider),
            0,
            "heuristic 关闭后名单模型不应被预替换"
        );
        assert_eq!(list_body["messages"][0]["content"][0]["type"], "image");

        // (b) 显式声明 text-only → 仍预替换（声明驱动，不受 heuristic 开关影响）
        let declared_provider = provider_with_settings(json!({
            "models": [ { "id": "some-text-model", "input": ["text"] } ]
        }));
        let mut declared_body = body_with_image("some-text-model");
        assert_eq!(
            fwd.apply_media_prevention(&mut declared_body, &declared_provider),
            1,
            "显式 text-only 即使关闭 heuristic 也应预替换"
        );
        assert_eq!(declared_body["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn reactive_triggers_when_all_switches_on() {
        let fwd = forwarder_with_rectifier(RectifierConfig::default());
        let body = body_with_image("any-model");
        assert!(fwd.media_retry_should_trigger("Claude", false, &body, &image_unsupported_error()));
    }

    #[test]
    fn reactive_triggers_for_codex_image_url_deserialize_errors() {
        let fwd = forwarder_with_rectifier(RectifierConfig::default());
        let body = body_with_codex_input_image("deepseek-v4-flash");
        let error = ProxyError::UpstreamError {
            status: 400,
            body: Some(
                r#"{"error":{"message":"Failed to deserialize the JSON body into the target type: messages[11]: unknown variant image_url, expected text"}}"#
                    .to_string(),
            ),
            retry_after: None,
        };

        assert!(fwd.media_retry_should_trigger("Codex", false, &body, &error));
    }

    #[test]
    fn reactive_skipped_when_media_fallback_off() {
        // 关闭 request_media_fallback：上游报图片错误也不触发兜底重试。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_fallback: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(!fwd.media_retry_should_trigger(
            "Claude",
            false,
            &body,
            &image_unsupported_error()
        ));
    }

    #[test]
    fn reactive_skipped_when_master_switch_off() {
        let fwd = forwarder_with_rectifier(RectifierConfig {
            enabled: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(!fwd.media_retry_should_trigger(
            "Claude",
            false,
            &body,
            &image_unsupported_error()
        ));
    }

    #[test]
    fn reactive_unaffected_by_heuristic_switch() {
        // 关闭 request_media_heuristic 不影响反应式兜底——它是上游实测错误后的恢复，不是预测。
        let fwd = forwarder_with_rectifier(RectifierConfig {
            request_media_heuristic: false,
            ..RectifierConfig::default()
        });
        let body = body_with_image("any-model");
        assert!(fwd.media_retry_should_trigger("Claude", false, &body, &image_unsupported_error()));
    }
    // ====================================================================
    // hop-by-hop / 自定义头规则 / anyrouter 适配
    // ====================================================================

    #[test]
    fn hop_by_hop_request_headers_are_detected_including_connection_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONNECTION,
            HeaderValue::from_static("keep-alive, X-Secret-Hop"),
        );
        let tokens = connection_declared_header_tokens(&headers);
        assert!(tokens.contains(&"x-secret-hop".to_string()));

        for name in [
            "Connection",
            "keep-alive",
            "TE",
            "Trailer",
            "Upgrade",
            "Proxy-Authorization",
            "x-secret-hop",
        ] {
            assert!(
                is_hop_by_hop_request_header(name, &tokens),
                "{name} should be hop-by-hop"
            );
        }
        assert!(!is_hop_by_hop_request_header("anthropic-beta", &tokens));
        assert!(!is_hop_by_hop_request_header("x-keep-me", &tokens));
    }

    fn header_rule(action: &str, name: &str, value: &str) -> crate::provider::CustomHeaderRule {
        crate::provider::CustomHeaderRule {
            action: action.to_string(),
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn provider_header_rules_apply_override_append_remove() {
        let mut headers = http::HeaderMap::new();
        headers.insert("x-old", HeaderValue::from_static("v1"));
        headers.insert("x-drop", HeaderValue::from_static("bye"));

        apply_provider_header_rules(
            &mut headers,
            &[
                header_rule("override", "x-old", "v2"),
                header_rule("append", "x-multi", "a"),
                header_rule("append", "x-multi", "b"),
                header_rule("remove", "x-drop", ""),
            ],
        );

        assert_eq!(headers.get("x-old").unwrap(), "v2");
        let multi: Vec<_> = headers.get_all("x-multi").iter().collect();
        assert_eq!(multi.len(), 2);
        assert!(headers.get("x-drop").is_none());
    }

    #[test]
    fn provider_header_rules_remove_single_csv_token() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("claude-code-20250219,context-1m-2025-08-07"),
        );

        apply_provider_header_rules(
            &mut headers,
            &[header_rule("remove", "anthropic-beta", "context-1m-2025-08-07")],
        );

        assert_eq!(
            headers.get("anthropic-beta").unwrap(),
            "claude-code-20250219"
        );

        // 摘空所有 token 后整头删除
        apply_provider_header_rules(
            &mut headers,
            &[header_rule("remove", "anthropic-beta", "claude-code-20250219")],
        );
        assert!(headers.get("anthropic-beta").is_none());
    }

    #[test]
    fn provider_header_rules_never_touch_auth_headers() {
        let mut headers = http::HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer real"));
        headers.insert("x-api-key", HeaderValue::from_static("sk-real"));

        apply_provider_header_rules(
            &mut headers,
            &[
                header_rule("override", "Authorization", "Bearer fake"),
                header_rule("remove", "X-Api-Key", ""),
                header_rule("override", "x-goog-api-key", "evil"),
                header_rule("unknown-action", "x-any", "v"),
                header_rule("override", "bad name\n", "v"),
            ],
        );

        assert_eq!(headers.get("authorization").unwrap(), "Bearer real");
        assert_eq!(headers.get("x-api-key").unwrap(), "sk-real");
        assert!(headers.get("x-goog-api-key").is_none());
    }

    #[test]
    fn anyrouter_channel_is_detected_by_name_or_known_hosts() {
        // 名称命中（不分大小写）
        assert!(is_anyrouter_channel("AnyRouter 主力", "https://example.com"));
        // 两个已知域名都命中（与渠道名无关）
        assert!(is_anyrouter_channel("中转A", "https://anyrouter.top"));
        assert!(is_anyrouter_channel(
            "备用",
            "https://a-ocnfniawgw.cn-shanghai.fcapp.run/api"
        ));
        // 子域也命中
        assert!(is_anyrouter_channel("x", "https://api.anyrouter.top/v1"));
        // 其他域名 + 无关名称不命中
        assert!(!is_anyrouter_channel("packy", "https://api.packycode.com"));
        // 相似但不同的域名不命中（不能用 contains 误匹配）
        assert!(!is_anyrouter_channel("x", "https://fakeanyrouter.top.evil.com"));
    }

    #[test]
    fn anthropic_beta_token_merge_is_idempotent() {
        assert_eq!(
            merge_anthropic_beta_token(None, ANYROUTER_CONTEXT_1M_BETA),
            "context-1m-2025-08-07"
        );
        assert_eq!(
            merge_anthropic_beta_token(
                Some("claude-code-20250219".to_string()),
                ANYROUTER_CONTEXT_1M_BETA
            ),
            "claude-code-20250219,context-1m-2025-08-07"
        );
        // 已含该 token（带空格）不重复追加
        assert_eq!(
            merge_anthropic_beta_token(
                Some("a, context-1m-2025-08-07".to_string()),
                ANYROUTER_CONTEXT_1M_BETA
            ),
            "a, context-1m-2025-08-07"
        );
    }

    #[test]
    fn adaptive_thinking_injection_respects_existing_value() {
        let mut body = json!({"model": "claude-sonnet-4-6", "messages": []});
        assert!(inject_adaptive_thinking_if_missing(&mut body));
        assert_eq!(body["thinking"]["type"], "adaptive");

        let mut with_thinking =
            json!({"thinking": {"type": "enabled", "budget_tokens": 2048}});
        assert!(!inject_adaptive_thinking_if_missing(&mut with_thinking));
        assert_eq!(with_thinking["thinking"]["type"], "enabled");
    }

    #[test]
    fn invalid_responses_request_error_is_matched() {
        assert!(is_invalid_responses_request_error(&ProxyError::UpstreamError {
            status: 400,
            body: Some(
                r#"{"error":{"code":"invalid_responses_request","message":"bad"}}"#.to_string()
            ),
            retry_after: None,
        }));
        assert!(is_invalid_responses_request_error(&ProxyError::UpstreamError {
            status: 400,
            body: Some(r#"{"error":{"message":"Invalid_Responses_Request: nope"}}"#.to_string()),
            retry_after: None,
        }));
        // 非 400 / 无关错误体不命中
        assert!(!is_invalid_responses_request_error(&ProxyError::UpstreamError {
            status: 500,
            body: Some("invalid_responses_request".to_string()),
            retry_after: None,
        }));
        assert!(!is_invalid_responses_request_error(&ProxyError::UpstreamError {
            status: 400,
            body: Some("other error".to_string()),
            retry_after: None,
        }));
    }

    #[test]
    fn codex_body_strip_removes_encrypted_and_tool_search() {
        let body = json!({
            "model": "gpt-5",
            "input": [
                {"type": "message", "content": [{"type": "input_text", "text": "hi"}]},
                {"type": "tool_search_call", "queries": ["x"]},
                {"type": "reasoning", "encrypted_content": "ZZZ", "summary": []}
            ],
            "nested": {"deep": [{"encrypted_content": "AAA"}]}
        });
        let stripped = codex_body_without_encrypted_and_tool_search(&body)
            .expect("should strip something");
        let dumped = serde_json::to_string(&stripped).unwrap();
        assert!(!dumped.contains("encrypted_content"));
        assert!(!dumped.contains("tool_search_call"));
        // 正常消息保留
        assert_eq!(stripped["input"].as_array().unwrap().len(), 2);

        // 无可剥内容 → None（不值得重试）
        let clean = json!({"model": "gpt-5", "input": [{"type": "message"}]});
        assert!(codex_body_without_encrypted_and_tool_search(&clean).is_none());
    }

    /// 端到端：hop-by-hop / dangerous 头剥离、自定义头规则、anyrouter 的
    /// context-1m beta 与 adaptive thinking 注入，一次请求全验证。
    #[tokio::test]
    async fn upstream_request_headers_are_sanitized_and_anyrouter_adapted() {
        install_test_crypto_provider();

        type Captured = Arc<Mutex<Vec<(HeaderMap, Value)>>>;
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));

        async fn capture_handler(
            State(captured): State<Captured>,
            headers: HeaderMap,
            body: Bytes,
        ) -> (StatusCode, &'static str) {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            captured.lock().await.push((headers, body));
            (StatusCode::OK, r#"{"ok":true}"#)
        }

        let app = Router::new()
            .route("/v1/messages", post(capture_handler))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock upstream");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let mut provider = Provider::with_id(
            "provider-anyrouter".to_string(),
            "AnyRouter 备用".to_string(),
            json!({
                "base_url": base_url,
                "env": {"ANTHROPIC_AUTH_TOKEN": "sk-test"}
            }),
            None,
        );
        provider.meta = Some(crate::provider::ProviderMeta {
            header_rules: vec![
                header_rule("override", "x-gateway", "cc-switch"),
                header_rule("remove", "x-keep-me", ""),
            ],
            ..Default::default()
        });
        db.save_provider("claude", &provider).unwrap();
        db.set_current_provider("claude", "provider-anyrouter").unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 1;
        forwarder.current_provider_id_at_start = "provider-anyrouter".to_string();
        let attempts = forwarder.router.select_providers("claude").await.unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-secret-hop"),
        );
        headers.insert("x-secret-hop", HeaderValue::from_static("zzz"));
        headers.insert("te", HeaderValue::from_static("trailers"));
        headers.insert("x-trace-id", HeaderValue::from_static("t1"));
        headers.insert(
            "anthropic-dangerous-direct-browser-access",
            HeaderValue::from_static("true"),
        );
        headers.insert("x-keep-me", HeaderValue::from_static("should-be-removed"));

        let result = forwarder
            .forward_with_retry(
                &AppType::Claude,
                http::Method::POST,
                "/v1/messages",
                json!({
                    "model": "claude-sonnet-4-6",
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
                headers,
                Extensions::new(),
                attempts,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(err) => panic!("forward should succeed: {}", err.error),
        };
        drop(result);

        let captured = captured.lock().await;
        assert_eq!(captured.len(), 1);
        let (upstream_headers, upstream_body) = &captured[0];

        // hop-by-hop（静态 + Connection 点名）与追踪头被剥离
        for absent in ["connection", "te", "x-secret-hop", "x-trace-id"] {
            assert!(
                upstream_headers.get(absent).is_none(),
                "{absent} should not reach upstream"
            );
        }
        // anthropic 原生格式：dangerous 头透传
        assert_eq!(
            upstream_headers
                .get("anthropic-dangerous-direct-browser-access")
                .unwrap(),
            "true"
        );
        // 自定义规则：覆盖 + 删除
        assert_eq!(upstream_headers.get("x-gateway").unwrap(), "cc-switch");
        assert!(upstream_headers.get("x-keep-me").is_none());
        // anyrouter：beta 同时含 claude-code 与 context-1m
        let beta = upstream_headers
            .get("anthropic-beta")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(beta.contains("claude-code-20250219"), "beta={beta}");
        assert!(beta.contains("context-1m-2025-08-07"), "beta={beta}");
        // anyrouter：注入 adaptive thinking
        assert_eq!(upstream_body["thinking"]["type"], "adaptive");
    }

    /// 端到端：anyrouter Codex 上游 400 invalid_responses_request →
    /// 剥 encrypted_content / tool_search_* 后同通道重试成功。
    #[tokio::test]
    async fn anyrouter_codex_invalid_responses_request_retries_with_stripped_body() {
        install_test_crypto_provider();

        type Captured = Arc<Mutex<Vec<Value>>>;
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));

        async fn responses_handler(
            State(captured): State<Arc<Mutex<Vec<Value>>>>,
            body: Bytes,
        ) -> (StatusCode, &'static str) {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let mut seen = captured.lock().await;
            seen.push(body);
            if seen.len() == 1 {
                (
                    StatusCode::BAD_REQUEST,
                    r#"{"error":{"code":"invalid_responses_request","message":"encrypted content could not be parsed"}}"#,
                )
            } else {
                (StatusCode::OK, r#"{"id":"resp_1","status":"completed"}"#)
            }
        }

        let app = Router::new()
            .route("/v1/responses", post(responses_handler))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("mock upstream");
        });

        let db = Arc::new(Database::memory().expect("memory db"));
        let provider = Provider::with_id(
            "provider-ar-codex".to_string(),
            "anyrouter-codex".to_string(),
            json!({
                "base_url": format!("{base_url}/v1"),
                "env": {"OPENAI_API_KEY": "sk-test"}
            }),
            None,
        );
        db.save_provider("codex", &provider).unwrap();
        db.set_current_provider("codex", "provider-ar-codex").unwrap();

        let mut forwarder =
            test_forwarder_with_db(db.clone(), Duration::from_secs(2), Duration::from_secs(2));
        forwarder.max_attempts = 2;
        forwarder.current_provider_id_at_start = "provider-ar-codex".to_string();
        let attempts = forwarder.router.select_providers("codex").await.unwrap();

        let result = forwarder
            .forward_with_retry(
                &AppType::Codex,
                http::Method::POST,
                "/v1/responses",
                json!({
                    "model": "gpt-5",
                    "stream": false,
                    "input": [
                        {"type": "message", "role": "user",
                         "content": [{"type": "input_text", "text": "hi"}]},
                        {"type": "tool_search_call", "queries": ["q"]},
                        {"type": "reasoning", "encrypted_content": "ZZZ", "summary": []}
                    ]
                }),
                HeaderMap::new(),
                Extensions::new(),
                attempts,
            )
            .await;
        let result = match result {
            Ok(result) => result,
            Err(err) => panic!("retry with stripped body should succeed: {}", err.error),
        };
        drop(result);

        let captured = captured.lock().await;
        assert_eq!(captured.len(), 2, "exactly one same-channel retry");
        let first = serde_json::to_string(&captured[0]).unwrap();
        assert!(first.contains("encrypted_content"));
        assert!(first.contains("tool_search_call"));
        let second = serde_json::to_string(&captured[1]).unwrap();
        assert!(!second.contains("encrypted_content"));
        assert!(!second.contains("tool_search_call"));
        assert_eq!(captured[1]["input"].as_array().unwrap().len(), 2);
    }
}
