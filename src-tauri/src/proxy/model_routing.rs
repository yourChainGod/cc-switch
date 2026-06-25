//! 路由层模型映射引擎
//!
//! 与 `model_mapper.rs`（按 Provider 的 env/catalog 旧逻辑）不同，这里提供一套
//! **按客户端区分、真正的 from→to 模型映射**：用户在路由 tab 配置规则，命中后
//! 目标模型作为最终上游模型，覆盖 catalog/env 等旧逻辑。未配置任何规则时不参与
//! 转发链，行为与现状完全一致。

use serde::{Deserialize, Serialize};

/// 匹配方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchType {
    /// 精确相等
    Exact,
    /// 前缀匹配
    Prefix,
    /// 后缀匹配
    Suffix,
    /// 关键词（子串）匹配
    Keyword,
    /// 正则匹配
    Regex,
}

impl MatchType {
    pub fn as_str(&self) -> &'static str {
        match self {
            MatchType::Exact => "exact",
            MatchType::Prefix => "prefix",
            MatchType::Suffix => "suffix",
            MatchType::Keyword => "keyword",
            MatchType::Regex => "regex",
        }
    }
}

/// 单条模型映射规则
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoutingRule {
    /// 是否启用（缺省 true，方便手写 JSON）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 匹配方式
    pub match_type: MatchType,
    /// 匹配模式（pattern）
    #[serde(default)]
    pub pattern: String,
    /// 命中后替换成的目标模型
    #[serde(default)]
    pub target: String,
}

fn default_true() -> bool {
    true
}

/// 按客户端分桶的模型映射配置
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoutingConfig {
    #[serde(default)]
    pub claude: Vec<ModelRoutingRule>,
    #[serde(default)]
    pub codex: Vec<ModelRoutingRule>,
    #[serde(default)]
    pub gemini: Vec<ModelRoutingRule>,
}

/// 一次成功匹配的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveHit {
    /// 命中规则在该桶内的下标（从 0 计）
    pub rule_index: usize,
    /// 目标模型
    pub target: String,
}

impl ModelRoutingConfig {
    /// 是否在任一桶配置了规则（用于快速短路）
    pub fn is_empty(&self) -> bool {
        self.claude.is_empty() && self.codex.is_empty() && self.gemini.is_empty()
    }
}

/// 取指定客户端的规则桶；未知 app_key 返回空切片，
/// 从而永不命中、行为不变。
pub fn rules_for<'a>(cfg: &'a ModelRoutingConfig, app_key: &str) -> &'a [ModelRoutingRule] {
    match app_key {
        "claude" => &cfg.claude,
        "codex" => &cfg.codex,
        "gemini" => &cfg.gemini,
        _ => &[],
    }
}

/// 判断单条规则是否命中给定模型名。
///
/// 字面模式（exact/prefix/suffix/keyword）大小写不敏感；regex 原样编译（用户可用
/// `(?i)` 自控），编译失败视为不命中并告警——绝不因一条坏规则让整条请求失败。
fn rule_matches(rule: &ModelRoutingRule, model: &str) -> bool {
    if !rule.enabled || rule.pattern.is_empty() || rule.target.is_empty() {
        return false;
    }
    match rule.match_type {
        MatchType::Regex => match regex::Regex::new(&rule.pattern) {
            Ok(re) => re.is_match(model),
            Err(e) => {
                log::warn!(
                    "[ModelRouting] 跳过非法正则规则 pattern='{}': {e}",
                    rule.pattern
                );
                false
            }
        },
        other => {
            let model_lc = model.to_lowercase();
            let pat_lc = rule.pattern.to_lowercase();
            match other {
                MatchType::Exact => model_lc == pat_lc,
                MatchType::Prefix => model_lc.starts_with(&pat_lc),
                MatchType::Suffix => model_lc.ends_with(&pat_lc),
                MatchType::Keyword => model_lc.contains(&pat_lc),
                MatchType::Regex => unreachable!(),
            }
        }
    }
}

/// 自上而下求解首条命中规则。未命中返回 None（按原样透传）。
pub fn resolve(rules: &[ModelRoutingRule], model: &str) -> Option<ResolveHit> {
    rules.iter().enumerate().find_map(|(idx, rule)| {
        rule_matches(rule, model).then(|| ResolveHit {
            rule_index: idx,
            target: rule.target.clone(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(match_type: MatchType, pattern: &str, target: &str) -> ModelRoutingRule {
        ModelRoutingRule {
            enabled: true,
            match_type,
            pattern: pattern.to_string(),
            target: target.to_string(),
        }
    }

    #[test]
    fn exact_match_case_insensitive() {
        let rules = vec![rule(MatchType::Exact, "GPT-5.4-Mini", "gpt-5.5")];
        let hit = resolve(&rules, "gpt-5.4-mini").unwrap();
        assert_eq!(hit.target, "gpt-5.5");
        assert_eq!(hit.rule_index, 0);
        // 非精确不命中
        assert!(resolve(&rules, "gpt-5.4-mini-2025").is_none());
    }

    #[test]
    fn prefix_match() {
        let rules = vec![rule(MatchType::Prefix, "claude-", "my-claude")];
        assert_eq!(
            resolve(&rules, "claude-sonnet-4-6").unwrap().target,
            "my-claude"
        );
        assert!(resolve(&rules, "gpt-claude").is_none());
    }

    #[test]
    fn suffix_match() {
        let rules = vec![rule(MatchType::Suffix, "-mini", "big")];
        assert_eq!(resolve(&rules, "gpt-5.4-mini").unwrap().target, "big");
        assert!(resolve(&rules, "gpt-5.4-mini-pro").is_none());
    }

    #[test]
    fn keyword_match() {
        let rules = vec![rule(MatchType::Keyword, "sonnet", "mapped")];
        assert_eq!(
            resolve(&rules, "claude-sonnet-4-6").unwrap().target,
            "mapped"
        );
        assert!(resolve(&rules, "claude-opus").is_none());
    }

    #[test]
    fn regex_match_and_case_flag() {
        let rules = vec![rule(MatchType::Regex, r"^gpt-5\.\d+-mini$", "gpt-5.5")];
        assert_eq!(resolve(&rules, "gpt-5.4-mini").unwrap().target, "gpt-5.5");
        assert!(resolve(&rules, "gpt-5.4-mini-x").is_none());
        // 默认大小写敏感，用户用 (?i) 自控
        let ci = vec![rule(MatchType::Regex, r"(?i)^GPT", "x")];
        assert!(resolve(&ci, "gpt-5").is_some());
    }

    #[test]
    fn first_hit_wins() {
        let rules = vec![
            rule(MatchType::Keyword, "mini", "first"),
            rule(MatchType::Exact, "gpt-5.4-mini", "second"),
        ];
        let hit = resolve(&rules, "gpt-5.4-mini").unwrap();
        assert_eq!(hit.target, "first");
        assert_eq!(hit.rule_index, 0);
    }

    #[test]
    fn skips_disabled_empty_and_invalid_regex() {
        // 禁用
        let mut r = rule(MatchType::Exact, "m", "t");
        r.enabled = false;
        assert!(resolve(&[r], "m").is_none());
        // 空 pattern / 空 target
        assert!(resolve(&[rule(MatchType::Exact, "", "t")], "m").is_none());
        assert!(resolve(&[rule(MatchType::Exact, "m", "")], "m").is_none());
        // 非法正则跳过而非 panic
        assert!(resolve(&[rule(MatchType::Regex, "(", "t")], "m").is_none());
    }

    #[test]
    fn empty_config_no_hit() {
        let cfg = ModelRoutingConfig::default();
        assert!(cfg.is_empty());
        assert!(resolve(rules_for(&cfg, "codex"), "gpt-5.4-mini").is_none());
        // 未知客户端取空桶
        assert!(rules_for(&cfg, "unknown-app").is_empty());
    }

    #[test]
    fn rules_for_buckets() {
        let cfg = ModelRoutingConfig {
            codex: vec![rule(MatchType::Exact, "a", "b")],
            ..Default::default()
        };
        assert_eq!(rules_for(&cfg, "codex").len(), 1);
        assert_eq!(rules_for(&cfg, "claude").len(), 0);
        assert_eq!(rules_for(&cfg, "gemini").len(), 0);
    }

    #[test]
    fn serde_defaults_enabled_true() {
        let json = r#"{"matchType":"exact","pattern":"a","target":"b"}"#;
        let r: ModelRoutingRule = serde_json::from_str(json).unwrap();
        assert!(r.enabled);
        assert_eq!(r.match_type, MatchType::Exact);
    }

    #[test]
    fn serde_config_roundtrip() {
        let cfg = ModelRoutingConfig {
            codex: vec![rule(MatchType::Regex, "(?i)gpt", "x")],
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: ModelRoutingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.codex.len(), 1);
        assert_eq!(back.codex[0].match_type, MatchType::Regex);
    }
}
