//! 隐私过滤检测引擎（进程内，纯 Rust）
//!
//! 对一段文本做正则脱敏（mask），命中敏感信息后替换为固定占位符。
//! 全部在进程内完成，不依赖任何外部服务 / 子进程 / 二进制。
//!
//! 设计要点：
//! - best-effort：检测是纯函数，正则在首次访问时由 `once_cell::Lazy` 编译并缓存，永不 panic。
//! - 检测顺序经过编排，先打高置信度的密钥/结构化 PII，最后才跑高熵兜底，
//!   占位符（`[邮箱]` 等）本身不会被后续规则二次命中。
//! - Rust `regex` 不支持 look-around，所有规则均以 `\b` 词边界 + 闭包校验实现。
//!
//! 覆盖类型与占位符：
//! | 类型 | 占位符 |
//! |---|---|
//! | 邮箱 | `[邮箱]` |
//! | 手机号（中国大陆） | `[电话]` |
//! | 身份证号 | `[身份证]` |
//! | 银行卡号（Luhn） | `[银行卡]` |
//! | IP 地址（IPv4） | `[IP]` |
//! | API 密钥 / 凭证 / 高熵 Token | `[密钥]` |

use crate::proxy::types::PrivacyFilterConfig;
use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use std::collections::HashMap;

/// 单段文本的脱敏结果。
#[derive(Debug, Clone)]
pub struct RedactOutcome {
    /// 脱敏后的文本（无命中时与输入一致）。
    pub redacted: String,
    /// 命中并替换的敏感片段数量。
    pub count: usize,
}

// --- 正则规则集（首次访问时编译并缓存）---

static RE_EMAIL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").unwrap());

/// 中国大陆手机号：1 开头、第二位 3-9、共 11 位，前后用词边界限制，避免命中长数字串中段。
static RE_PHONE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b1[3-9]\d{9}\b").unwrap());

/// 身份证号：17 位数字 + 1 位校验位（数字或 X）。
static RE_ID_CARD: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{17}[0-9Xx]\b").unwrap());

/// 银行卡候选：13-19 位连续数字（实际是否脱敏由 Luhn 校验决定）。
static RE_BANK_CARD: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b\d{13,19}\b").unwrap());

/// IPv4 候选：四段点分十进制（实际是否脱敏由 octet ≤ 255 校验决定）。
static RE_IPV4: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}\b").unwrap());

/// 私钥块（PEM）。
static RE_PRIVATE_KEY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)-----BEGIN[ A-Z]*PRIVATE KEY-----.*?-----END[ A-Z]*PRIVATE KEY-----").unwrap()
});

/// JWT：三段 base64url，以 `eyJ` 开头。
static RE_JWT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}").unwrap()
});

/// 常见密钥前缀：OpenAI / GitHub / AWS / Google / Slack。
///
/// 每个分支前加 `\b` 左边界：否则 `sk-[…]{16,}` 会把英文里以 `sk-` 收尾的连字符
/// 短语吞掉（`task-management-…` / `risk-assessment-…` / `disk-usage-…`）。加边界后
/// 这类词中嵌套不再命中，而真实 key（前面是空格/引号/`=`/行首）仍正常匹配。
static RE_TOKEN_PREFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:\bsk-[A-Za-z0-9_\-]{16,}|\bgh[pousr]_[A-Za-z0-9]{20,}|\bAKIA[0-9A-Z]{16}|\bAIza[0-9A-Za-z_\-]{35}|\bxox[baprs]-[A-Za-z0-9\-]{10,})",
    )
    .unwrap()
});

/// `Bearer <token>` 形式的凭证。
static RE_BEARER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)bearer\s+[A-Za-z0-9._~+/\-]{16,}=*").unwrap());

/// 键值对凭证：`api_key = xxx` / `password: xxx` 等。group(1)=键与分隔符（保留），group(2)=值（脱敏）。
static RE_SECRET_KV: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?i)((?:api[_-]?key|access[_-]?token|secret(?:[_-]?key)?|client[_-]?secret|password|passwd|pwd|token)["']?\s*[:=]\s*["']?)([A-Za-z0-9._\-/+=]{6,})"#,
    )
    .unwrap()
});

/// 高熵兜底候选：长度 ≥ 20 的 token 字符集连续串（实际是否脱敏由熵+字符集判定）。
///
/// 字符集刻意**不含** `/ + =`：含 `/` 会把整条文件路径（`/Users/a8888/Documents/...`）
/// 当成单个 token，因混合字符熵偏高而被误判为密钥。剔除后路径会被 `/` 切成
/// 短目录段（各自 <20 或纯字母/纯数字），不再误伤；真正的随机密钥（URL-safe
/// base64 / token 用的就是 `A-Za-z0-9_-`）仍能命中。代价：含 `/` 的标准 base64
/// 裸串（如 AWS secret）兜底会漏，但这类通常带 `xxx=` 键名前缀，由 RE_SECRET_KV 接住。
static RE_TOKEN_CANDIDATE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[A-Za-z0-9_-]{20,}").unwrap());

// --- 校验辅助 ---

/// Luhn 校验（银行卡）。
fn luhn_valid(digits: &str) -> bool {
    let n = digits.len();
    if !(13..=19).contains(&n) {
        return false;
    }
    let mut sum = 0u32;
    // 从最右侧起，偶数位（1-based 从右数第 2、4… 位）翻倍。
    for (i, c) in digits.chars().rev().enumerate() {
        let d = match c.to_digit(10) {
            Some(d) => d,
            None => return false,
        };
        if i % 2 == 1 {
            let dd = d * 2;
            sum += if dd > 9 { dd - 9 } else { dd };
        } else {
            sum += d;
        }
    }
    sum % 10 == 0
}

/// 四段是否都 ≤ 255。
fn ipv4_valid(s: &str) -> bool {
    let mut parts = 0;
    for seg in s.split('.') {
        parts += 1;
        match seg.parse::<u32>() {
            Ok(v) if v <= 255 => {}
            _ => return false,
        }
    }
    parts == 4
}

/// 香农熵（bits/char）。
fn shannon_entropy(s: &str) -> f64 {
    let len = s.chars().count();
    if len == 0 {
        return 0.0;
    }
    let mut freq: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *freq.entry(c).or_insert(0) += 1;
    }
    let len_f = len as f64;
    freq.values()
        .map(|&c| {
            let p = c as f64 / len_f;
            -p * p.log2()
        })
        .sum()
}

/// 判断一个候选 token 是否像密钥。
///
/// 熵判定作用在「按 `-`/`_` 切分后**最长的连续字母数字子段**」上，而非整串：
/// 连字符短语 / slug（`risk-assessment-framework-2024`、`my-awesome-project-v3`）
/// 切开后每段都是短单词或纯数字，最长子段不够长而落选；无分隔的随机密钥子段
/// 仍然够长够乱，正常命中。子段需 ≥16、字母数字混合、香农熵 ≥ 3.5。
fn is_high_entropy_secret(tok: &str) -> bool {
    if tok.len() < 20 {
        return false;
    }
    // tok 来自 `[A-Za-z0-9_-]{20,}`，分隔符只会是 `_` / `-`。
    let seg = tok
        .split(|c: char| !c.is_ascii_alphanumeric())
        .max_by_key(|s| s.len())
        .unwrap_or("");
    if seg.len() < 16 {
        return false;
    }
    let has_alpha = seg.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = seg.chars().any(|c| c.is_ascii_digit());
    if !(has_alpha && has_digit) {
        return false;
    }
    shannon_entropy(seg) >= 3.5
}

// --- 替换辅助 ---

/// 无条件替换：每个匹配都替换为 `placeholder` 并计数。
fn redact_all(input: String, re: &Regex, placeholder: &str, count: &mut usize) -> String {
    let mut n = 0usize;
    let out = re.replace_all(&input, |_: &Captures| {
        n += 1;
        placeholder.to_string()
    });
    *count += n;
    out.into_owned()
}

/// 对单段文本执行脱敏。
pub fn redact_text(text: &str, cfg: &PrivacyFilterConfig) -> RedactOutcome {
    let mut count = 0usize;
    let mut s = text.to_string();

    // 1) 密钥/凭证（高置信度，先打）
    if cfg.secret {
        s = redact_all(s, &RE_PRIVATE_KEY, "[密钥]", &mut count);
        s = redact_all(s, &RE_JWT, "[密钥]", &mut count);
        s = redact_all(s, &RE_TOKEN_PREFIX, "[密钥]", &mut count);
        s = redact_all(s, &RE_BEARER, "[密钥]", &mut count);
        // 键值对：保留键名与分隔符，仅替换值
        {
            let mut n = 0usize;
            let out = RE_SECRET_KV.replace_all(&s, |caps: &Captures| {
                n += 1;
                format!("{}[密钥]", &caps[1])
            });
            count += n;
            s = out.into_owned();
        }
    }

    // 2) 邮箱
    if cfg.email {
        s = redact_all(s, &RE_EMAIL, "[邮箱]", &mut count);
    }

    // 3) IP（先于纯数字类，避免点分串被数字规则误吃）
    if cfg.ip {
        let mut n = 0usize;
        let out = RE_IPV4.replace_all(&s, |caps: &Captures| {
            let m = caps.get(0).unwrap().as_str();
            if ipv4_valid(m) {
                n += 1;
                "[IP]".to_string()
            } else {
                m.to_string()
            }
        });
        count += n;
        s = out.into_owned();
    }

    // 4) 身份证（18 位，必须先于银行卡，避免 18 位被当作卡号）
    if cfg.id_card {
        s = redact_all(s, &RE_ID_CARD, "[身份证]", &mut count);
    }

    // 5) 银行卡（13-19 位 + Luhn）
    if cfg.bank_card {
        let mut n = 0usize;
        let out = RE_BANK_CARD.replace_all(&s, |caps: &Captures| {
            let m = caps.get(0).unwrap().as_str();
            if luhn_valid(m) {
                n += 1;
                "[银行卡]".to_string()
            } else {
                m.to_string()
            }
        });
        count += n;
        s = out.into_owned();
    }

    // 6) 手机号
    if cfg.phone {
        s = redact_all(s, &RE_PHONE, "[电话]", &mut count);
    }

    // 7) 高熵兜底（最后，捕获未知密钥）
    if cfg.secret {
        let mut n = 0usize;
        let out = RE_TOKEN_CANDIDATE.replace_all(&s, |caps: &Captures| {
            let tok = caps.get(0).unwrap().as_str();
            if is_high_entropy_secret(tok) {
                n += 1;
                "[密钥]".to_string()
            } else {
                tok.to_string()
            }
        });
        count += n;
        s = out.into_owned();
    }

    RedactOutcome {
        redacted: s,
        count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_all() -> PrivacyFilterConfig {
        PrivacyFilterConfig {
            enabled: true,
            email: true,
            phone: true,
            id_card: true,
            bank_card: true,
            ip: true,
            secret: true,
        }
    }

    #[test]
    fn redacts_email_and_phone() {
        let out = redact_text("我的邮箱是 contact@example.com，手机号是 13800138000", &cfg_all());
        assert_eq!(out.redacted, "我的邮箱是 [邮箱]，手机号是 [电话]");
        assert_eq!(out.count, 2);
    }

    #[test]
    fn plain_text_unchanged() {
        let out = redact_text("今天天气不错，我们去公园散步吧。", &cfg_all());
        assert_eq!(out.count, 0);
        assert_eq!(out.redacted, "今天天气不错，我们去公园散步吧。");
    }

    #[test]
    fn redacts_id_card_not_as_bankcard() {
        // 18 位合法身份证结构
        let out = redact_text("身份证 11010519491231002X 请保密", &cfg_all());
        assert!(out.redacted.contains("[身份证]"));
        assert!(!out.redacted.contains("[银行卡]"));
    }

    #[test]
    fn bankcard_requires_luhn() {
        // 合法 Luhn（Visa 测试号）
        let ok = redact_text("卡号 4111111111111111", &cfg_all());
        assert!(ok.redacted.contains("[银行卡]"), "got: {}", ok.redacted);
        // 非法 Luhn 的 16 位数字不应被当作银行卡
        let bad = redact_text("订单 1234567890123456 号", &cfg_all());
        assert!(!bad.redacted.contains("[银行卡]"), "got: {}", bad.redacted);
    }

    #[test]
    fn redacts_ipv4_valid_only() {
        let out = redact_text("服务器 192.168.1.1 与 999.1.1.1", &cfg_all());
        assert!(out.redacted.contains("[IP]"));
        assert!(out.redacted.contains("999.1.1.1"));
    }

    #[test]
    fn redacts_known_secret_prefix() {
        let out = redact_text(
            "key=sk-abcdefghijklmnopqrstuvwxyz0123456789",
            &cfg_all(),
        );
        assert!(out.redacted.contains("[密钥]"), "got: {}", out.redacted);
    }

    #[test]
    fn secret_prefix_not_matched_mid_word() {
        // `sk-` 嵌在 task-/risk-/disk- 这类连字符短语中不应命中。
        for s in [
            "task-management-system-implementation",
            "risk-assessment-framework-2024",
            "disk-usage-monitoring-dashboard-v2",
        ] {
            let out = redact_text(&format!("模块 {s} 已就绪"), &cfg_all());
            assert!(!out.redacted.contains("[密钥]"), "false positive on: {s} -> {}", out.redacted);
            assert!(out.redacted.contains(s), "mangled: {}", out.redacted);
        }
        // 独立出现的真实 key 仍应命中。
        let real = redact_text("export KEY=sk-abcdefghijklmnopqrstuvwxyz here", &cfg_all());
        assert!(real.redacted.contains("[密钥]"), "should match real key: {}", real.redacted);
    }

    #[test]
    fn secret_kv_preserves_key() {
        let out = redact_text("password: hunter2supersecret", &cfg_all());
        assert!(out.redacted.starts_with("password:"));
        assert!(out.redacted.contains("[密钥]"));
    }

    #[test]
    fn high_entropy_token_redacted_but_words_kept() {
        // 混合字母数字的长 token → 命中
        let tok = redact_text("token a1B2c3D4e5F6g7H8i9J0kLmN here", &cfg_all());
        assert!(tok.redacted.contains("[密钥]"), "got: {}", tok.redacted);
        // 纯字母的长标识符（无数字） → 不命中
        let word = redact_text("getUserAccountByIdentifierAndStatus", &cfg_all());
        assert!(!word.redacted.contains("[密钥]"), "got: {}", word.redacted);
    }

    #[test]
    fn hyphenated_slugs_not_treated_as_secret() {
        // 含数字的连字符短语 / slug 不应被高熵兜底误判（熵判定走最长子段）。
        for s in [
            "risk-assessment-framework-2024",
            "disk-usage-monitoring-dashboard-v2",
            "my-awesome-project-v3-final",
            "feature-user-auth-refactor-2024",
        ] {
            let out = redact_text(&format!("分支 {s} 合并", ), &cfg_all());
            assert_eq!(out.count, 0, "slug misclassified: {s} -> {}", out.redacted);
            assert!(out.redacted.contains(s), "slug mangled: {}", out.redacted);
        }
    }

    #[test]
    fn respects_disabled_categories() {
        let mut cfg = cfg_all();
        cfg.email = false;
        let out = redact_text("邮箱 a@b.com 手机 13800138000", &cfg);
        assert!(out.redacted.contains("a@b.com"));
        assert!(out.redacted.contains("[电话]"));
    }

    #[test]
    fn file_paths_not_treated_as_secret() {
        // 高熵兜底不应把文件路径误判为密钥（字符集已剔除 `/`）。
        for p in [
            "/Users/a8888/Documents/gateway/cc-switch/src-tauri",
            "/home/user/projects/my-app-2024/node_modules/react-dom/index.js",
            "C:/Users/admin/AppData/Local/Temp/build123",
            "src/components/settings/PrivacyFilterSettings.tsx",
        ] {
            let out = redact_text(&format!("打开文件 {p} 看看"), &cfg_all());
            assert_eq!(out.count, 0, "path misclassified as secret: {p} -> {}", out.redacted);
            assert!(out.redacted.contains(p), "path mangled: {}", out.redacted);
        }
    }

    #[test]
    fn urlsafe_random_token_still_redacted() {
        // 收窄字符集后，URL-safe base64 / 混合随机串仍应命中兜底。
        let out = redact_text("token dGhpcyBpcyBhIHNlY3JldCB0b2tlbjEyMzQ1Ng here", &cfg_all());
        assert!(out.redacted.contains("[密钥]"), "got: {}", out.redacted);
    }
}
