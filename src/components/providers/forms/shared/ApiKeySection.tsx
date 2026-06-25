import { useTranslation } from "react-i18next";
import { KeyRound, Loader2 } from "lucide-react";
import ApiKeyInput from "../ApiKeyInput";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { useKeyPoolEntry } from "@/components/providers/keyPool/KeyPoolEntryContext";
import type { ProviderCategory } from "@/types";

interface ApiKeySectionProps {
  id?: string;
  label?: string;
  value: string;
  onChange: (value: string) => void;
  category?: ProviderCategory;
  shouldShowLink: boolean;
  websiteUrl: string;
  placeholder?: {
    official: string;
    thirdParty: string;
  };
  disabled?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  /** 新建场景：支持一次粘贴多个 Key，保存后自动组成 Key 池 */
  supportsMultiKey?: boolean;
}

export function ApiKeySection({
  id,
  label,
  value,
  onChange,
  category,
  shouldShowLink,
  websiteUrl,
  placeholder,
  disabled,
  supportsMultiKey,
}: ApiKeySectionProps) {
  const { t } = useTranslation();
  // 编辑供应商时由 EditProviderDialog 提供；新建等场景为 null，入口隐藏
  const keyPool = useKeyPoolEntry();

  const defaultPlaceholder = {
    official: t("providerForm.officialNoApiKey", {
      defaultValue: "官方供应商无需 API Key",
    }),
    thirdParty: t("providerForm.apiKeyAutoFill", {
      defaultValue: "输入 API Key，将自动填充到配置",
    }),
  };

  const finalPlaceholder = placeholder || defaultPlaceholder;
  const isBulkKeyInput = Boolean(
    supportsMultiKey && !keyPool && category !== "official",
  );

  return (
    <div className="space-y-1">
      <ApiKeyInput
        id={id}
        label={
          isBulkKeyInput
            ? t("providerForm.apiKeys", { defaultValue: "API Keys" })
            : label
        }
        value={value}
        onChange={onChange}
        placeholder={
          isBulkKeyInput
            ? t("providerForm.multiKeyPlaceholder", {
                defaultValue: "每行粘贴一个 Key，也可用空格、逗号分隔",
              })
            : category === "official"
              ? finalPlaceholder.official
              : finalPlaceholder.thirdParty
        }
        disabled={disabled ?? category === "official"}
        multiline={isBulkKeyInput}
      />
      {/* 新建场景（无 Key 池上下文）：提示可粘贴多个 Key 自动组池 */}
      {isBulkKeyInput && (
        <p className="pl-1 text-xs text-muted-foreground">
          {t("providerForm.multiKeyHint", {
            defaultValue:
              "支持粘贴多个 Key（空格 / 逗号 / 换行分隔），保存后自动组成 Key 池",
          })}
        </p>
      )}
      {/* Key 池统一入口：直连配置与路由请求共享同一个池 */}
      {keyPool && (
        <div className="mt-2 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-border-default bg-muted/30 px-3 py-2">
          <div className="flex min-w-0 flex-wrap items-center gap-2 text-xs text-muted-foreground">
            <KeyRound className="h-3.5 w-3.5 shrink-0" />
            {keyPool.isLoading ? (
              <span className="inline-flex items-center gap-1">
                <Loader2 className="h-3 w-3 animate-spin" />
                {t("providerKeys.loading", { defaultValue: "加载 Key 池…" })}
              </span>
            ) : keyPool.total > 0 ? (
              <>
                <span
                  className={cn(
                    "font-medium",
                    keyPool.issues > 0
                      ? "text-amber-600 dark:text-amber-400"
                      : "text-emerald-600 dark:text-emerald-400",
                  )}
                >
                  {keyPool.issues > 0
                    ? t("providerKeys.summaryWithIssues", {
                        total: keyPool.total,
                        issues: keyPool.issues,
                        defaultValue: "Key {{total}} / {{issues}} 异常",
                      })
                    : t("providerKeys.summary", {
                        available: keyPool.available,
                        total: keyPool.total,
                        defaultValue: "Key {{available}}/{{total}}",
                      })}
                </span>
                <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium">
                  {t(`providerKeys.configMode.${keyPool.configKeyMode}`, {
                    defaultValue:
                      keyPool.configKeyMode === "auto" ? "Auto" : "Manual",
                  })}
                </span>
                <span className="hidden min-w-0 truncate lg:inline">
                  {t("providerKeys.entryHint", {
                    defaultValue: "直连用优先 Key · 路由请求共享整个池",
                  })}
                </span>
              </>
            ) : (
              <span className="min-w-0">
                {t("providerKeys.entryEmpty", {
                  defaultValue: "可添加多个 Key 组成池，自动调度与故障转移",
                })}
              </span>
            )}
          </div>
          <Button
            type="button"
            size="sm"
            variant="outline"
            className="h-7 shrink-0 px-2.5 text-xs"
            onClick={keyPool.open}
          >
            {t("providerKeys.manage", { defaultValue: "管理 Key 池" })}
          </Button>
        </div>
      )}
      {/* API Key 获取链接 */}
      {shouldShowLink && websiteUrl && (
        <div className="space-y-2 -mt-1 pl-1">
          <a
            href={websiteUrl}
            target="_blank"
            rel="noopener noreferrer"
            className="text-xs text-blue-400 dark:text-blue-500 hover:text-blue-500 dark:hover:text-blue-400 transition-colors"
          >
            {t("providerForm.getApiKey", {
              defaultValue: "获取 API Key",
            })}
          </a>
        </div>
      )}
    </div>
  );
}
