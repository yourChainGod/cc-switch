import { useTranslation } from "react-i18next";
import { useState, useEffect } from "react";
import {
  ChevronDown,
  ChevronRight,
  FlaskConical,
  Coins,
  ListPlus,
  Plus,
  Trash2,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import type { CustomHeaderRule, ProviderTestConfig } from "@/types";

export type PricingModelSourceOption = "inherit" | "request" | "response";

interface ProviderPricingConfig {
  enabled: boolean;
  costMultiplier?: string;
  pricingModelSource: PricingModelSourceOption;
}

interface ProviderAdvancedConfigProps {
  testConfig: ProviderTestConfig;
  pricingConfig: ProviderPricingConfig;
  headerRules: CustomHeaderRule[];
  onTestConfigChange: (config: ProviderTestConfig) => void;
  onPricingConfigChange: (config: ProviderPricingConfig) => void;
  onHeaderRulesChange: (rules: CustomHeaderRule[]) => void;
}

export function ProviderAdvancedConfig({
  testConfig,
  pricingConfig,
  headerRules,
  onTestConfigChange,
  onPricingConfigChange,
  onHeaderRulesChange,
}: ProviderAdvancedConfigProps) {
  const { t } = useTranslation();
  const testConfigPanelId = "provider-test-config-panel";
  const pricingConfigPanelId = "provider-pricing-config-panel";
  const [isTestConfigOpen, setIsTestConfigOpen] = useState(testConfig.enabled);
  const [isPricingConfigOpen, setIsPricingConfigOpen] = useState(
    pricingConfig.enabled,
  );
  const headerRulesPanelId = "provider-header-rules-panel";
  const [isHeaderRulesOpen, setIsHeaderRulesOpen] = useState(
    headerRules.length > 0,
  );

  const updateHeaderRule = (index: number, patch: Partial<CustomHeaderRule>) => {
    onHeaderRulesChange(
      headerRules.map((rule, i) => (i === index ? { ...rule, ...patch } : rule)),
    );
  };

  useEffect(() => {
    setIsTestConfigOpen(testConfig.enabled);
  }, [testConfig.enabled]);

  useEffect(() => {
    setIsPricingConfigOpen(pricingConfig.enabled);
  }, [pricingConfig.enabled]);

  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-border/50 bg-muted/20">
        <div className="flex w-full flex-wrap items-center justify-between gap-3 p-4 transition-colors hover:bg-muted/30">
          <button
            type="button"
            className="flex min-w-0 flex-1 items-center gap-3 text-left"
            aria-expanded={isTestConfigOpen}
            aria-controls={testConfigPanelId}
            onClick={() => setIsTestConfigOpen(!isTestConfigOpen)}
          >
            <FlaskConical className="h-4 w-4 text-muted-foreground" />
            <span className="font-medium">
              {t("providerAdvanced.testConfig", {
                defaultValue: "模型测试配置",
              })}
            </span>
            {isTestConfigOpen ? (
              <ChevronDown className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
            ) : (
              <ChevronRight className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
            )}
          </button>
          <div className="flex items-center gap-2">
            <Label
              htmlFor="test-config-enabled"
              className="text-sm text-muted-foreground"
            >
              {t("providerAdvanced.useCustomConfig", {
                defaultValue: "使用单独配置",
              })}
            </Label>
            <Switch
              id="test-config-enabled"
              checked={testConfig.enabled}
              onCheckedChange={(checked) => {
                onTestConfigChange({ ...testConfig, enabled: checked });
                if (checked) setIsTestConfigOpen(true);
              }}
            />
          </div>
        </div>
        <div
          id={testConfigPanelId}
          className={cn(
            "overflow-hidden transition-all duration-200",
            isTestConfigOpen
              ? "max-h-[500px] opacity-100"
              : "max-h-0 opacity-0",
          )}
        >
          <div className="border-t border-border/50 p-4 space-y-4">
            <p className="text-sm text-muted-foreground">
              {t("providerAdvanced.testConfigDesc", {
                defaultValue:
                  "为此供应商配置单独的模型测试参数，不启用时使用全局配置。",
              })}
            </p>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="test-model">
                  {t("providerAdvanced.testModel", {
                    defaultValue: "测试模型",
                  })}
                </Label>
                <Input
                  id="test-model"
                  value={testConfig.testModel || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      testModel: e.target.value || undefined,
                    })
                  }
                  placeholder={t("providerAdvanced.testModelPlaceholder", {
                    defaultValue: "留空使用全局配置",
                  })}
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="test-timeout">
                  {t("providerAdvanced.timeoutSecs", {
                    defaultValue: "超时时间（秒）",
                  })}
                </Label>
                <Input
                  id="test-timeout"
                  type="number"
                  min={1}
                  max={300}
                  value={testConfig.timeoutSecs || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      timeoutSecs: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="45"
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="test-prompt">
                  {t("providerAdvanced.testPrompt", {
                    defaultValue: "测试提示词",
                  })}
                </Label>
                <Input
                  id="test-prompt"
                  value={testConfig.testPrompt || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      testPrompt: e.target.value || undefined,
                    })
                  }
                  placeholder="Who are you?"
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="degraded-threshold">
                  {t("providerAdvanced.degradedThreshold", {
                    defaultValue: "降级阈值（毫秒）",
                  })}
                </Label>
                <Input
                  id="degraded-threshold"
                  type="number"
                  min={100}
                  max={60000}
                  value={testConfig.degradedThresholdMs || ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      degradedThresholdMs: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="6000"
                  disabled={!testConfig.enabled}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="max-retries">
                  {t("providerAdvanced.maxRetries", {
                    defaultValue: "最大重试次数",
                  })}
                </Label>
                <Input
                  id="max-retries"
                  type="number"
                  min={0}
                  max={10}
                  value={testConfig.maxRetries ?? ""}
                  onChange={(e) =>
                    onTestConfigChange({
                      ...testConfig,
                      maxRetries: e.target.value
                        ? parseInt(e.target.value, 10)
                        : undefined,
                    })
                  }
                  placeholder="2"
                  disabled={!testConfig.enabled}
                />
              </div>
            </div>
          </div>
        </div>
      </div>

      {/* 计费配置 */}
      <div className="rounded-lg border border-border/50 bg-muted/20">
        <div className="flex w-full flex-wrap items-center justify-between gap-3 p-4 transition-colors hover:bg-muted/30">
          <button
            type="button"
            className="flex min-w-0 flex-1 items-center gap-3 text-left"
            aria-expanded={isPricingConfigOpen}
            aria-controls={pricingConfigPanelId}
            onClick={() => setIsPricingConfigOpen(!isPricingConfigOpen)}
          >
            <Coins className="h-4 w-4 text-muted-foreground" />
            <span className="font-medium">
              {t("providerAdvanced.pricingConfig", {
                defaultValue: "计费配置",
              })}
            </span>
            {isPricingConfigOpen ? (
              <ChevronDown className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
            ) : (
              <ChevronRight className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
            )}
          </button>
          <div className="flex items-center gap-2">
            <Label
              htmlFor="pricing-config-enabled"
              className="text-sm text-muted-foreground"
            >
              {t("providerAdvanced.useCustomPricing", {
                defaultValue: "使用单独配置",
              })}
            </Label>
            <Switch
              id="pricing-config-enabled"
              checked={pricingConfig.enabled}
              onCheckedChange={(checked) => {
                onPricingConfigChange({ ...pricingConfig, enabled: checked });
                if (checked) setIsPricingConfigOpen(true);
              }}
            />
          </div>
        </div>
        <div
          id={pricingConfigPanelId}
          className={cn(
            "overflow-hidden transition-all duration-200",
            isPricingConfigOpen
              ? "max-h-[500px] opacity-100"
              : "max-h-0 opacity-0",
          )}
        >
          <div className="border-t border-border/50 p-4 space-y-4">
            <p className="text-sm text-muted-foreground">
              {t("providerAdvanced.pricingConfigDesc", {
                defaultValue:
                  "为此供应商配置单独的计费参数，不启用时使用全局默认配置。",
              })}
            </p>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label htmlFor="cost-multiplier">
                  {t("providerAdvanced.costMultiplier", {
                    defaultValue: "成本倍率",
                  })}
                </Label>
                <Input
                  id="cost-multiplier"
                  type="number"
                  step="0.01"
                  min="0"
                  inputMode="decimal"
                  value={pricingConfig.costMultiplier || ""}
                  onChange={(e) =>
                    onPricingConfigChange({
                      ...pricingConfig,
                      costMultiplier: e.target.value || undefined,
                    })
                  }
                  placeholder={t("providerAdvanced.costMultiplierPlaceholder", {
                    defaultValue: "留空使用全局默认（1）",
                  })}
                  disabled={!pricingConfig.enabled}
                />
                <p className="text-xs text-muted-foreground">
                  {t("providerAdvanced.costMultiplierHint", {
                    defaultValue: "实际成本 = 基础成本 × 倍率，支持小数如 1.5",
                  })}
                </p>
              </div>
              <div className="space-y-2">
                <Label htmlFor="pricing-model-source">
                  {t("providerAdvanced.pricingModelSourceLabel", {
                    defaultValue: "计费模式",
                  })}
                </Label>
                <Select
                  value={pricingConfig.pricingModelSource}
                  onValueChange={(value) =>
                    onPricingConfigChange({
                      ...pricingConfig,
                      pricingModelSource: value as PricingModelSourceOption,
                    })
                  }
                  disabled={!pricingConfig.enabled}
                >
                  <SelectTrigger id="pricing-model-source">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="inherit">
                      {t("providerAdvanced.pricingModelSourceInherit", {
                        defaultValue: "继承全局默认",
                      })}
                    </SelectItem>
                    <SelectItem value="request">
                      {t("providerAdvanced.pricingModelSourceRequest", {
                        defaultValue: "请求模型",
                      })}
                    </SelectItem>
                    <SelectItem value="response">
                      {t("providerAdvanced.pricingModelSourceResponse", {
                        defaultValue: "返回模型",
                      })}
                    </SelectItem>
                  </SelectContent>
                </Select>
                <p className="text-xs text-muted-foreground">
                  {t("providerAdvanced.pricingModelSourceHint", {
                    defaultValue: "选择按请求模型还是返回模型进行定价匹配",
                  })}
                </p>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div className="rounded-lg border border-border/50 bg-muted/20">
        <button
          type="button"
          className="flex w-full items-center gap-3 p-4 text-left transition-colors hover:bg-muted/30"
          aria-expanded={isHeaderRulesOpen}
          aria-controls={headerRulesPanelId}
          onClick={() => setIsHeaderRulesOpen(!isHeaderRulesOpen)}
        >
          <ListPlus className="h-4 w-4 text-muted-foreground" />
          <span className="font-medium">
            {t("providerAdvanced.headerRules", {
              defaultValue: "自定义请求头规则",
            })}
          </span>
          {headerRules.length > 0 && (
            <span className="rounded-full bg-primary/10 px-2 py-0.5 text-xs text-primary">
              {headerRules.length}
            </span>
          )}
          {isHeaderRulesOpen ? (
            <ChevronDown className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
          ) : (
            <ChevronRight className="ml-auto h-4 w-4 shrink-0 text-muted-foreground" />
          )}
        </button>
        <div
          id={headerRulesPanelId}
          className={cn(
            "overflow-hidden transition-all duration-200",
            isHeaderRulesOpen ? "max-h-[600px] opacity-100" : "max-h-0 opacity-0",
          )}
        >
          <div className="border-t border-border/50 p-4 space-y-3">
            <p className="text-sm text-muted-foreground">
              {t("providerAdvanced.headerRulesDesc", {
                defaultValue:
                  "按顺序改写发往该供应商的请求头。认证头（authorization / x-api-key / x-goog-api-key）受保护，规则不会生效。",
              })}
            </p>
            {headerRules.map((rule, index) => (
              <div
                key={index}
                className="grid grid-cols-[110px_minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-2"
              >
                <Select
                  value={rule.action}
                  onValueChange={(value) =>
                    updateHeaderRule(index, {
                      action: value as CustomHeaderRule["action"],
                    })
                  }
                >
                  <SelectTrigger aria-label="action">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="override">
                      {t("providerAdvanced.headerRuleOverride", {
                        defaultValue: "覆盖",
                      })}
                    </SelectItem>
                    <SelectItem value="append">
                      {t("providerAdvanced.headerRuleAppend", {
                        defaultValue: "追加",
                      })}
                    </SelectItem>
                    <SelectItem value="remove">
                      {t("providerAdvanced.headerRuleRemove", {
                        defaultValue: "删除",
                      })}
                    </SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  value={rule.name}
                  onChange={(e) =>
                    updateHeaderRule(index, { name: e.target.value })
                  }
                  placeholder={t("providerAdvanced.headerRuleNamePlaceholder", {
                    defaultValue: "Header 名称，如 anthropic-beta",
                  })}
                  autoComplete="off"
                  spellCheck={false}
                />
                <Input
                  value={rule.value}
                  onChange={(e) =>
                    updateHeaderRule(index, { value: e.target.value })
                  }
                  placeholder={
                    rule.action === "remove"
                      ? t("providerAdvanced.headerRuleRemoveValuePlaceholder", {
                          defaultValue: "留空删整头；填 token 按 CSV 摘除",
                        })
                      : t("providerAdvanced.headerRuleValuePlaceholder", {
                          defaultValue: "值",
                        })
                  }
                  autoComplete="off"
                  spellCheck={false}
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="text-muted-foreground hover:text-destructive"
                  aria-label={t("common.delete", { defaultValue: "删除" })}
                  onClick={() =>
                    onHeaderRulesChange(
                      headerRules.filter((_, i) => i !== index),
                    )
                  }
                >
                  <Trash2 className="h-4 w-4" />
                </Button>
              </div>
            ))}
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => {
                onHeaderRulesChange([
                  ...headerRules,
                  { action: "override", name: "", value: "" },
                ]);
                setIsHeaderRulesOpen(true);
              }}
            >
              <Plus className="mr-1 h-4 w-4" />
              {t("providerAdvanced.headerRuleAdd", {
                defaultValue: "添加规则",
              })}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
