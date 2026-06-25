//! OpenCode 配置文件读写模块
//!
//! 处理 `~/.config/opencode/opencode.json` 的读写。该文件实际是 JSONC/JSON5
//! （读取端用 json5 解析，用户文件可能带注释），因此写入端不能整文件重新
//! 序列化，否则注释会被销毁、键序被打乱。
//!
//! 写入策略：
//! 1. 进程内写锁覆盖所有读-改-写入口，防止并发写互踩；
//! 2. 内容实际变化时，写盘前先把原文件备份到配置目录下的 `backups/`；
//! 3. 用 json-five 的 round-trip AST 只替换被编辑的顶层子树
//!    （provider/mcp/plugin），其余内容（含注释与键序）字节级原样保留。
//!
//! 已知限制：被替换的顶层子树内部的注释会丢失（子树按 serde_json pretty
//! 重新生成，双引号键、2 空格缩进）。

use crate::config::atomic_write;
use crate::error::AppError;
use crate::provider::OpenCodeProviderConfig;
use crate::settings::{effective_backup_retain_count, get_opencode_override_dir};
use chrono::Local;
use indexmap::IndexMap;
use json_five::rt::parser::{
    from_str as rt_from_str, JSONKeyValuePair as RtJSONKeyValuePair,
    JSONObjectContext as RtJSONObjectContext, JSONText as RtJSONText, JSONValue as RtJSONValue,
    KeyValuePairContext as RtKeyValuePairContext,
};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const STANDARD_OMO_PLUGIN_PREFIXES: [&str; 2] = ["oh-my-openagent", "oh-my-opencode"];
const SLIM_OMO_PLUGIN_PREFIXES: [&str; 1] = ["oh-my-opencode-slim"];

/// 文件不存在时的初始文档（保持 opencode.json 的严格 JSON 风格）
const OPENCODE_DEFAULT_SOURCE: &str = "{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n";

/// opencode.json 进程内写锁：所有读-改-写入口必须先持锁
/// （`OnceLock<Mutex<()>>` 模式）。
fn opencode_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn matches_plugin_prefix(plugin_name: &str, prefix: &str) -> bool {
    plugin_name == prefix
        || plugin_name
            .strip_prefix(prefix)
            .map(|suffix| suffix.starts_with('@'))
            .unwrap_or(false)
}

fn matches_any_plugin_prefix(plugin_name: &str, prefixes: &[&str]) -> bool {
    prefixes
        .iter()
        .any(|prefix| matches_plugin_prefix(plugin_name, prefix))
}

fn canonicalize_plugin_name(plugin_name: &str) -> String {
    if let Some(suffix) = plugin_name.strip_prefix("oh-my-opencode") {
        if suffix.is_empty() || suffix.starts_with('@') {
            return format!("oh-my-openagent{suffix}");
        }
    }
    plugin_name.to_string()
}

pub fn get_opencode_dir() -> PathBuf {
    if let Some(override_dir) = get_opencode_override_dir() {
        return override_dir;
    }

    crate::config::get_home_dir()
        .join(".config")
        .join("opencode")
}

pub fn get_opencode_config_path() -> PathBuf {
    get_opencode_dir().join("opencode.json")
}

/// 获取 opencode.json 写前备份目录（位于 OpenCode 配置目录下）
fn get_opencode_backup_dir() -> PathBuf {
    get_opencode_dir().join("backups")
}

/// 获取 OpenCode SQLite 数据库路径
/// 优先级: OPENCODE_DB 环境变量 > XDG_DATA_HOME > ~/.local/share/opencode
pub fn get_opencode_db_path() -> PathBuf {
    // 支持 OPENCODE_DB 环境变量覆盖（忽略空字符串）
    if let Ok(custom_path) = std::env::var("OPENCODE_DB") {
        if !custom_path.is_empty() {
            let path = PathBuf::from(&custom_path);
            if path.is_absolute() {
                return path;
            }
            // 相对路径基于数据目录
            return get_opencode_data_dir().join(path);
        }
    }

    get_opencode_data_dir().join("opencode.db")
}

fn get_opencode_data_dir() -> PathBuf {
    // 尊重 XDG_DATA_HOME（按 XDG 规范，空字符串视为未设置）
    if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME") {
        if !xdg_data.is_empty() {
            return PathBuf::from(xdg_data).join("opencode");
        }
    }

    // OpenCode 使用 xdg-basedir，不遵守 macOS/Windows 平台约定，
    // 所有平台默认都落在 ~/.local/share/opencode
    crate::config::get_home_dir()
        .join(".local")
        .join("share")
        .join("opencode")
}

#[allow(dead_code)]
pub fn get_opencode_env_path() -> PathBuf {
    get_opencode_dir().join(".env")
}

pub fn read_opencode_config() -> Result<Value, AppError> {
    let path = get_opencode_config_path();

    if !path.exists() {
        return Ok(json!({
            "$schema": "https://opencode.ai/config.json"
        }));
    }

    let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    json5::from_str(&content).map_err(|e| {
        AppError::Config(format!(
            "Failed to parse OpenCode config: {}: {e}",
            path.display()
        ))
    })
}

/// 整文件覆盖写入 opencode.json（公开入口，自动加写锁）
///
/// 已知限制：整文件序列化无法保留注释；键序保留 `config` 中的插入顺序
/// （serde_json 开启 preserve_order），不再按字母排序。
/// 常规修改请使用 set_provider/set_mcp_server/add_plugin 等子树级入口。
#[allow(dead_code)]
pub fn write_opencode_config(config: &Value) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;
    write_full_config_locked(config)
}

/// 整文件写入：非排序 pretty 序列化 + 写前备份 + 原子落盘。
/// 调用方必须已持有 `opencode_write_lock`。
fn write_full_config_locked(config: &Value) -> Result<(), AppError> {
    let path = get_opencode_config_path();
    let current_source = if path.exists() {
        Some(fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?)
    } else {
        None
    };

    let next_source =
        serde_json::to_string_pretty(config).map_err(|e| AppError::JsonSerialize { source: e })?;

    if current_source.as_deref() == Some(next_source.as_str()) {
        return Ok(());
    }

    if let Some(source) = current_source.as_ref() {
        create_opencode_backup(source)?;
    }

    atomic_write(&path, next_source.as_bytes())?;
    log::debug!("OpenCode config written to {path:?}");
    Ok(())
}

/// 修改前备份当前 opencode.json 内容
///
/// 备份模式：时间戳文件名 + 冲突计数后缀 +
/// 按 settings.backup_retain_count（默认 10）清理最旧的备份。
fn create_opencode_backup(source: &str) -> Result<PathBuf, AppError> {
    let backup_dir = get_opencode_backup_dir();
    fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

    let base_id = format!("opencode_{}", Local::now().format("%Y%m%d_%H%M%S"));
    let mut filename = format!("{base_id}.json");
    let mut backup_path = backup_dir.join(&filename);
    let mut counter = 1;

    while backup_path.exists() {
        filename = format!("{base_id}_{counter}.json");
        backup_path = backup_dir.join(&filename);
        counter += 1;
    }

    atomic_write(&backup_path, source.as_bytes())?;
    cleanup_opencode_backups(&backup_dir)?;
    Ok(backup_path)
}

fn cleanup_opencode_backups(dir: &Path) -> Result<(), AppError> {
    let retain = effective_backup_retain_count();
    let mut entries = fs::read_dir(dir)
        .map_err(|e| AppError::io(dir, e))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    if entries.len() <= retain {
        return Ok(());
    }

    entries.sort_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok());
    let remove_count = entries.len().saturating_sub(retain);
    for entry in entries.into_iter().take(remove_count) {
        if let Err(err) = fs::remove_file(entry.path()) {
            log::warn!(
                "Failed to remove old OpenCode config backup {}: {err}",
                entry.path().display()
            );
        }
    }

    Ok(())
}

// ============================================================================
// Round-trip 文档编辑（保留注释与键序）
// ============================================================================

struct OpenCodeConfigDocument {
    path: PathBuf,
    original_source: Option<String>,
    text: RtJSONText,
}

impl OpenCodeConfigDocument {
    /// 加载 round-trip 文档。
    ///
    /// 返回 `Ok(None)` 表示 json-five 无法解析该文件（即便 json5 crate 可以），
    /// 调用方应回退到整文件重写路径。
    fn load() -> Result<Option<Self>, AppError> {
        let path = get_opencode_config_path();
        let original_source = if path.exists() {
            Some(fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?)
        } else {
            None
        };

        let source = original_source
            .clone()
            .unwrap_or_else(|| OPENCODE_DEFAULT_SOURCE.to_string());

        match rt_from_str(&source) {
            Ok(text) => Ok(Some(Self {
                path,
                original_source,
                text,
            })),
            Err(e) => {
                log::warn!(
                    "OpenCode config round-trip parse failed ({}); falling back to full rewrite (comments will be lost): {}",
                    path.display(),
                    e.message
                );
                Ok(None)
            }
        }
    }

    /// 替换（或追加）单个顶层键，其余内容原样保留
    fn set_root_section(&mut self, key: &str, value: &Value) -> Result<(), AppError> {
        let RtJSONValue::JSONObject {
            key_value_pairs,
            context,
        } = &mut self.text.value
        else {
            return Err(AppError::Config(
                "OpenCode config root must be a JSON object".to_string(),
            ));
        };

        if key_value_pairs.is_empty()
            && context
                .as_ref()
                .map(|ctx| ctx.wsc.0.is_empty())
                .unwrap_or(true)
        {
            *context = Some(RtJSONObjectContext {
                wsc: ("\n  ".to_string(),),
            });
        }

        let leading_ws = context
            .as_ref()
            .map(|ctx| ctx.wsc.0.clone())
            .unwrap_or_default();
        let entry_separator_ws = derive_entry_separator(&leading_ws);
        let child_indent = extract_trailing_indent(&leading_ws);
        let new_value = value_to_rt_value(value, &child_indent)?;

        if let Some(existing) = key_value_pairs
            .iter_mut()
            .find(|pair| json5_key_name(&pair.key) == Some(key))
        {
            existing.value = new_value;
            return Ok(());
        }

        let new_pair = if let Some(last_pair) = key_value_pairs.last_mut() {
            let last_ctx = ensure_kvp_context(last_pair);
            let closing_ws = if let Some(after_comma) = last_ctx.wsc.3.clone() {
                last_ctx.wsc.3 = Some(entry_separator_ws.clone());
                after_comma
            } else {
                let closing_ws = std::mem::take(&mut last_ctx.wsc.2);
                last_ctx.wsc.3 = Some(entry_separator_ws.clone());
                closing_ws
            };

            make_root_pair(key, new_value, closing_ws)
        } else {
            make_root_pair(
                key,
                new_value,
                derive_closing_ws_from_separator(&leading_ws),
            )
        };

        key_value_pairs.push(new_pair);
        Ok(())
    }

    /// 删除单个顶层键，返回是否真的删除了
    fn remove_root_section(&mut self, key: &str) -> Result<bool, AppError> {
        let RtJSONValue::JSONObject {
            key_value_pairs,
            context,
        } = &mut self.text.value
        else {
            return Err(AppError::Config(
                "OpenCode config root must be a JSON object".to_string(),
            ));
        };

        let Some(index) = key_value_pairs
            .iter()
            .position(|pair| json5_key_name(&pair.key) == Some(key))
        else {
            return Ok(false);
        };

        let removed = key_value_pairs.remove(index);

        if key_value_pairs.is_empty() {
            // 对象被删空：收敛为紧凑的 `{}`
            *context = None;
        } else if index == key_value_pairs.len() {
            // 删除的是最后一个键：让新的最后一个键继承被删键的收尾空白并去掉
            // 行尾逗号，避免写出 `value,\n}` 这种尾随逗号（严格 JSON 不允许）。
            let closing_ws = removed
                .context
                .map(|ctx| ctx.wsc.3.unwrap_or(ctx.wsc.2))
                .unwrap_or_default();
            if let Some(last_pair) = key_value_pairs.last_mut() {
                let last_ctx = ensure_kvp_context(last_pair);
                last_ctx.wsc.2 = closing_ws;
                last_ctx.wsc.3 = None;
            }
        }
        // 删除首个/中间键时，前后空白自然衔接，无需调整。

        Ok(true)
    }

    /// 写盘：内容无变化则跳过；写前先备份原内容并自检输出合法性。
    /// 调用方必须已持有 `opencode_write_lock`。
    fn save_locked(self) -> Result<(), AppError> {
        let next_source = self.text.to_string();
        if self.original_source.as_deref() == Some(next_source.as_str()) {
            return Ok(());
        }

        // 写前自检：AST 手术结果必须仍可被解析，避免写出坏文件
        json5::from_str::<Value>(&next_source).map_err(|e| {
            AppError::Config(format!(
                "Generated OpenCode config is invalid, write aborted: {}: {e}",
                self.path.display()
            ))
        })?;

        if let Some(source) = self.original_source.as_ref() {
            create_opencode_backup(source)?;
        }

        atomic_write(&self.path, next_source.as_bytes())?;
        log::debug!("OpenCode config written to {:?}", self.path);
        Ok(())
    }
}

/// 用 round-trip 文档替换单个顶层子树；json-five 不可用时回退整文件重写
/// （注释丢失为已知限制，键序仍保留）。调用方必须已持有 `opencode_write_lock`。
fn write_root_section_locked(key: &str, value: &Value) -> Result<(), AppError> {
    if let Some(mut document) = OpenCodeConfigDocument::load()? {
        document.set_root_section(key, value)?;
        return document.save_locked();
    }

    let mut config = read_opencode_config()?;
    let Some(obj) = config.as_object_mut() else {
        return Err(AppError::Config(
            "OpenCode config root must be a JSON object".to_string(),
        ));
    };
    obj.insert(key.to_string(), value.clone());
    write_full_config_locked(&config)
}

/// 删除单个顶层键（round-trip 优先，回退整文件重写）。
/// 调用方必须已持有 `opencode_write_lock`。
fn remove_root_section_locked(key: &str) -> Result<(), AppError> {
    if let Some(mut document) = OpenCodeConfigDocument::load()? {
        if document.remove_root_section(key)? {
            return document.save_locked();
        }
        return Ok(());
    }

    let mut config = read_opencode_config()?;
    let Some(obj) = config.as_object_mut() else {
        return Err(AppError::Config(
            "OpenCode config root must be a JSON object".to_string(),
        ));
    };
    if obj.remove(key).is_none() {
        return Ok(());
    }
    write_full_config_locked(&config)
}

// ---- 以下 round-trip AST helper ----

fn ensure_kvp_context(pair: &mut RtJSONKeyValuePair) -> &mut RtKeyValuePairContext {
    pair.context.get_or_insert_with(|| RtKeyValuePairContext {
        wsc: (String::new(), " ".to_string(), String::new(), None),
    })
}

fn extract_trailing_indent(separator_ws: &str) -> String {
    separator_ws
        .rsplit_once('\n')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_default()
}

fn derive_closing_ws_from_separator(separator_ws: &str) -> String {
    let Some((prefix, indent)) = separator_ws.rsplit_once('\n') else {
        return String::new();
    };

    let reduced_indent = if indent.ends_with('\t') {
        &indent[..indent.len().saturating_sub(1)]
    } else if indent.ends_with("  ") {
        &indent[..indent.len().saturating_sub(2)]
    } else if indent.ends_with(' ') {
        &indent[..indent.len().saturating_sub(1)]
    } else {
        indent
    };

    format!("{prefix}\n{reduced_indent}")
}

fn derive_entry_separator(leading_ws: &str) -> String {
    if leading_ws.is_empty() {
        return String::new();
    }

    if leading_ws.contains('\n') {
        return format!("\n{}", extract_trailing_indent(leading_ws));
    }

    String::new()
}

fn value_to_rt_value(value: &Value, parent_indent: &str) -> Result<RtJSONValue, AppError> {
    // `json-five` 0.3.1 can panic when pretty-printing nested empty maps/arrays.
    // Serialize with `serde_json` instead; the resulting JSON is valid JSON5 and
    // can still be parsed back into the round-trip AST we use for insertion.
    let source = serde_json::to_string_pretty(value)
        .map_err(|e| AppError::Config(format!("Failed to serialize JSON section: {e}")))?;

    let adjusted = reindent_json5_block(&source, parent_indent);
    let text = rt_from_str(&adjusted).map_err(|e| {
        AppError::Config(format!(
            "Failed to parse generated JSON5 section: {}",
            e.message
        ))
    })?;
    Ok(text.value)
}

fn reindent_json5_block(source: &str, parent_indent: &str) -> String {
    if parent_indent.is_empty() || !source.contains('\n') {
        return source.to_string();
    }

    let mut lines = source.lines();
    let Some(first_line) = lines.next() else {
        return String::new();
    };

    let mut result = String::from(first_line);
    for line in lines {
        result.push('\n');
        result.push_str(parent_indent);
        result.push_str(line);
    }
    result
}

fn make_root_pair(key: &str, value: RtJSONValue, closing_ws: String) -> RtJSONKeyValuePair {
    RtJSONKeyValuePair {
        key: make_json_key(key),
        value,
        context: Some(RtKeyValuePairContext {
            wsc: (String::new(), " ".to_string(), closing_ws, None),
        }),
    }
}

fn make_json_key(key: &str) -> RtJSONValue {
    // opencode.json 是严格 JSON/JSONC，新增键一律使用双引号字符串
    RtJSONValue::DoubleQuotedString(key.to_string())
}

fn json5_key_name(key: &RtJSONValue) -> Option<&str> {
    match key {
        RtJSONValue::Identifier(name)
        | RtJSONValue::DoubleQuotedString(name)
        | RtJSONValue::SingleQuotedString(name) => Some(name),
        _ => None,
    }
}

// ============================================================================
// Provider / MCP / Plugin 读写入口
// ============================================================================

pub fn get_providers() -> Result<Map<String, Value>, AppError> {
    let config = read_opencode_config()?;
    Ok(config
        .get("provider")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default())
}

pub fn set_provider(id: &str, config: Value) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let full_config = read_opencode_config()?;
    let mut providers = full_config
        .get("provider")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if providers.get(id) == Some(&config) {
        // 内容未变：跳过写盘，避免无谓地重写 provider 子树
        return Ok(());
    }

    providers.insert(id.to_string(), config);
    write_root_section_locked("provider", &Value::Object(providers))
}

pub fn remove_provider(id: &str) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let config = read_opencode_config()?;
    let Some(providers) = config.get("provider").and_then(Value::as_object) else {
        return Ok(());
    };
    if !providers.contains_key(id) {
        return Ok(());
    }

    let mut providers = providers.clone();
    providers.remove(id);
    write_root_section_locked("provider", &Value::Object(providers))
}

pub fn get_typed_providers() -> Result<IndexMap<String, OpenCodeProviderConfig>, AppError> {
    let providers = get_providers()?;
    let mut result = IndexMap::new();

    for (id, value) in providers {
        match serde_json::from_value::<OpenCodeProviderConfig>(value.clone()) {
            Ok(config) => {
                result.insert(id, config);
            }
            Err(e) => {
                log::warn!("Failed to parse provider '{id}': {e}");
            }
        }
    }

    Ok(result)
}

pub fn set_typed_provider(id: &str, config: &OpenCodeProviderConfig) -> Result<(), AppError> {
    let value = serde_json::to_value(config).map_err(|e| AppError::JsonSerialize { source: e })?;
    set_provider(id, value)
}

pub fn get_mcp_servers() -> Result<Map<String, Value>, AppError> {
    let config = read_opencode_config()?;
    Ok(config
        .get("mcp")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default())
}

pub fn set_mcp_server(id: &str, config: Value) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let full_config = read_opencode_config()?;
    let mut mcp = full_config
        .get("mcp")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if mcp.get(id) == Some(&config) {
        return Ok(());
    }

    mcp.insert(id.to_string(), config);
    write_root_section_locked("mcp", &Value::Object(mcp))
}

pub fn remove_mcp_server(id: &str) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let config = read_opencode_config()?;
    let Some(mcp) = config.get("mcp").and_then(Value::as_object) else {
        return Ok(());
    };
    if !mcp.contains_key(id) {
        return Ok(());
    }

    let mut mcp = mcp.clone();
    mcp.remove(id);
    write_root_section_locked("mcp", &Value::Object(mcp))
}

pub fn add_plugin(plugin_name: &str) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let config = read_opencode_config()?;
    let normalized_plugin_name = canonicalize_plugin_name(plugin_name);

    let original = config.get("plugin").and_then(Value::as_array).cloned();
    let mut plugins = original.clone().unwrap_or_default();

    // Mutual exclusion: standard OMO and OMO Slim cannot coexist as plugins
    if matches_any_plugin_prefix(&normalized_plugin_name, &STANDARD_OMO_PLUGIN_PREFIXES)
        || matches_any_plugin_prefix(&normalized_plugin_name, &SLIM_OMO_PLUGIN_PREFIXES)
    {
        plugins.retain(|v| {
            v.as_str()
                .map(|s| {
                    !matches_any_plugin_prefix(s, &STANDARD_OMO_PLUGIN_PREFIXES)
                        && !matches_any_plugin_prefix(s, &SLIM_OMO_PLUGIN_PREFIXES)
                })
                .unwrap_or(true)
        });
    }

    let already_exists = plugins
        .iter()
        .any(|v| v.as_str() == Some(normalized_plugin_name.as_str()));
    if !already_exists {
        plugins.push(Value::String(normalized_plugin_name));
    }

    if original.as_ref() == Some(&plugins) {
        return Ok(());
    }

    write_root_section_locked("plugin", &Value::Array(plugins))
}

pub fn remove_plugins_by_prefixes(prefixes: &[&str]) -> Result<(), AppError> {
    let _guard = opencode_write_lock().lock()?;

    let config = read_opencode_config()?;
    let Some(original) = config.get("plugin").and_then(Value::as_array) else {
        return Ok(());
    };

    let mut plugins = original.clone();
    plugins.retain(|v| {
        v.as_str()
            .map(|s| !matches_any_plugin_prefix(s, prefixes))
            .unwrap_or(true)
    });

    if plugins == *original {
        return Ok(());
    }

    if plugins.is_empty() {
        remove_root_section_locked("plugin")
    } else {
        write_root_section_locked("plugin", &Value::Array(plugins))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::{Mutex, OnceLock};

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    fn with_test_home<T>(initial_source: Option<&str>, test: impl FnOnce(&Path) -> T) -> T {
        let _guard = test_guard();
        let temp = tempfile::tempdir().unwrap();
        let opencode_dir = temp.path().join(".config").join("opencode");
        fs::create_dir_all(&opencode_dir).unwrap();
        let config_path = opencode_dir.join("opencode.json");
        if let Some(source) = initial_source {
            fs::write(&config_path, source).unwrap();
        }

        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
        std::env::set_var("HOME", temp.path());
        let result = test(&config_path);
        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        result
    }

    const COMMENTED_SOURCE: &str = r#"{
  // schema comment
  "$schema": "https://opencode.ai/config.json",
  "zeta_custom": {
    "keep": "me"
  },
  // provider comment
  "provider": {
    "existing": {
      "options": {
        "apiKey": "old-key"
      }
    }
  }
}
"#;

    #[test]
    #[serial]
    fn set_provider_preserves_comments_and_key_order() {
        with_test_home(Some(COMMENTED_SOURCE), |config_path| {
            set_provider("packycode", json!({ "options": { "apiKey": "new-key" } })).unwrap();

            let written = fs::read_to_string(config_path).unwrap();
            // provider 子树之外的注释与键序必须原样保留
            assert!(
                written.contains("// schema comment"),
                "注释应保留: {written}"
            );
            assert!(
                written.contains("// provider comment"),
                "注释应保留: {written}"
            );
            let schema_pos = written.find("$schema").unwrap();
            let zeta_pos = written.find("zeta_custom").unwrap();
            let provider_pos = written.find("\"provider\"").unwrap();
            assert!(
                schema_pos < zeta_pos && zeta_pos < provider_pos,
                "原键序不得被重排: {written}"
            );

            // 内容正确合并：原有 provider 保留，新 provider 写入
            let parsed: Value = json5::from_str(&written).unwrap();
            assert_eq!(
                parsed["provider"]["existing"]["options"]["apiKey"],
                "old-key"
            );
            assert_eq!(
                parsed["provider"]["packycode"]["options"]["apiKey"],
                "new-key"
            );
            assert_eq!(parsed["zeta_custom"]["keep"], "me");
        });
    }

    #[test]
    #[serial]
    fn set_provider_writes_backup_and_skips_noop_rewrite() {
        with_test_home(Some(COMMENTED_SOURCE), |config_path| {
            let provider_config = json!({ "options": { "apiKey": "new-key" } });
            set_provider("packycode", provider_config.clone()).unwrap();

            let backup_dir = get_opencode_backup_dir();
            let backups: Vec<_> = fs::read_dir(&backup_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert_eq!(backups.len(), 1, "修改前应生成一份备份");
            let backup_content = fs::read_to_string(backups[0].path()).unwrap();
            assert_eq!(
                backup_content, COMMENTED_SOURCE,
                "备份必须是修改前的原始内容"
            );

            // 同样内容再写一次：no-op，不应产生新备份，文件字节不变
            let written_before = fs::read_to_string(config_path).unwrap();
            set_provider("packycode", provider_config).unwrap();
            assert_eq!(fs::read_to_string(config_path).unwrap(), written_before);
            assert_eq!(fs::read_dir(&backup_dir).unwrap().count(), 1);
        });
    }

    #[test]
    #[serial]
    fn set_provider_creates_default_config_when_missing() {
        with_test_home(None, |config_path| {
            set_provider("packycode", json!({ "name": "PackyCode" })).unwrap();

            let written = fs::read_to_string(config_path).unwrap();
            let parsed: Value = json5::from_str(&written).unwrap();
            assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
            assert_eq!(parsed["provider"]["packycode"]["name"], "PackyCode");
            // 新建文件没有可备份的旧内容
            assert!(!get_opencode_backup_dir().exists());
        });
    }

    #[test]
    #[serial]
    fn concurrent_set_provider_keeps_all_entries() {
        with_test_home(
            Some("{\n  \"$schema\": \"https://opencode.ai/config.json\"\n}\n"),
            |_| {
                std::thread::scope(|scope| {
                    for i in 0..8 {
                        scope.spawn(move || {
                            set_provider(
                                &format!("provider-{i}"),
                                json!({ "options": { "apiKey": format!("key-{i}") } }),
                            )
                            .unwrap();
                        });
                    }
                });

                let providers = get_providers().unwrap();
                assert_eq!(providers.len(), 8, "并发写不得互相覆盖丢失条目");
                for i in 0..8 {
                    assert!(providers.contains_key(&format!("provider-{i}")));
                }
            },
        );
    }

    #[test]
    #[serial]
    fn remove_provider_only_touches_provider_section() {
        with_test_home(Some(COMMENTED_SOURCE), |config_path| {
            remove_provider("existing").unwrap();

            let written = fs::read_to_string(config_path).unwrap();
            assert!(written.contains("// schema comment"));
            let parsed: Value = json5::from_str(&written).unwrap();
            assert!(parsed["provider"].as_object().unwrap().is_empty());
            assert_eq!(parsed["zeta_custom"]["keep"], "me");

            // 删除不存在的 provider：no-op，不应改动文件
            let before = fs::read_to_string(config_path).unwrap();
            remove_provider("ghost").unwrap();
            assert_eq!(fs::read_to_string(config_path).unwrap(), before);
        });
    }

    #[test]
    #[serial]
    fn remove_plugins_drops_empty_plugin_key_and_keeps_comments() {
        let source = r#"{
  // keep this comment
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["oh-my-openagent@1.2.3"]
}
"#;
        with_test_home(Some(source), |config_path| {
            remove_plugins_by_prefixes(&["oh-my-openagent"]).unwrap();

            let written = fs::read_to_string(config_path).unwrap();
            assert!(written.contains("// keep this comment"), "{written}");
            let parsed: Value = json5::from_str(&written).unwrap();
            assert!(parsed.get("plugin").is_none(), "空插件数组应整键移除");
            assert_eq!(parsed["$schema"], "https://opencode.ai/config.json");
            // 输出必须仍是严格合法 JSON5（无尾随逗号等）
            assert!(serde_json::from_str::<Value>(
                &written
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
            .is_ok());
        });
    }

    #[test]
    #[serial]
    fn add_plugin_enforces_omo_exclusivity_and_canonicalizes() {
        let source = r#"{
  "$schema": "https://opencode.ai/config.json",
  "plugin": ["oh-my-opencode-slim@1.0.0", "unrelated-plugin"]
}
"#;
        with_test_home(Some(source), |config_path| {
            add_plugin("oh-my-opencode@2.0.0").unwrap();

            let written = fs::read_to_string(config_path).unwrap();
            let parsed: Value = json5::from_str(&written).unwrap();
            let plugins: Vec<&str> = parsed["plugin"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(plugins, vec!["unrelated-plugin", "oh-my-openagent@2.0.0"]);
        });
    }
}
