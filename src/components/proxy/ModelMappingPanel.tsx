import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Plus,
  Trash2,
  ArrowUp,
  ArrowDown,
  Save,
  Loader2,
  ArrowRight,
  FlaskConical,
  Info,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { proxyApi } from "@/lib/api/proxy";
import {
  useModelRoutingConfig,
  useUpdateModelRoutingConfig,
} from "@/lib/query/modelRouting";
import type {
  MatchType,
  ModelRoutingClient,
  ModelRoutingConfig,
  ModelRoutingRule,
  ModelRoutingTestResult,
} from "@/types/modelRouting";
import { createDefaultRule } from "@/types/modelRouting";

const CLIENTS: ModelRoutingClient[] = ["claude", "codex", "gemini"];
const MATCH_TYPES: MatchType[] = [
  "exact",
  "prefix",
  "suffix",
  "keyword",
  "regex",
];

export function ModelMappingPanel({ client }: { client?: ModelRoutingClient }) {
  const { t } = useTranslation();
  const { data: config, isLoading } = useModelRoutingConfig();
  const updateConfig = useUpdateModelRoutingConfig();

  // 本地草稿（编辑期与服务端解耦，保存时一次性提交）
  const [draft, setDraft] = useState<ModelRoutingConfig>({
    claude: [],
    codex: [],
    gemini: [],
  });

  useEffect(() => {
    if (config) {
      setDraft({
        claude: config.claude ?? [],
        codex: config.codex ?? [],
        gemini: config.gemini ?? [],
      });
    }
  }, [config]);

  const handleSave = () => {
    updateConfig.mutate(draft);
  };

  const setRules = (client: ModelRoutingClient, rules: ModelRoutingRule[]) =>
    setDraft((prev) => ({ ...prev, [client]: rules }));

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-6">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  const hint = (
    <div className="flex items-start gap-2 rounded-lg border border-blue-500/30 bg-blue-500/10 p-3">
      <Info className="mt-0.5 h-4 w-4 flex-shrink-0 text-blue-500" />
      <p className="text-xs text-blue-700 dark:text-blue-300">
        {t("proxy.modelMapping.hint", {
          defaultValue:
            "按客户端区分的「真正」模型映射：当请求模型命中某条规则时，目标模型即为最终上游模型，覆盖供应商的 catalog / 环境变量映射。未配置任何规则时不影响现有行为。自上而下首条命中生效。",
        })}
      </p>
    </div>
  );

  const saveButton = (
    <div className="flex justify-end pt-1">
      <Button onClick={handleSave} disabled={updateConfig.isPending}>
        {updateConfig.isPending ? (
          <>
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            {t("common.saving", { defaultValue: "保存中..." })}
          </>
        ) : (
          <>
            <Save className="mr-2 h-4 w-4" />
            {t("common.save", { defaultValue: "保存" })}
          </>
        )}
      </Button>
    </div>
  );

  // 单客户端模式：只渲染该客户端的规则编辑器（保存仍提交完整草稿，其余客户端沿用服务端配置）
  if (client) {
    return (
      <div className="space-y-4">
        {hint}
        <ClientRuleEditor
          client={client}
          rules={draft[client]}
          onChange={(rules) => setRules(client, rules)}
        />
        {saveButton}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {hint}

      <Tabs defaultValue="claude" className="w-full">
        <TabsList className="grid w-full grid-cols-3">
          {CLIENTS.map((c) => (
            <TabsTrigger key={c} value={c} className="capitalize">
              {c}
            </TabsTrigger>
          ))}
        </TabsList>
        {CLIENTS.map((client) => (
          <TabsContent key={client} value={client} className="mt-4">
            <ClientRuleEditor
              client={client}
              rules={draft[client]}
              onChange={(rules) => setRules(client, rules)}
            />
          </TabsContent>
        ))}
      </Tabs>

      {saveButton}
    </div>
  );
}

interface ClientRuleEditorProps {
  client: ModelRoutingClient;
  rules: ModelRoutingRule[];
  onChange: (rules: ModelRoutingRule[]) => void;
}

function ClientRuleEditor({ client, rules, onChange }: ClientRuleEditorProps) {
  const { t } = useTranslation();

  const matchTypeLabel = (m: MatchType) =>
    t(`proxy.modelMapping.matchType.${m}`, {
      defaultValue: {
        exact: "精确",
        prefix: "前缀",
        suffix: "后缀",
        keyword: "关键词",
        regex: "正则",
      }[m],
    });

  const updateRule = (index: number, patch: Partial<ModelRoutingRule>) => {
    onChange(rules.map((r, i) => (i === index ? { ...r, ...patch } : r)));
  };
  const removeRule = (index: number) =>
    onChange(rules.filter((_, i) => i !== index));
  const moveRule = (index: number, dir: -1 | 1) => {
    const next = index + dir;
    if (next < 0 || next >= rules.length) return;
    const copy = [...rules];
    [copy[index], copy[next]] = [copy[next], copy[index]];
    onChange(copy);
  };
  const addRule = () => onChange([...rules, createDefaultRule()]);

  return (
    <div className="space-y-3">
      {rules.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border bg-muted/20 py-6 text-center text-sm text-muted-foreground">
          {t("proxy.modelMapping.empty", {
            defaultValue: "暂无映射规则，点击下方按钮添加",
          })}
        </div>
      ) : (
        <div className="space-y-2">
          {/* 表头 */}
          <div className="hidden grid-cols-[auto_7rem_1fr_auto_1fr_auto] items-center gap-2 px-1 text-xs text-muted-foreground md:grid">
            <span>{t("proxy.modelMapping.col.enabled", { defaultValue: "启用" })}</span>
            <span>{t("proxy.modelMapping.col.matchType", { defaultValue: "匹配方式" })}</span>
            <span>{t("proxy.modelMapping.col.pattern", { defaultValue: "匹配模式" })}</span>
            <span />
            <span>{t("proxy.modelMapping.col.target", { defaultValue: "目标模型" })}</span>
            <span>{t("proxy.modelMapping.col.actions", { defaultValue: "操作" })}</span>
          </div>

          {rules.map((rule, index) => {
            const isRegexInvalid =
              rule.matchType === "regex" &&
              rule.pattern.length > 0 &&
              !isValidRegex(rule.pattern);
            return (
              <div
                key={index}
                className="grid grid-cols-2 items-center gap-2 rounded-lg border border-border bg-card/40 p-2 md:grid-cols-[auto_7rem_1fr_auto_1fr_auto]"
              >
                <Switch
                  checked={rule.enabled}
                  onCheckedChange={(v) => updateRule(index, { enabled: v })}
                />
                <Select
                  value={rule.matchType}
                  onValueChange={(v) =>
                    updateRule(index, { matchType: v as MatchType })
                  }
                >
                  <SelectTrigger className="h-9">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MATCH_TYPES.map((m) => (
                      <SelectItem key={m} value={m}>
                        {matchTypeLabel(m)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <div className="col-span-2 md:col-span-1">
                  <Input
                    value={rule.pattern}
                    onChange={(e) =>
                      updateRule(index, { pattern: e.target.value })
                    }
                    placeholder={t("proxy.modelMapping.patternPlaceholder", {
                      defaultValue: "如 gpt-5.4-mini",
                    })}
                    className={`h-9 ${isRegexInvalid ? "border-red-500" : ""}`}
                  />
                  {isRegexInvalid && (
                    <p className="mt-1 text-xs text-red-500">
                      {t("proxy.modelMapping.invalidRegex", {
                        defaultValue: "正则表达式无效",
                      })}
                    </p>
                  )}
                </div>
                <ArrowRight className="hidden h-4 w-4 text-muted-foreground md:block" />
                <Input
                  value={rule.target}
                  onChange={(e) => updateRule(index, { target: e.target.value })}
                  placeholder={t("proxy.modelMapping.targetPlaceholder", {
                    defaultValue: "如 gpt-5.5",
                  })}
                  className="col-span-2 h-9 md:col-span-1"
                />
                <div className="col-span-2 flex items-center justify-end gap-1 md:col-span-1">
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    disabled={index === 0}
                    onClick={() => moveRule(index, -1)}
                  >
                    <ArrowUp className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8"
                    disabled={index === rules.length - 1}
                    onClick={() => moveRule(index, 1)}
                  >
                    <ArrowDown className="h-4 w-4" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    className="h-8 w-8 text-red-500 hover:text-red-600"
                    onClick={() => removeRule(index)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      )}

      <Button variant="outline" size="sm" onClick={addRule} className="gap-1.5">
        <Plus className="h-4 w-4" />
        {t("proxy.modelMapping.addRule", { defaultValue: "添加规则" })}
      </Button>

      <MatchTester client={client} rules={rules} />
    </div>
  );
}

interface MatchTesterProps {
  client: ModelRoutingClient;
  rules: ModelRoutingRule[];
}

function MatchTester({ client, rules }: MatchTesterProps) {
  const { t } = useTranslation();
  const [input, setInput] = useState("");
  const [result, setResult] = useState<ModelRoutingTestResult | null>(null);
  const [testing, setTesting] = useState(false);

  // 输入或规则变化时清空旧结果，避免误导
  useEffect(() => {
    setResult(null);
  }, [input, rules]);

  const runTest = async () => {
    if (!input.trim()) return;
    setTesting(true);
    try {
      const res = await proxyApi.testModelRouting(input.trim(), rules);
      setResult(res);
    } finally {
      setTesting(false);
    }
  };

  const matchTypeLabel = useMemo(
    () => (m: string | null) =>
      m
        ? t(`proxy.modelMapping.matchType.${m}`, {
            defaultValue: m,
          })
        : "",
    [t],
  );

  return (
    <div className="mt-2 space-y-2 rounded-lg border border-border bg-muted/30 p-3">
      <div className="flex items-center gap-2 text-xs font-medium text-muted-foreground">
        <FlaskConical className="h-3.5 w-3.5" />
        {t("proxy.modelMapping.tester.title", {
          defaultValue: "匹配测试",
        })}
        <span className="font-normal capitalize">({client})</span>
      </div>
      <div className="flex gap-2">
        <Input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && runTest()}
          placeholder={t("proxy.modelMapping.tester.placeholder", {
            defaultValue: "输入请求模型名，回车测试",
          })}
          className="h-9"
        />
        <Button
          variant="secondary"
          size="sm"
          onClick={runTest}
          disabled={testing || !input.trim()}
        >
          {testing ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            t("proxy.modelMapping.tester.run", { defaultValue: "测试" })
          )}
        </Button>
      </div>
      {result && (
        <div className="text-sm">
          {result.matched ? (
            <div className="flex flex-wrap items-center gap-1.5 text-green-600 dark:text-green-400">
              <span>
                {t("proxy.modelMapping.tester.matched", {
                  index: (result.ruleIndex ?? 0) + 1,
                  matchType: matchTypeLabel(result.matchType),
                  pattern: result.pattern ?? "",
                  defaultValue: `命中第 ${(result.ruleIndex ?? 0) + 1} 条（${matchTypeLabel(result.matchType)}: ${result.pattern}）`,
                })}
              </span>
              <ArrowRight className="h-3.5 w-3.5" />
              <code className="rounded bg-background px-1.5 py-0.5 font-medium text-foreground">
                {result.output}
              </code>
            </div>
          ) : (
            <span className="text-muted-foreground">
              {t("proxy.modelMapping.tester.noMatch", {
                model: result.output,
                defaultValue: `未命中任何规则，按原样透传：${result.output}`,
              })}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

function isValidRegex(pattern: string): boolean {
  try {
    // 仅做前端语法预检；后端用 Rust regex crate 校验为准
    new RegExp(pattern);
    return true;
  } catch {
    return false;
  }
}
