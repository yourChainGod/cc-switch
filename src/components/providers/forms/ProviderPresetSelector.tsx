import { useTranslation } from "react-i18next";
import { FormLabel } from "@/components/ui/form";
import { ClaudeIcon, CodexIcon, GeminiIcon } from "@/components/BrandIcons";
import { Zap, Layers, Settings2 } from "lucide-react";
import type { ProviderPreset } from "@/config/claudeProviderPresets";
import type { CodexProviderPreset } from "@/config/codexProviderPresets";
import type { GeminiProviderPreset } from "@/config/geminiProviderPresets";
import type { ClaudeDesktopProviderPreset } from "@/config/claudeDesktopProviderPresets";
import type { OpenCodeProviderPreset } from "@/config/opencodeProviderPresets";
import type { OpenClawProviderPreset } from "@/config/openclawProviderPresets";
import type { HermesProviderPreset } from "@/config/hermesProviderPresets";
import type { ProviderCategory } from "@/types";
import {
  universalProviderPresets,
  type UniversalProviderPreset,
} from "@/config/universalProviderPresets";
import { ProviderIcon } from "@/components/ProviderIcon";

type AnyPreset =
  | ProviderPreset
  | CodexProviderPreset
  | GeminiProviderPreset
  | ClaudeDesktopProviderPreset
  | OpenCodeProviderPreset
  | OpenClawProviderPreset
  | HermesProviderPreset;

type PresetEntry = {
  id: string;
  preset: AnyPreset;
};

interface ProviderPresetSelectorProps {
  selectedPresetId: string | null;
  presetEntries: PresetEntry[];
  presetCategoryLabels: Record<string, string>;
  onPresetChange: (value: string) => void;
  onUniversalPresetSelect?: (preset: UniversalProviderPreset) => void;
  onManageUniversalProviders?: () => void;
  category?: ProviderCategory; // 当前选中的分类
}

export function ProviderPresetSelector({
  selectedPresetId,
  presetEntries,
  presetCategoryLabels,
  onPresetChange,
  onUniversalPresetSelect,
  onManageUniversalProviders,
  category,
}: ProviderPresetSelectorProps) {
  const { t } = useTranslation();

  const getCategoryHint = (): React.ReactNode => {
    switch (category) {
      case "official":
        return t("providerForm.officialHint", {
          defaultValue: "💡 官方供应商使用浏览器登录，无需配置 API Key",
        });
      case "cn_official":
        return t("providerForm.cnOfficialApiKeyHint", {
          defaultValue: "💡 国产官方供应商只需填写 API Key，请求地址已预设",
        });
      case "aggregator":
        return t("providerForm.aggregatorApiKeyHint", {
          defaultValue: "💡 聚合服务供应商只需填写 API Key 即可使用",
        });
      case "third_party":
        return t("providerForm.thirdPartyApiKeyHint", {
          defaultValue: "💡 第三方供应商需要填写 API Key 和请求地址",
        });
      case "custom":
        return t("providerForm.customApiKeyHint", {
          defaultValue: "💡 自定义配置需手动填写所有必要字段",
        });
      case "omo":
        return t("providerForm.omoHint", {
          defaultValue:
            "💡 OMO 配置管理 Agent 模型分配，兼容 oh-my-openagent.jsonc / oh-my-opencode.jsonc",
        });
      default:
        return t("providerPreset.hint", {
          defaultValue: "选择预设后可继续调整下方字段。",
        });
    }
  };

  const renderPresetIcon = (preset: AnyPreset) => {
    const iconType = preset.theme?.icon;

    switch (iconType) {
      case "claude":
        return <ClaudeIcon size={18} />;
      case "codex":
        return <CodexIcon size={18} />;
      case "gemini":
        return <GeminiIcon size={18} />;
      case "generic":
        return <Zap size={18} />;
      default:
        return (
          <ProviderIcon
            icon={(preset as { icon?: string }).icon}
            name={preset.name}
            size={18}
          />
        );
    }
  };

  // 卡片副标题：官方走登录提示，其余显示分类名
  const getPresetCardDescription = (preset: AnyPreset) => {
    if (preset.category === "official") {
      return t("providerPreset.officialCardDesc", {
        defaultValue: "浏览器登录，无需 API Key",
      });
    }
    return (
      presetCategoryLabels[preset.category ?? "others"] ??
      t("providerPreset.other")
    );
  };

  const presetCardClass = (isSelected: boolean) =>
    `flex items-center gap-3 rounded-xl border p-3 text-left transition-colors ${
      isSelected
        ? "border-primary bg-primary/5 ring-1 ring-primary"
        : "border-border hover:border-primary/40 hover:bg-accent/50"
    }`;

  return (
    <div className="space-y-3">
      <FormLabel>{t("providerPreset.label")}</FormLabel>
      <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
        <button
          type="button"
          onClick={() => onPresetChange("custom")}
          className={presetCardClass(selectedPresetId === "custom")}
        >
          <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent text-muted-foreground">
            <Settings2 size={18} />
          </span>
          <span className="min-w-0">
            <span className="block truncate text-sm font-medium text-foreground">
              {t("providerPreset.custom")}
            </span>
            <span className="block truncate text-xs text-muted-foreground">
              {t("providerPreset.customCardDesc", {
                defaultValue: "手动填写请求地址与 API Key",
              })}
            </span>
          </span>
        </button>

        {presetEntries.map((entry) => {
          const isSelected = selectedPresetId === entry.id;
          return (
            <button
              key={entry.id}
              type="button"
              onClick={() => onPresetChange(entry.id)}
              className={presetCardClass(isSelected)}
            >
              <span
                className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent text-muted-foreground"
                style={
                  entry.preset.theme?.backgroundColor
                    ? {
                        backgroundColor: entry.preset.theme.backgroundColor,
                        color: entry.preset.theme.textColor || "#FFFFFF",
                      }
                    : undefined
                }
              >
                {renderPresetIcon(entry.preset)}
              </span>
              <span className="min-w-0">
                <span className="block truncate text-sm font-medium text-foreground">
                  {entry.preset.nameKey
                    ? t(entry.preset.nameKey)
                    : entry.preset.name}
                </span>
                <span className="block truncate text-xs text-muted-foreground">
                  {getPresetCardDescription(entry.preset)}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      {onUniversalPresetSelect && universalProviderPresets.length > 0 && (
        <>
          <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
            {universalProviderPresets.map((preset) => (
              <button
                key={`universal-${preset.providerType}`}
                type="button"
                onClick={() => onUniversalPresetSelect(preset)}
                className={`${presetCardClass(false)} relative`}
                title={t("universalProvider.hint", {
                  defaultValue:
                    "跨应用统一配置，自动同步到 Claude/Codex/Gemini",
                })}
              >
                <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent text-muted-foreground">
                  <ProviderIcon
                    icon={preset.icon}
                    name={preset.name}
                    size={18}
                  />
                </span>
                <span className="min-w-0">
                  <span className="block truncate text-sm font-medium text-foreground">
                    {preset.name}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {t("universalProvider.cardDesc", {
                      defaultValue: "统一供应商 · 同步到多个应用",
                    })}
                  </span>
                </span>
                <span className="absolute -top-1 -right-1 flex items-center gap-0.5 rounded-full bg-gradient-to-r from-indigo-500 to-purple-500 px-1.5 py-0.5 text-[10px] font-bold text-white shadow-md">
                  <Layers className="h-2.5 w-2.5" />
                </span>
              </button>
            ))}
            {onManageUniversalProviders && (
              <button
                type="button"
                onClick={onManageUniversalProviders}
                className={presetCardClass(false)}
                title={t("universalProvider.manage", {
                  defaultValue: "管理统一供应商",
                })}
              >
                <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-accent text-muted-foreground">
                  <Settings2 size={18} />
                </span>
                <span className="min-w-0">
                  <span className="block truncate text-sm font-medium text-foreground">
                    {t("universalProvider.manage", {
                      defaultValue: "管理",
                    })}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {t("universalProvider.manageCardDesc", {
                      defaultValue: "查看与编辑统一供应商",
                    })}
                  </span>
                </span>
              </button>
            )}
          </div>
        </>
      )}

      <p className="text-xs text-muted-foreground">{getCategoryHint()}</p>
    </div>
  );
}
