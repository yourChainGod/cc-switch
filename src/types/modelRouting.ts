// 路由层模型映射类型（与后端 proxy/model_routing.rs 对齐，camelCase）

export type MatchType = "exact" | "prefix" | "suffix" | "keyword" | "regex";

// 支持的客户端（按桶区分）
export type ModelRoutingClient = "claude" | "codex" | "gemini";

export interface ModelRoutingRule {
  enabled: boolean;
  matchType: MatchType;
  pattern: string;
  target: string;
}

export interface ModelRoutingConfig {
  claude: ModelRoutingRule[];
  codex: ModelRoutingRule[];
  gemini: ModelRoutingRule[];
}

// test_model_routing 返回
export interface ModelRoutingTestResult {
  matched: boolean;
  ruleIndex: number | null;
  matchType: MatchType | null;
  pattern: string | null;
  output: string;
  // 下标 → 正则编译错误信息
  regexErrors: Record<number, string>;
}

export function emptyModelRoutingConfig(): ModelRoutingConfig {
  return { claude: [], codex: [], gemini: [] };
}

export function createDefaultRule(): ModelRoutingRule {
  return { enabled: true, matchType: "exact", pattern: "", target: "" };
}
