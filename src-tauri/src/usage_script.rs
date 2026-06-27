use rquickjs::{Context, Function, Runtime};
use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use url::{Host, Url};

use crate::error::AppError;

/// 执行用量查询脚本
pub async fn execute_usage_script(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    timeout_secs: u64,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<Value, AppError> {
    // 检测是否为自定义模板模式
    // 优先使用前端传递的 template_type
    let is_custom_template = template_type.map(|t| t == "custom").unwrap_or(false);

    // 1. 替换模板变量，避免泄露敏感信息
    let script_with_vars =
        build_script_with_vars(script_code, api_key, base_url, access_token, user_id);

    // 2. 验证 base_url 的安全性（仅当提供了 base_url 时）
    // 自定义模板模式下，用户可能不使用模板变量，而是直接在脚本中写完整 URL
    if should_validate_base_url(base_url, is_custom_template) {
        validate_base_url(base_url)?;
        validate_url_resolved_addrs(base_url, "base_url").await?;
    }

    // 3. 在独立作用域中提取 request 配置（确保 Runtime/Context 在 await 前释放）
    let request_config = {
        let runtime = Runtime::new().map_err(|e| {
            AppError::localized(
                "usage_script.runtime_create_failed",
                format!("创建 JS 运行时失败: {e}"),
                format!("Failed to create JS runtime: {e}"),
            )
        })?;
        let context = Context::full(&runtime).map_err(|e| {
            AppError::localized(
                "usage_script.context_create_failed",
                format!("创建 JS 上下文失败: {e}"),
                format!("Failed to create JS context: {e}"),
            )
        })?;

        context.with(|ctx| {
            // 执行用户代码，获取配置对象
            let config: rquickjs::Object = ctx.eval(script_with_vars.clone()).map_err(|e| {
                AppError::localized(
                    "usage_script.config_parse_failed",
                    format!("解析配置失败: {e}"),
                    format!("Failed to parse config: {e}"),
                )
            })?;

            // 提取 request 配置
            let request: rquickjs::Object = config.get("request").map_err(|e| {
                AppError::localized(
                    "usage_script.request_missing",
                    format!("缺少 request 配置: {e}"),
                    format!("Missing request config: {e}"),
                )
            })?;

            // 将 request 转换为 JSON 字符串
            let request_json: String = ctx
                .json_stringify(request)
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.request_serialize_failed",
                        format!("序列化 request 失败: {e}"),
                        format!("Failed to serialize request: {e}"),
                    )
                })?
                .ok_or_else(|| {
                    AppError::localized(
                        "usage_script.serialize_none",
                        "序列化返回 None",
                        "Serialization returned None",
                    )
                })?
                .get()
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.get_string_failed",
                        format!("获取字符串失败: {e}"),
                        format!("Failed to get string: {e}"),
                    )
                })?;

            Ok::<_, AppError>(request_json)
        })?
    }; // Runtime 和 Context 在这里被 drop

    // 4. 解析 request 配置
    let request: RequestConfig = serde_json::from_str(&request_config).map_err(|e| {
        AppError::localized(
            "usage_script.request_format_invalid",
            format!("request 配置格式错误: {e}"),
            format!("Invalid request config format: {e}"),
        )
    })?;

    // 5. 验证请求 URL（HTTPS 强制 + 同源检查）
    validate_request_url(&request.url, base_url, is_custom_template)?;
    if !is_custom_template {
        validate_url_resolved_addrs(&request.url, "request").await?;
    }

    // 6. 发送 HTTP 请求
    let response_data =
        send_http_request(&request, base_url, is_custom_template, timeout_secs).await?;

    // 7. 在独立作用域中执行 extractor（确保 Runtime/Context 在函数结束前释放）
    let result: Value = {
        let runtime = Runtime::new().map_err(|e| {
            AppError::localized(
                "usage_script.runtime_create_failed",
                format!("创建 JS 运行时失败: {e}"),
                format!("Failed to create JS runtime: {e}"),
            )
        })?;
        let context = Context::full(&runtime).map_err(|e| {
            AppError::localized(
                "usage_script.context_create_failed",
                format!("创建 JS 上下文失败: {e}"),
                format!("Failed to create JS context: {e}"),
            )
        })?;

        context.with(|ctx| {
            // 重新 eval 获取配置对象
            let config: rquickjs::Object = ctx.eval(script_with_vars.clone()).map_err(|e| {
                AppError::localized(
                    "usage_script.config_reparse_failed",
                    format!("重新解析配置失败: {e}"),
                    format!("Failed to re-parse config: {e}"),
                )
            })?;

            // 提取 extractor 函数
            let extractor: Function = config.get("extractor").map_err(|e| {
                AppError::localized(
                    "usage_script.extractor_missing",
                    format!("缺少 extractor 函数: {e}"),
                    format!("Missing extractor function: {e}"),
                )
            })?;

            // 将响应数据转换为 JS 值
            let response_js: rquickjs::Value =
                ctx.json_parse(response_data.as_str()).map_err(|e| {
                    AppError::localized(
                        "usage_script.response_parse_failed",
                        format!("解析响应 JSON 失败: {e}"),
                        format!("Failed to parse response JSON: {e}"),
                    )
                })?;

            // 调用 extractor(response)
            let result_js: rquickjs::Value = extractor.call((response_js,)).map_err(|e| {
                AppError::localized(
                    "usage_script.extractor_exec_failed",
                    format!("执行 extractor 失败: {e}"),
                    format!("Failed to execute extractor: {e}"),
                )
            })?;

            // 转换为 JSON 字符串
            let result_json: String = ctx
                .json_stringify(result_js)
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.result_serialize_failed",
                        format!("序列化结果失败: {e}"),
                        format!("Failed to serialize result: {e}"),
                    )
                })?
                .ok_or_else(|| {
                    AppError::localized(
                        "usage_script.serialize_none",
                        "序列化返回 None",
                        "Serialization returned None",
                    )
                })?
                .get()
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.get_string_failed",
                        format!("获取字符串失败: {e}"),
                        format!("Failed to get string: {e}"),
                    )
                })?;

            // 解析为 serde_json::Value
            serde_json::from_str(&result_json).map_err(|e| {
                AppError::localized(
                    "usage_script.json_parse_failed",
                    format!("JSON 解析失败: {e}"),
                    format!("JSON parse failed: {e}"),
                )
            })
        })?
    }; // Runtime 和 Context 在这里被 drop

    // 8. 验证返回值格式
    validate_result(&result)?;

    Ok(result)
}

/// 请求配置结构
#[derive(Debug, serde::Deserialize)]
struct RequestConfig {
    url: String,
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// 发送 HTTP 请求
async fn send_http_request(
    config: &RequestConfig,
    base_url: &str,
    is_custom_template: bool,
    timeout_secs: u64,
) -> Result<String, AppError> {
    // 使用独立 HTTP 客户端并禁用自动重定向：每一次 3xx Location 都必须重新经过
    // HTTPS / 同源 / SSRF（含 DNS 解析）校验，避免初始 URL 安全但跳转到内网地址。
    let client = build_usage_http_client()?;
    // 约束超时范围，防止异常配置导致长时间阻塞（最小 2 秒，最大 30 秒）
    let request_timeout = std::time::Duration::from_secs(timeout_secs.clamp(2, 30));

    // 严格校验 HTTP 方法，非法值不回退为 GET
    let method: reqwest::Method = config.method.parse().map_err(|_| {
        AppError::localized(
            "usage_script.invalid_http_method",
            format!("不支持的 HTTP 方法: {}", config.method),
            format!("Unsupported HTTP method: {}", config.method),
        )
    })?;

    let mut current_url = config.url.clone();
    let mut redirects = 0usize;
    let resp = loop {
        validate_request_url(&current_url, base_url, is_custom_template)?;
        if !is_custom_template {
            validate_url_resolved_addrs(&current_url, "request").await?;
        }

        let mut req = client
            .request(method.clone(), &current_url)
            .timeout(request_timeout);

        // 添加请求头
        for (k, v) in &config.headers {
            req = req.header(k, v);
        }

        // 307/308 保留方法语义；这里为避免 body 被重复消费，直接 clone 模板 body。
        if let Some(body) = &config.body {
            req = req.body(body.clone());
        }

        let resp = req.send().await.map_err(|e| {
            AppError::localized(
                "usage_script.request_failed",
                format!("请求失败: {e}"),
                format!("Request failed: {e}"),
            )
        })?;

        if !resp.status().is_redirection() {
            break resp;
        }
        redirects += 1;
        if redirects > 10 {
            return Err(AppError::localized(
                "usage_script.too_many_redirects",
                "请求重定向次数过多",
                "Too many redirects",
            ));
        }
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                AppError::localized(
                    "usage_script.redirect_missing_location",
                    "重定向响应缺少 Location 头",
                    "Redirect response is missing Location header",
                )
            })?;
        let base = Url::parse(&current_url).map_err(|e| {
            AppError::localized(
                "usage_script.request_url_invalid",
                format!("无效的请求 URL: {e}"),
                format!("Invalid request URL: {e}"),
            )
        })?;
        current_url = base
            .join(location)
            .map_err(|e| {
                AppError::localized(
                    "usage_script.redirect_url_invalid",
                    format!("无效的重定向 URL: {e}"),
                    format!("Invalid redirect URL: {e}"),
                )
            })?
            .to_string();
    };

    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        AppError::localized(
            "usage_script.read_response_failed",
            format!("读取响应失败: {e}"),
            format!("Failed to read response: {e}"),
        )
    })?;

    if !status.is_success() {
        let preview = if text.len() > 200 {
            let mut safe_cut = 200usize;
            while !text.is_char_boundary(safe_cut) {
                safe_cut = safe_cut.saturating_sub(1);
            }
            format!("{}...", &text[..safe_cut])
        } else {
            text.clone()
        };
        return Err(AppError::localized(
            "usage_script.http_error",
            format!("HTTP {status} : {preview}"),
            format!("HTTP {status} : {preview}"),
        ));
    }

    Ok(text)
}

fn build_usage_http_client() -> Result<reqwest::Client, AppError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(30));

    if let Some(proxy_url) = crate::proxy::http_client::get_current_proxy_url() {
        let proxy = reqwest::Proxy::all(&proxy_url).map_err(|e| {
            AppError::localized(
                "usage_script.proxy_invalid",
                format!("代理配置无效: {e}"),
                format!("Invalid proxy configuration: {e}"),
            )
        })?;
        builder = builder.proxy(proxy);
    } else if crate::proxy::http_client::system_proxy_points_to_loopback() {
        builder = builder.no_proxy();
    }

    builder.build().map_err(|e| {
        AppError::localized(
            "usage_script.client_build_failed",
            format!("创建 HTTP 客户端失败: {e}"),
            format!("Failed to build HTTP client: {e}"),
        )
    })
}

/// 验证脚本返回值（支持单对象或数组）
fn validate_result(result: &Value) -> Result<(), AppError> {
    // 如果是数组，验证每个元素
    if let Some(arr) = result.as_array() {
        if arr.is_empty() {
            return Err(AppError::localized(
                "usage_script.empty_array",
                "脚本返回的数组不能为空",
                "Script returned empty array",
            ));
        }
        for (idx, item) in arr.iter().enumerate() {
            validate_single_usage(item).map_err(|e| {
                AppError::localized(
                    "usage_script.array_validation_failed",
                    format!("数组索引[{idx}]验证失败: {e}"),
                    format!("Validation failed at index [{idx}]: {e}"),
                )
            })?;
        }
        return Ok(());
    }

    // 如果是单对象，直接验证（向后兼容）
    validate_single_usage(result)
}

/// 验证单个用量数据对象
fn validate_single_usage(result: &Value) -> Result<(), AppError> {
    let obj = result.as_object().ok_or_else(|| {
        AppError::localized(
            "usage_script.must_return_object",
            "脚本必须返回对象或对象数组",
            "Script must return object or array of objects",
        )
    })?;

    // 所有字段均为可选，只进行类型检查
    if obj.contains_key("isValid")
        && !result["isValid"].is_null()
        && !result["isValid"].is_boolean()
    {
        return Err(AppError::localized(
            "usage_script.isvalid_type_error",
            "isValid 必须是布尔值或 null",
            "isValid must be boolean or null",
        ));
    }
    if obj.contains_key("invalidMessage")
        && !result["invalidMessage"].is_null()
        && !result["invalidMessage"].is_string()
    {
        return Err(AppError::localized(
            "usage_script.invalidmessage_type_error",
            "invalidMessage 必须是字符串或 null",
            "invalidMessage must be string or null",
        ));
    }
    if obj.contains_key("remaining")
        && !result["remaining"].is_null()
        && !result["remaining"].is_number()
    {
        return Err(AppError::localized(
            "usage_script.remaining_type_error",
            "remaining 必须是数字或 null",
            "remaining must be number or null",
        ));
    }
    if obj.contains_key("unit") && !result["unit"].is_null() && !result["unit"].is_string() {
        return Err(AppError::localized(
            "usage_script.unit_type_error",
            "unit 必须是字符串或 null",
            "unit must be string or null",
        ));
    }
    if obj.contains_key("total") && !result["total"].is_null() && !result["total"].is_number() {
        return Err(AppError::localized(
            "usage_script.total_type_error",
            "total 必须是数字或 null",
            "total must be number or null",
        ));
    }
    if obj.contains_key("used") && !result["used"].is_null() && !result["used"].is_number() {
        return Err(AppError::localized(
            "usage_script.used_type_error",
            "used 必须是数字或 null",
            "used must be number or null",
        ));
    }
    if obj.contains_key("planName")
        && !result["planName"].is_null()
        && !result["planName"].is_string()
    {
        return Err(AppError::localized(
            "usage_script.planname_type_error",
            "planName 必须是字符串或 null",
            "planName must be string or null",
        ));
    }
    if obj.contains_key("extra") && !result["extra"].is_null() && !result["extra"].is_string() {
        return Err(AppError::localized(
            "usage_script.extra_type_error",
            "extra 必须是字符串或 null",
            "extra must be string or null",
        ));
    }

    Ok(())
}

/// 构建替换变量后的脚本，保持与旧版脚本的兼容性
fn build_script_with_vars(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    access_token: Option<&str>,
    user_id: Option<&str>,
) -> String {
    let mut replaced = script_code
        .replace("{{apiKey}}", api_key)
        .replace("{{baseUrl}}", base_url);

    if let Some(token) = access_token {
        replaced = replaced.replace("{{accessToken}}", token);
    }
    if let Some(uid) = user_id {
        replaced = replaced.replace("{{userId}}", uid);
    }

    replaced
}

/// 验证 base_url 的基本安全性
fn validate_base_url(base_url: &str) -> Result<(), AppError> {
    if base_url.is_empty() {
        return Err(AppError::localized(
            "usage_script.base_url_empty",
            "base_url 不能为空",
            "base_url cannot be empty",
        ));
    }

    // 解析 URL
    let parsed_url = Url::parse(base_url).map_err(|e| {
        AppError::localized(
            "usage_script.base_url_invalid",
            format!("无效的 base_url: {e}"),
            format!("Invalid base_url: {e}"),
        )
    })?;

    let is_loopback = is_loopback_host(&parsed_url);

    // 必须是 HTTPS（允许 localhost 用于开发）
    if parsed_url.scheme() != "https" && !is_loopback {
        return Err(AppError::localized(
            "usage_script.base_url_https_required",
            "base_url 必须使用 HTTPS 协议（localhost 除外）",
            "base_url must use HTTPS (localhost allowed)",
        ));
    }

    // 检查主机名格式有效性
    let hostname = parsed_url.host_str().ok_or_else(|| {
        AppError::localized(
            "usage_script.base_url_hostname_missing",
            "base_url 必须包含有效的主机名",
            "base_url must include a valid hostname",
        )
    })?;

    // 基本的主机名格式检查
    if hostname.is_empty() {
        return Err(AppError::localized(
            "usage_script.base_url_hostname_empty",
            "base_url 主机名不能为空",
            "base_url hostname cannot be empty",
        ));
    }

    // SSRF 防护：禁止 base_url 指向私网 / 链路本地 / ULA 地址（loopback 已在上方按 HTTPS 例外保留）
    // 自定义模板模式下用户已自担风险（调用方控制 should_validate_base_url），不会走到这里。
    if !is_loopback && is_private_or_link_local_host(hostname) {
        return Err(AppError::localized(
            "usage_script.base_url_private_host",
            format!("base_url 主机 {hostname} 指向私网/链路本地地址，已拒绝以防 SSRF"),
            format!(
                "base_url host {hostname} points to a private/link-local address; rejected to prevent SSRF"
            ),
        ));
    }

    Ok(())
}

fn should_validate_base_url(base_url: &str, is_custom_template: bool) -> bool {
    !base_url.is_empty() && !is_custom_template
}

/// DNS 层 SSRF 防护：域名解析后的每个地址也必须是公网地址。
///
/// 仅在非自定义模板路径调用；localhost/loopback 继续作为本地调试例外放行。
async fn validate_url_resolved_addrs(url: &str, label: &str) -> Result<(), AppError> {
    let parsed = Url::parse(url).map_err(|e| {
        AppError::localized(
            "usage_script.url_invalid",
            format!("无效的 {label} URL: {e}"),
            format!("Invalid {label} URL: {e}"),
        )
    })?;
    if is_loopback_host(&parsed) {
        return Ok(());
    }

    let host = parsed.host_str().ok_or_else(|| {
        AppError::localized(
            "usage_script.hostname_missing",
            format!("{label} URL 必须包含有效的主机名"),
            format!("{label} URL must include a valid hostname"),
        )
    })?;
    if host.parse::<IpAddr>().is_ok() {
        if is_private_or_link_local_host(host) {
            return Err(private_resolved_host_error(label, host, host));
        }
        return Ok(());
    }

    let port = parsed.port_or_known_default().ok_or_else(|| {
        AppError::localized(
            "usage_script.port_unknown",
            format!("无法确定 {label} URL 的端口号"),
            format!("Unable to determine port for {label} URL"),
        )
    })?;
    let addrs = tokio::net::lookup_host((host, port)).await.map_err(|e| {
        AppError::localized(
            "usage_script.dns_lookup_failed",
            format!("解析 {label} 主机 {host} 失败: {e}"),
            format!("Failed to resolve {label} host {host}: {e}"),
        )
    })?;

    for addr in addrs {
        let ip = addr.ip();
        if is_private_or_link_local_ip(ip) {
            return Err(private_resolved_host_error(label, host, &ip.to_string()));
        }
    }
    Ok(())
}

fn private_resolved_host_error(label: &str, host: &str, ip: &str) -> AppError {
    AppError::localized(
        "usage_script.resolved_private_host",
        format!("{label} 主机 {host} 解析到私网/链路本地地址 {ip}，已拒绝以防 SSRF"),
        format!(
            "{label} host {host} resolves to private/link-local address {ip}; rejected to prevent SSRF"
        ),
    )
}

/// 验证请求 URL 是否安全（HTTPS 强制 + 同源检查）
fn validate_request_url(
    request_url: &str,
    base_url: &str,
    is_custom_template: bool,
) -> Result<(), AppError> {
    // 解析请求 URL
    let parsed_request = Url::parse(request_url).map_err(|e| {
        AppError::localized(
            "usage_script.request_url_invalid",
            format!("无效的请求 URL: {e}"),
            format!("Invalid request URL: {e}"),
        )
    })?;

    let is_request_loopback = is_loopback_host(&parsed_request);

    // 必须使用 HTTPS（允许 localhost 用于开发）
    // 自定义模板模式下，允许用户自行决定是否使用 HTTP（用户需自行承担安全风险）
    if !is_custom_template && parsed_request.scheme() != "https" && !is_request_loopback {
        return Err(AppError::localized(
            "usage_script.request_https_required",
            "请求 URL 必须使用 HTTPS 协议（localhost 除外）",
            "Request URL must use HTTPS (localhost allowed)",
        ));
    }

    // SSRF 防护：非自定义模板下，禁止请求 URL 指向私网 / 链路本地 / ULA 地址。
    // loopback 允许（与 HTTPS 例外语义保持一致，便于本地调试）；
    // 自定义模板模式保留逃生口由用户自担风险。
    if !is_custom_template && !is_request_loopback {
        if let Some(host) = parsed_request.host_str() {
            if is_private_or_link_local_host(host) {
                return Err(AppError::localized(
                    "usage_script.request_private_host",
                    format!("请求主机 {host} 指向私网/链路本地地址，已拒绝以防 SSRF"),
                    format!(
                        "Request host {host} points to a private/link-local address; rejected to prevent SSRF"
                    ),
                ));
            }
        }
    }

    // 如果提供了 base_url（非空），则进行同源检查
    // 🔧 自定义模板模式下，用户可以自由访问任意 HTTPS 域名，跳过同源检查
    if !base_url.is_empty() && !is_custom_template {
        // 解析 base URL
        let parsed_base = Url::parse(base_url).map_err(|e| {
            AppError::localized(
                "usage_script.base_url_invalid",
                format!("无效的 base_url: {e}"),
                format!("Invalid base_url: {e}"),
            )
        })?;

        // 核心安全检查：必须与 base_url 同源（相同域名和端口）
        if parsed_request.host_str() != parsed_base.host_str() {
            return Err(AppError::localized(
                "usage_script.request_host_mismatch",
                format!(
                    "请求域名 {} 与 base_url 域名 {} 不匹配（必须是同源请求）",
                    parsed_request.host_str().unwrap_or("unknown"),
                    parsed_base.host_str().unwrap_or("unknown")
                ),
                format!(
                    "Request host {} must match base_url host {} (same-origin required)",
                    parsed_request.host_str().unwrap_or("unknown"),
                    parsed_base.host_str().unwrap_or("unknown")
                ),
            ));
        }

        // 检查端口是否匹配（考虑默认端口）
        // 使用 port_or_known_default() 会自动处理默认端口（http->80, https->443）
        match (
            parsed_request.port_or_known_default(),
            parsed_base.port_or_known_default(),
        ) {
            (Some(request_port), Some(base_port)) if request_port == base_port => {
                // 端口匹配，继续执行
            }
            (Some(request_port), Some(base_port)) => {
                return Err(AppError::localized(
                    "usage_script.request_port_mismatch",
                    format!("请求端口 {request_port} 必须与 base_url 端口 {base_port} 匹配"),
                    format!("Request port {request_port} must match base_url port {base_port}"),
                ));
            }
            _ => {
                // 理论上不会发生，因为 port_or_known_default() 应该总是返回 Some
                return Err(AppError::localized(
                    "usage_script.request_port_unknown",
                    "无法确定端口号",
                    "Unable to determine port number",
                ));
            }
        }
    }

    Ok(())
}

/// 判断 URL 是否指向本机（localhost / loopback）
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

/// 判断 host 是否为私网 / 链路本地 / 回环 / 未指定地址（用于 SSRF 防护）
///
/// 覆盖 RFC1918（10/8、172.16/12、192.168/16）、169.254/16 链路本地、
/// IPv6 ULA（fc00::/7）、IPv6 link-local（fe80::/10）、loopback 与 unspecified。
/// 不解析域名 — 只识别字面 IP；域名走 DNS 那一层不在这里兜底。
fn is_private_or_link_local_host(host: &str) -> bool {
    // 去掉 IPv6 字面量的 [ ]
    let s = host.trim_start_matches('[').trim_end_matches(']');
    s.parse::<IpAddr>()
        .map(is_private_or_link_local_ip)
        .unwrap_or(false)
}

fn is_private_or_link_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()       // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local() // 169.254/16
                || v4.is_loopback()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // fc00::/7 ULA
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // fe80::/10 link-local
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_https_bypass_prevention() {
        // 非本地域名的 HTTP 应该被拒绝
        let result = validate_base_url("http://127.0.0.1.evil.com/api");
        assert!(
            result.is_err(),
            "Should reject HTTP for non-localhost domains"
        );
    }

    #[test]
    fn test_custom_template_allows_http_lan_request_with_different_base_url() {
        assert!(
            !should_validate_base_url("http://10.37.192.156:8090/anthropic", true),
            "Custom scripts should not validate an unused provider base_url fallback"
        );

        let result = validate_request_url(
            "http://10.37.192.156:18344/user/balance",
            "http://10.37.192.156:8090/anthropic",
            true,
        );
        assert!(
            result.is_ok(),
            "Custom usage scripts should be able to call an explicit HTTP quota endpoint"
        );
    }

    #[test]
    fn test_port_comparison() {
        // 测试端口比较逻辑是否正确处理默认端口和显式端口

        // 测试用例：(base_url, request_url, should_match)
        let test_cases = vec![
            // HTTPS默认端口测试
            (
                "https://api.example.com",
                "https://api.example.com/v1/test",
                true,
            ),
            (
                "https://api.example.com",
                "https://api.example.com:443/v1/test",
                true,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com/v1/test",
                true,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com:443/v1/test",
                true,
            ),
            // 端口不匹配测试
            (
                "https://api.example.com",
                "https://api.example.com:8443/v1/test",
                false,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com:8443/v1/test",
                false,
            ),
        ];

        for (base_url, request_url, should_match) in test_cases {
            let result = validate_request_url(request_url, base_url, false);

            if should_match {
                assert!(
                    result.is_ok(),
                    "应该匹配的URL被拒绝: base_url={}, request_url={}, error={}",
                    base_url,
                    request_url,
                    result.unwrap_err()
                );
            } else {
                assert!(
                    result.is_err(),
                    "应该不匹配的URL被允许: base_url={}, request_url={}",
                    base_url,
                    request_url
                );
            }
        }
    }

    #[test]
    fn test_sub2api_rejects_private_v4_host() {
        // 模拟 sub2api 非 custom 模板探测内网 host
        // base_url 留空以避开同源检查、隔离出私网拒绝这一条规则
        let result = validate_request_url("https://10.37.192.156/v1/usage", "", false);
        assert!(
            result.is_err(),
            "RFC1918 IPv4 私网地址在非 custom 模板下必须被拒绝"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("10.37.192.156"),
            "错误消息应包含被拒绝的 host，实际: {msg}"
        );
    }

    #[test]
    fn test_sub2api_rejects_private_v6_ula_host() {
        // IPv6 ULA fc00::/7 必须被拒绝
        let result = validate_request_url("https://[fc00::1]/v1/usage", "", false);
        assert!(result.is_err(), "IPv6 ULA 地址在非 custom 模板下必须被拒绝");
    }

    #[test]
    fn test_sub2api_rejects_link_local_v4() {
        // 169.254.169.254 是典型的云元数据地址（AWS/GCP/Azure 都用 169.254/16）
        let result = validate_request_url("https://169.254.169.254/latest/meta-data/", "", false);
        assert!(result.is_err(), "IPv4 链路本地（含云元数据）必须被拒绝");
    }

    #[test]
    fn test_sub2api_rejects_link_local_v6() {
        // IPv6 fe80::/10 link-local
        let result = validate_request_url("https://[fe80::1]/v1/usage", "", false);
        assert!(
            result.is_err(),
            "IPv6 链路本地地址在非 custom 模板下必须被拒绝"
        );
    }

    #[test]
    fn test_custom_template_still_allows_private_host() {
        // is_custom_template=true 保留逃生口：用户自定义脚本可以打内网
        let result = validate_request_url("http://10.37.192.156:18344/user/balance", "", true);
        assert!(
            result.is_ok(),
            "自定义模板模式应保留逃生口，可访问私网 host：{:?}",
            result.err().map(|e| e.to_string())
        );

        // IPv6 ULA 在 custom 模板下也应放行
        let result_v6 = validate_request_url("http://[fc00::1]/api/usage", "", true);
        assert!(
            result_v6.is_ok(),
            "自定义模板模式下 IPv6 ULA 应放行：{:?}",
            result_v6.err().map(|e| e.to_string())
        );
    }

    #[test]
    fn test_base_url_rejects_private_v4() {
        // validate_base_url 同样应该拒绝 https 私网地址
        let result = validate_base_url("https://10.0.0.1/api");
        assert!(
            result.is_err(),
            "validate_base_url 应拒绝 https 指向私网的 base_url"
        );
    }

    #[test]
    fn test_base_url_still_allows_public_host() {
        // 公网 HTTPS 不应受影响
        let result = validate_base_url("https://api.anthropic.com/v1");
        assert!(
            result.is_ok(),
            "公网 HTTPS base_url 不应被新增规则误伤: {:?}",
            result.err().map(|e| e.to_string())
        );
    }

    #[test]
    fn test_loopback_still_allowed() {
        // 127.0.0.1 与 localhost 仍允许（保留本地开发场景）
        let res_ipv4 = validate_request_url("http://127.0.0.1:8080/usage", "", false);
        assert!(
            res_ipv4.is_ok(),
            "127.0.0.1 应继续允许: {:?}",
            res_ipv4.err().map(|e| e.to_string())
        );

        let res_localhost = validate_request_url("http://localhost:8080/usage", "", false);
        assert!(
            res_localhost.is_ok(),
            "localhost 应继续允许: {:?}",
            res_localhost.err().map(|e| e.to_string())
        );
    }
}
