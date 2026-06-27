use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::AppError;

/// 获取用户主目录，带回退和日志
///
/// ## Windows 注意事项
///
/// - `dirs::home_dir()` 在 Windows 上使用 `SHGetKnownFolderPath(FOLDERID_Profile)`，
///   返回的是真实用户目录（类似 `C:\\Users\\Alice`），与 v3.10.2 行为一致。
/// - 不要直接使用 `HOME` 环境变量：它可能由 Git/Cygwin/MSYS 等第三方工具注入，
///   且不一定等于用户目录，可能导致 `.cc-switch/cc-switch.db` 路径变化，从而“看起来像数据丢失”。
///
/// ## 测试隔离
///
/// 为了让 Windows CI/本地测试能稳定隔离真实用户数据，可通过 `CC_SWITCH_TEST_HOME`
/// 显式覆盖 home dir（仅用于测试/调试场景）。
pub fn get_home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("CC_SWITCH_TEST_HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    dirs::home_dir().unwrap_or_else(|| {
        log::warn!("无法获取用户主目录，回退到当前目录");
        PathBuf::from(".")
    })
}

/// 获取 Claude Code 配置目录路径
pub fn get_claude_config_dir() -> PathBuf {
    if let Some(custom) = crate::settings::get_claude_override_dir() {
        return custom;
    }

    get_home_dir().join(".claude")
}

/// 默认 Claude MCP 配置文件路径 (~/.claude.json)
pub fn get_default_claude_mcp_path() -> PathBuf {
    get_home_dir().join(".claude.json")
}

fn derive_mcp_path_from_override(dir: &Path) -> Option<PathBuf> {
    let file_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())?
        .trim()
        .to_string();
    if file_name.is_empty() {
        return None;
    }
    let parent = dir.parent().unwrap_or_else(|| Path::new(""));
    Some(parent.join(format!("{file_name}.json")))
}

/// 获取 Claude MCP 配置文件路径，若设置了目录覆盖则与覆盖目录同级
pub fn get_claude_mcp_path() -> PathBuf {
    if let Some(custom_dir) = crate::settings::get_claude_override_dir() {
        if let Some(path) = derive_mcp_path_from_override(&custom_dir) {
            return path;
        }
    }
    get_default_claude_mcp_path()
}

/// 获取 Claude Code 主配置文件路径
pub fn get_claude_settings_path() -> PathBuf {
    let dir = get_claude_config_dir();
    let settings = dir.join("settings.json");
    if settings.exists() {
        return settings;
    }
    // 兼容旧版命名：若存在旧文件则继续使用
    let legacy = dir.join("claude.json");
    if legacy.exists() {
        return legacy;
    }
    // 默认新建：回落到标准文件名 settings.json（不再生成 claude.json）
    settings
}

/// 获取应用配置目录路径 (~/.cc-switch)
pub fn get_app_config_dir() -> PathBuf {
    if let Some(custom) = crate::app_store::get_app_config_dir_override() {
        return custom;
    }

    let default_dir = get_home_dir().join(".cc-switch");

    // 兼容 v3.10.3：当用户环境存在 `HOME` 且与真实用户目录不同，
    // v3.10.3 可能在 `HOME/.cc-switch/` 下创建/使用了数据库。
    // 这里仅在“默认位置没有数据库”时回退到旧位置，避免再次出现“供应商消失”问题，
    // 同时也避免新安装因为 `HOME` 被设置而写入非预期路径。
    #[cfg(windows)]
    {
        let default_db = default_dir.join("cc-switch.db");
        if !default_db.exists() {
            if let Ok(home_env) = std::env::var("HOME") {
                let trimmed = home_env.trim();
                if !trimmed.is_empty() {
                    let legacy_dir = PathBuf::from(trimmed).join(".cc-switch");
                    if legacy_dir.join("cc-switch.db").exists() {
                        log::info!(
                            "Detected v3.10.3 legacy database at {}, using it instead of {}",
                            legacy_dir.display(),
                            default_dir.display()
                        );
                        return legacy_dir;
                    }
                }
            }
        }
    }

    default_dir
}

/// 获取应用配置文件路径
pub fn get_app_config_path() -> PathBuf {
    get_app_config_dir().join("config.json")
}

/// 清理供应商名称，确保文件名安全
#[allow(dead_code)]
pub fn sanitize_provider_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '-',
            _ => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// 获取供应商配置文件路径
#[allow(dead_code)]
pub fn get_provider_config_path(provider_id: &str, provider_name: Option<&str>) -> PathBuf {
    let base_name = provider_name
        .map(sanitize_provider_name)
        .unwrap_or_else(|| sanitize_provider_name(provider_id));

    get_claude_config_dir().join(format!("settings-{base_name}.json"))
}

/// 读取 JSON 配置文件
pub fn read_json_file<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<T, AppError> {
    if !path.exists() {
        return Err(AppError::Config(format!("文件不存在: {}", path.display())));
    }

    let content = fs::read_to_string(path).map_err(|e| AppError::io(path, e))?;

    serde_json::from_str(&content).map_err(|e| AppError::json(path, e))
}

/// 递归排序 JSON 对象的键（按字母顺序），确保序列化输出是确定性的
fn sort_json_keys(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted_map = Map::new();
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted_map.insert(key.clone(), sort_json_keys(&map[key]));
            }
            Value::Object(sorted_map)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sort_json_keys).collect()),
        other => other.clone(),
    }
}

/// 写入 JSON 配置文件（键按字母排序，确保确定性输出）
pub fn write_json_file<T: Serialize>(path: &Path, data: &T) -> Result<(), AppError> {
    // 确保目录存在
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }

    let value = serde_json::to_value(data).map_err(|e| AppError::JsonSerialize { source: e })?;
    let sorted_value = sort_json_keys(&value);
    let json = serde_json::to_string_pretty(&sorted_value)
        .map_err(|e| AppError::JsonSerialize { source: e })?;

    atomic_write(path, json.as_bytes())
}

/// 原子写入文本文件（用于 TOML/纯文本）
pub fn write_text_file(path: &Path, data: &str) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
    }
    atomic_write(path, data.as_bytes())
}

/// 原子写入：写入同目录临时文件，fsync 后原子替换目标，避免半写状态
///
/// - 临时文件必须与目标同目录（rename 跨卷会失败）。
/// - rename 前对临时文件执行 `sync_all()`，确保掉电后不会出现
///   “rename 已生效但内容未落盘”的空文件/半写文件。
/// - 通过 `NamedTempFile::persist` 替换目标：Unix 上是 rename(2)，
///   Windows 上使用 MOVEFILE_REPLACE_EXISTING 原子覆盖，消除旧实现
///   “先删后改名”两步之间目标文件彻底消失的窗口。
///
/// 行为：未指定 `mode` 时，若目标已存在则继承其权限位（保留用户手动
/// 收紧过的配置文件权限）；目标不存在时沿用 `NamedTempFile` 的默认 0600。
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), AppError> {
    atomic_write_mode(path, data, None)
}

/// 与 [`atomic_write`] 等价的原子写入，但允许在 Unix 下**于临时文件创建时**
/// 就用 `O_CREAT` + `mode` 直接指定权限位（而不是事后 chmod），消除
/// “rename 已生效但权限尚未收紧”的可被旁路读取的窗口。
///
/// - `mode = Some(0o600)`：临时文件一创建即为 owner-only，rename 后无需二次 chmod。
/// - `mode = None`：行为与历史 [`atomic_write`] 一致——若目标已存在，继承其
///   权限位；否则使用 `NamedTempFile` 的默认 0600。
/// - Windows 忽略 `mode`（沿用系统默认 ACL）。
pub fn atomic_write_mode(path: &Path, data: &[u8], mode: Option<u32>) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Config("无效的路径".to_string()))?;
    fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;

    if path.file_name().is_none() {
        return Err(AppError::Config("无效的文件名".to_string()));
    }

    // Unix 且指定了 mode 时：手撸 OpenOptions 走 O_CREAT|O_EXCL + mode，
    // 让“临时文件创建瞬间”就是目标权限，rename 后零窗口。
    // 其它情况（Windows，或未指定 mode）：复用 NamedTempFile，保留
    // “rename 失败自动清理 tmp + Windows 上 MOVEFILE_REPLACE_EXISTING 原子覆盖”。
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        let tmp = path.with_extension(format!(
            "tmp.{}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(m)
            .open(&tmp)
            .map_err(|e| AppError::io(&tmp, e))?;
        f.write_all(data).map_err(|e| AppError::io(&tmp, e))?;
        f.sync_all().map_err(|e| AppError::io(&tmp, e))?;
        // 显式关闭以确保 rename 前所有 fd 状态稳定（Drop 也会关，但显式更清晰）
        drop(f);

        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(AppError::IoContext {
                context: format!("原子替换失败: {} -> {}", tmp.display(), path.display()),
                source: e,
            });
        }
        return Ok(());
    }

    // 兼容路径：未指定 mode（或非 Unix）走原 NamedTempFile 实现
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| AppError::IoContext {
        context: format!("创建临时文件失败: {}", parent.display()),
        source: e,
    })?;
    tmp.write_all(data)
        .map_err(|e| AppError::io(tmp.path(), e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| AppError::io(tmp.path(), e))?;

    #[cfg(unix)]
    {
        // mode 为 None 时，若目标已存在则继承其权限（保留用户手动收紧过的配置）。
        // 目标不存在时沿用 NamedTempFile 的默认 0600。
        let _ = mode; // mode 已在上方 Some(_) 分支处理
        if let Ok(meta) = fs::metadata(path) {
            let _ = fs::set_permissions(tmp.path(), meta.permissions());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }

    tmp.persist(path).map_err(|e| {
        let context = format!(
            "原子替换失败: {} -> {}",
            e.file.path().display(),
            path.display()
        );
        AppError::IoContext {
            context,
            source: e.error,
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_mcp_path_from_override_preserves_folder_name() {
        let override_dir = PathBuf::from("/tmp/profile/.claude");
        let derived = derive_mcp_path_from_override(&override_dir)
            .expect("should derive path for nested dir");
        assert_eq!(derived, PathBuf::from("/tmp/profile/.claude.json"));
    }

    #[test]
    fn derive_mcp_path_from_override_handles_non_hidden_folder() {
        let override_dir = PathBuf::from("/data/claude-config");
        let derived = derive_mcp_path_from_override(&override_dir)
            .expect("should derive path for standard dir");
        assert_eq!(derived, PathBuf::from("/data/claude-config.json"));
    }

    #[test]
    fn derive_mcp_path_from_override_supports_relative_rootless_dir() {
        let override_dir = PathBuf::from("claude");
        let derived = derive_mcp_path_from_override(&override_dir)
            .expect("should derive path for single segment");
        assert_eq!(derived, PathBuf::from("claude.json"));
    }

    #[test]
    fn derive_mcp_path_from_root_like_dir_returns_none() {
        let override_dir = PathBuf::from("/");
        assert!(derive_mcp_path_from_override(&override_dir).is_none());
    }

    #[test]
    fn sort_json_keys_sorts_top_level_object() {
        let input = serde_json::json!({
            "z": 1,
            "a": 2,
            "m": 3,
        });
        let sorted = sort_json_keys(&input);
        let serialized = serde_json::to_string(&sorted).unwrap();
        assert_eq!(serialized, r#"{"a":2,"m":3,"z":1}"#);
    }

    #[test]
    fn sort_json_keys_recurses_into_nested_objects() {
        let input = serde_json::json!({
            "outer_b": {"z": 1, "a": 2},
            "outer_a": {"y": 3, "b": 4},
        });
        let sorted = sort_json_keys(&input);
        let serialized = serde_json::to_string(&sorted).unwrap();
        assert_eq!(
            serialized,
            r#"{"outer_a":{"b":4,"y":3},"outer_b":{"a":2,"z":1}}"#
        );
    }

    #[test]
    fn sort_json_keys_preserves_array_order() {
        let input = serde_json::json!([3, 1, 2]);
        let sorted = sort_json_keys(&input);
        let serialized = serde_json::to_string(&sorted).unwrap();
        assert_eq!(serialized, "[3,1,2]");
    }

    #[test]
    fn sort_json_keys_sorts_objects_inside_arrays_but_keeps_array_order() {
        let input = serde_json::json!([
            {"z": 1, "a": 2},
            {"y": 3, "b": 4},
        ]);
        let sorted = sort_json_keys(&input);
        let serialized = serde_json::to_string(&sorted).unwrap();
        assert_eq!(serialized, r#"[{"a":2,"z":1},{"b":4,"y":3}]"#);
    }

    #[test]
    fn sort_json_keys_passes_through_primitives() {
        let cases = vec![
            serde_json::json!("hello"),
            serde_json::json!(42),
            serde_json::json!(3.5),
            serde_json::json!(true),
            serde_json::json!(null),
        ];
        for value in cases {
            let sorted = sort_json_keys(&value);
            assert_eq!(sorted, value);
        }
    }

    #[test]
    fn sort_json_keys_handles_empty_collections() {
        let empty_obj = serde_json::json!({});
        assert_eq!(
            serde_json::to_string(&sort_json_keys(&empty_obj)).unwrap(),
            "{}"
        );

        let empty_arr = serde_json::json!([]);
        assert_eq!(
            serde_json::to_string(&sort_json_keys(&empty_arr)).unwrap(),
            "[]"
        );
    }

    #[test]
    fn sort_json_keys_produces_identical_output_for_different_insertion_orders() {
        // 核心保证：同一逻辑配置无论键的插入顺序如何，写出的字节序列必须一致。
        let mut a = Map::new();
        a.insert("env".to_string(), serde_json::json!({"PATH": "/usr/bin"}));
        a.insert("model".to_string(), serde_json::json!("claude-sonnet-4-5"));
        a.insert("permissions".to_string(), serde_json::json!({"allow": []}));

        let mut b = Map::new();
        b.insert("permissions".to_string(), serde_json::json!({"allow": []}));
        b.insert("model".to_string(), serde_json::json!("claude-sonnet-4-5"));
        b.insert("env".to_string(), serde_json::json!({"PATH": "/usr/bin"}));

        let sorted_a = sort_json_keys(&Value::Object(a));
        let sorted_b = sort_json_keys(&Value::Object(b));

        assert_eq!(
            serde_json::to_string(&sorted_a).unwrap(),
            serde_json::to_string(&sorted_b).unwrap(),
        );
    }

    #[test]
    fn atomic_write_creates_missing_directory_and_writes_full_content() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let target = dir.path().join("nested").join("deeper").join("config.json");
        let data = br#"{"model":"claude-sonnet-4-5","env":{"PATH":"/usr/bin"}}"#;

        atomic_write(&target, data).expect("atomic write should succeed");

        assert_eq!(fs::read(&target).expect("read back"), data);
    }

    #[test]
    fn atomic_write_overwrites_existing_target() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let target = dir.path().join("config.json");
        fs::write(&target, b"old-content-which-is-much-longer-than-new").expect("seed old file");

        atomic_write(&target, b"new").expect("overwrite should succeed");

        // 内容完整替换（无旧内容残留），且目标始终存在
        assert_eq!(fs::read(&target).expect("read back"), b"new");

        // 不应留下任何临时文件
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("list dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name != "config.json")
            .collect();
        assert!(leftovers.is_empty(), "unexpected leftovers: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_existing_target_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create temp dir");
        let target = dir.path().join("config.json");
        fs::write(&target, b"old").expect("seed old file");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("chmod 600");

        atomic_write(&target, b"new").expect("overwrite should succeed");

        let mode = fs::metadata(&target)
            .expect("stat target")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "existing permissions should be preserved");
        assert_eq!(fs::read(&target).expect("read back"), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_write_mode_sets_0600_at_creation() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        atomic_write_mode(&path, b"{\"k\":1}", Some(0o600)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic_write_mode 应在 rename 后立即是 0600");
        // 内容也要写对
        assert_eq!(fs::read(&path).unwrap(), b"{\"k\":1}");
        // 不应留下任何 .tmp.* 临时残留
        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .expect("list dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name != "secret.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "atomic_write_mode 不应残留临时文件: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_atomic_write_mode_0600_overwrites_existing_loose_target() {
        // 覆盖一个原本 0644 的目标，最终应是 0600（来自创建时的 mode）。
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        atomic_write_mode(&path, b"new", Some(0o600)).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "Some(0o600) 必须覆盖已有的宽松权限，不应继承旧 0644"
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn atomic_write_rejects_path_without_file_name() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let invalid = dir.path().join("..");
        let err = atomic_write(&invalid, b"data").expect_err("should reject");
        assert!(matches!(err, AppError::Config(_)));
    }
}

/// 复制文件
pub fn copy_file(from: &Path, to: &Path) -> Result<(), AppError> {
    fs::copy(from, to).map_err(|e| AppError::IoContext {
        context: format!("复制文件失败 ({} -> {})", from.display(), to.display()),
        source: e,
    })?;
    Ok(())
}

/// 删除文件
pub fn delete_file(path: &Path) -> Result<(), AppError> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| AppError::io(path, e))?;
    }
    Ok(())
}

/// 检查 Claude Code 配置状态
#[derive(Serialize, Deserialize)]
pub struct ConfigStatus {
    pub exists: bool,
    pub path: String,
}

/// 获取 Claude Code 配置状态
pub fn get_claude_config_status() -> ConfigStatus {
    let path = get_claude_settings_path();
    ConfigStatus {
        exists: path.exists(),
        path: path.to_string_lossy().to_string(),
    }
}
