//! 路由层模型映射引擎
//!
//! 与 `model_mapper.rs`（按 Provider 的 env/catalog 旧逻辑）不同，这里提供一套
//! **按客户端区分、真正的 from→to 模型映射**：用户在路由 tab 配置规则，命中后
//! 目标模型作为最终上游模型，覆盖 catalog/env 等旧逻辑。未配置任何规则时不参与
//! 转发链，行为与现状完全一致。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

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
///
/// 注：请求路径现已统一走 `resolve_compiled`（编译缓存），本函数仅保留为兼容
/// 入口与测试基线，确保编译路径行为与原版完全等价。
#[allow(dead_code)]
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
///
/// 注：请求路径已统一走 `resolve_compiled`；本函数保留作为编译路径的行为基线。
#[allow(dead_code)]
pub fn resolve(rules: &[ModelRoutingRule], model: &str) -> Option<ResolveHit> {
    rules.iter().enumerate().find_map(|(idx, rule)| {
        rule_matches(rule, model).then(|| ResolveHit {
            rule_index: idx,
            target: rule.target.clone(),
        })
    })
}

// ---------------------------------------------------------------------------
// 编译缓存：把正则编译推到配置加载/热更新点，请求路径只持有 Arc 引用，
// 避免每请求 `Regex::new(&rule.pattern)` 的重复 CPU 成本。
// ---------------------------------------------------------------------------

/// 已编译的单条规则
pub struct CompiledRule {
    /// 规则在用户原始配置列表里的下标。编译阶段会跳过 disabled/空/坏正则规则，
    /// 但日志和测试结果仍应指向用户看到的原始行号。
    pub original_index: usize,
    pub match_type: MatchType,
    /// 原始 pattern（保留供未来诊断/日志使用；编译期已将其归一到 pattern_lc）
    #[allow(dead_code)]
    pub pattern_raw: String,
    /// 字面匹配用的小写 pattern（exact/prefix/suffix/keyword）
    pub pattern_lc: String,
    /// 仅 Regex 类型有值；编译失败的规则在 compile 阶段就被剔除
    pub regex: Option<regex::Regex>,
    pub target: String,
}

/// 已编译的全量配置：按 app_key 桶存放
#[derive(Default)]
pub struct CompiledRoutingConfig {
    by_app: HashMap<String, Arc<[CompiledRule]>>,
}

impl CompiledRoutingConfig {
    /// 把 raw 配置一次性编译；失败的正则会被记录并跳过——绝不让一条坏规则
    /// 拖垮整条请求路径，行为与请求期 `rule_matches` 的容错保持一致。
    pub fn compile(raw: &ModelRoutingConfig) -> Self {
        let mut by_app: HashMap<String, Arc<[CompiledRule]>> = HashMap::new();
        for app_key in ["claude", "codex", "gemini"] {
            let rules = rules_for(raw, app_key);
            let compiled: Vec<CompiledRule> = rules
                .iter()
                .enumerate()
                .filter(|(_, r)| r.enabled && !r.pattern.is_empty() && !r.target.is_empty())
                .filter_map(|(original_index, r)| {
                    let regex = if matches!(r.match_type, MatchType::Regex) {
                        match regex::Regex::new(&r.pattern) {
                            Ok(re) => Some(re),
                            Err(e) => {
                                log::warn!(
                                    "[ModelRouting] 跳过非法正则规则 app={app_key} pattern='{}': {e}",
                                    r.pattern
                                );
                                return None;
                            }
                        }
                    } else {
                        None
                    };
                    Some(CompiledRule {
                        original_index,
                        match_type: r.match_type,
                        pattern_raw: r.pattern.clone(),
                        pattern_lc: r.pattern.to_lowercase(),
                        regex,
                        target: r.target.clone(),
                    })
                })
                .collect();
            by_app.insert(app_key.to_string(), compiled.into());
        }
        Self { by_app }
    }

    pub fn rules_for(&self, app_key: &str) -> Option<&[CompiledRule]> {
        self.by_app.get(app_key).map(|a| a.as_ref())
    }

    /// 编译后是否全部为空（任何桶都没有有效规则）。
    pub fn is_empty(&self) -> bool {
        self.by_app.values().all(|bucket| bucket.is_empty())
    }
}

/// 编译版求解：与 `rule_matches`+`resolve` 行为一致，但不再做运行期 `Regex::new`。
pub fn resolve_compiled(rules: &[CompiledRule], model: &str) -> Option<ResolveHit> {
    // 一次性算小写视图，供所有字面匹配复用，避免每条规则各算一遍。
    let model_lc = model.to_lowercase();
    for rule in rules.iter() {
        let hit = match rule.match_type {
            MatchType::Regex => rule
                .regex
                .as_ref()
                .map(|re| re.is_match(model))
                .unwrap_or(false),
            MatchType::Exact => model_lc == rule.pattern_lc,
            MatchType::Prefix => model_lc.starts_with(&rule.pattern_lc),
            MatchType::Suffix => model_lc.ends_with(&rule.pattern_lc),
            MatchType::Keyword => model_lc.contains(&rule.pattern_lc),
        };
        if hit {
            return Some(ResolveHit {
                rule_index: rule.original_index,
                target: rule.target.clone(),
            });
        }
    }
    None
}

static COMPILED_ROUTING: OnceLock<RwLock<Arc<CompiledRoutingConfig>>> = OnceLock::new();

/// 设置层保存/启动加载入口调用。把 raw 配置一次性编译并替换全局缓存。
pub fn refresh_compiled_routing(raw: &ModelRoutingConfig) {
    let compiled = Arc::new(CompiledRoutingConfig::compile(raw));
    let store = COMPILED_ROUTING.get_or_init(|| RwLock::new(compiled.clone()));
    *store.write().expect("compiled routing lock poisoned") = compiled;
}

/// 请求路径取已编译配置的快照（只克隆 Arc）。
pub fn compiled_routing() -> Arc<CompiledRoutingConfig> {
    COMPILED_ROUTING
        .get_or_init(|| RwLock::new(Arc::new(CompiledRoutingConfig::default())))
        .read()
        .expect("compiled routing lock poisoned")
        .clone()
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

    // -----------------------------------------------------------------
    // 编译缓存：行为必须与运行期 rule_matches+resolve 完全等价。
    // -----------------------------------------------------------------

    #[test]
    fn compiled_matches_runtime_for_all_match_types() {
        let cfg = ModelRoutingConfig {
            codex: vec![
                rule(MatchType::Exact, "GPT-5.4-Mini", "exact-out"),
                rule(MatchType::Prefix, "claude-", "prefix-out"),
                rule(MatchType::Suffix, "-mini", "suffix-out"),
                rule(MatchType::Keyword, "sonnet", "kw-out"),
                rule(MatchType::Regex, r"^gpt-5\.\d+-mini$", "rx-out"),
            ],
            ..Default::default()
        };
        let compiled = CompiledRoutingConfig::compile(&cfg);
        let raw_rules = rules_for(&cfg, "codex");
        let comp_rules = compiled.rules_for("codex").unwrap();

        for model in [
            "gpt-5.4-mini",
            "claude-sonnet-4-6",
            "anything-else",
            "GPT-5.4-Mini",
        ] {
            assert_eq!(
                resolve(raw_rules, model).map(|h| (h.rule_index, h.target)),
                resolve_compiled(comp_rules, model).map(|h| (h.rule_index, h.target)),
                "behavior mismatch on {model}"
            );
        }
    }

    #[test]
    fn compile_skips_disabled_empty_and_bad_regex() {
        let mut disabled = rule(MatchType::Exact, "m", "t");
        disabled.enabled = false;
        let cfg = ModelRoutingConfig {
            codex: vec![
                disabled,
                rule(MatchType::Exact, "", "t"),
                rule(MatchType::Exact, "m", ""),
                rule(MatchType::Regex, "(", "t"),
                rule(MatchType::Exact, "ok", "out"),
            ],
            ..Default::default()
        };
        let compiled = CompiledRoutingConfig::compile(&cfg);
        let comp_rules = compiled.rules_for("codex").unwrap();
        assert_eq!(comp_rules.len(), 1);
        assert_eq!(comp_rules[0].target, "out");
        // 命中只能落在那一条，且下标指向用户原始配置里的位置。
        assert_eq!(resolve_compiled(comp_rules, "ok").unwrap().rule_index, 4);
        assert!(resolve_compiled(comp_rules, "m").is_none());
    }

    #[test]
    fn refresh_and_compiled_routing_global() {
        let cfg = ModelRoutingConfig {
            codex: vec![rule(MatchType::Exact, "abc", "def")],
            ..Default::default()
        };
        refresh_compiled_routing(&cfg);
        let snap = compiled_routing();
        let rules = snap.rules_for("codex").unwrap();
        assert_eq!(resolve_compiled(rules, "abc").unwrap().target, "def");
        // 再 refresh 空配置：is_empty 应反映出来
        refresh_compiled_routing(&ModelRoutingConfig::default());
        let snap2 = compiled_routing();
        assert!(snap2.is_empty());
    }
}
