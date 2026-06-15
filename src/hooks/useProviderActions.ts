import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { providersApi, settingsApi, type AppId } from "@/lib/api";
import type { Provider, ProviderKey, UsageScript } from "@/types";
import {
  injectCodingPlanUsageScript,
  injectOfficialSubscriptionUsageScript,
} from "@/config/codingPlanProviders";
import { autoConfigureSub2apiUsage } from "@/lib/usage/autoDetectSub2api";
import {
  useAddProviderMutation,
  useUpdateProviderMutation,
  useDeleteProviderMutation,
  useSwitchProviderMutation,
} from "@/lib/query";
import { extractErrorMessage } from "@/utils/errorUtils";
import {
  extractCodexWireApi,
  isCodexChatWireApi,
} from "@/utils/providerConfigUtils";

/**
 * Hook for managing provider actions (add, update, delete, switch)
 * Extracts business logic from App.tsx
 */
export function useProviderActions(
  activeApp: AppId,
  isProxyRunning?: boolean,
  isProxyTakeover?: boolean,
) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const addProviderMutation = useAddProviderMutation(activeApp);
  const updateProviderMutation = useUpdateProviderMutation(activeApp);
  const deleteProviderMutation = useDeleteProviderMutation(activeApp);
  const switchProviderMutation = useSwitchProviderMutation(activeApp);

  // Claude 插件同步逻辑
  const syncClaudePlugin = useCallback(
    async (provider: Provider) => {
      if (activeApp !== "claude") return;

      try {
        const settings = await settingsApi.get();
        if (!settings?.enableClaudePluginIntegration) {
          return;
        }

        const isOfficial = provider.category === "official";
        await settingsApi.applyClaudePluginConfig({ official: isOfficial });

        // 静默执行，不显示成功通知
      } catch (error) {
        const detail =
          extractErrorMessage(error) ||
          t("notifications.syncClaudePluginFailed", {
            defaultValue: "同步 Claude 插件失败",
          });
        toast.error(detail, { duration: 4200 });
      }
    },
    [activeApp, t],
  );

  // 添加供应商
  const addProvider = useCallback(
    async (
      provider: Omit<Provider, "id"> & {
        providerKey?: string;
        providerKeys?: string[];
        addToLive?: boolean;
      },
    ) => {
      // 多 Key 输入：providerKeys 不属于 Provider 本体，先剥离，创建成功后组成 Key 池
      const { providerKeys, ...providerRest } = provider;
      const enhanced = injectOfficialSubscriptionUsageScript(
        activeApp,
        injectCodingPlanUsageScript(activeApp, providerRest),
      );
      const created = await addProviderMutation.mutateAsync(enhanced);

      // 创建成功后把全部 Key 写入该供应商的 Key 池（首个 Key 已在配置里）
      if (providerKeys && providerKeys.length > 1 && created?.id) {
        try {
          const seededKeys: ProviderKey[] = [];
          for (const [index, keyValue] of providerKeys.entries()) {
            const key = await providersApi.addKey(activeApp, created.id, {
              name: `Key ${index + 1}`,
              keyValue,
              enabled: true,
              priority: index,
              weight: 1,
            });
            seededKeys.push(key);
          }
          // sub2api 自动探测：命中结构则自动启用用量查询。
          // 创建流程不阻塞、不弹独立 toast——后台并发探测，下次打开 Key 池即可见。
          void Promise.allSettled(
            seededKeys.map((key) =>
              autoConfigureSub2apiUsage(
                created,
                key.id,
                key.keyValue,
                activeApp,
              ),
            ),
          ).then((detections) => {
            const autoEnabledCount = detections.filter(
              (d) => d.status === "fulfilled" && d.value,
            ).length;
            if (autoEnabledCount === 0) return;

            void queryClient.invalidateQueries({
              queryKey: ["providerKeySummaries", activeApp],
            });
            void queryClient.invalidateQueries({
              queryKey: ["usage", "aggregated", created.id, activeApp],
            });
          });
          toast.success(
            t("providerKeys.poolSeeded", {
              count: providerKeys.length,
              defaultValue: "已创建 Key 池（{{count}} 个 Key）",
            }),
            { closeButton: true },
          );
        } catch (error) {
          console.error("Failed to seed provider key pool:", error);
          toast.warning(
            t("providerKeys.poolSeedFailed", {
              defaultValue: "供应商已创建，但部分 Key 写入 Key 池失败",
            }),
            { closeButton: true },
          );
        }
      } else if (created && created.id && created.category !== "official") {
        // 单 Key / 内嵌 Key 新建：首个 Key 已写入 config，由后端同步成 Key 池条目，
        // 不经上面的播种循环。这里拉取同步出的条目做 sub2api 自动探测，与多 Key
        // 路径保持一致（官方供应商已自动启用订阅用量，无需探测）。
        const createdId = created.id;
        void (async () => {
          try {
            const seededKeys = await providersApi.getKeys(activeApp, createdId);
            const detections = await Promise.allSettled(
              seededKeys
                .filter((key) => key.enabled && !key.usageScript)
                .map((key) =>
                  autoConfigureSub2apiUsage(
                    created,
                    key.id,
                    key.keyValue,
                    activeApp,
                  ),
                ),
            );
            const autoEnabledCount = detections.filter(
              (d) => d.status === "fulfilled" && d.value,
            ).length;
            if (autoEnabledCount === 0) return;
            void queryClient.invalidateQueries({
              queryKey: ["providerKeySummaries", activeApp],
            });
            void queryClient.invalidateQueries({
              queryKey: ["usage", "aggregated", createdId, activeApp],
            });
          } catch (error) {
            console.error(
              "Failed to auto-detect sub2api usage for embedded key:",
              error,
            );
          }
        })();
      }
    },
    [addProviderMutation, activeApp, queryClient, t],
  );

  // 更新供应商
  const updateProvider = useCallback(
    async (provider: Provider, originalId?: string) => {
      await updateProviderMutation.mutateAsync({ provider, originalId });

      // 更新托盘菜单（失败不影响主操作）
      try {
        await providersApi.updateTrayMenu();
      } catch (trayError) {
        console.error(
          "Failed to update tray menu after updating provider",
          trayError,
        );
      }
    },
    [updateProviderMutation],
  );

  // 切换供应商
  const switchProvider = useCallback(
    async (provider: Provider) => {
      const isCodexChatFormat =
        activeApp === "codex" &&
        (provider.meta?.apiFormat === "openai_chat" ||
          (typeof (provider.settingsConfig as Record<string, any>)?.config ===
            "string" &&
            isCodexChatWireApi(
              extractCodexWireApi(
                (provider.settingsConfig as Record<string, any>).config,
              ),
            )));

      // Determine why this provider requires the proxy
      let proxyRequiredReason: string | null = null;
      if (!isProxyRunning && provider.category !== "official") {
        if (
          provider.meta?.apiFormat === "openai_chat" &&
          activeApp === "claude"
        ) {
          proxyRequiredReason = t("notifications.proxyReasonOpenAIChat", {
            defaultValue: "使用 OpenAI Chat 接口格式",
          });
        } else if (
          provider.meta?.apiFormat === "openai_responses" &&
          activeApp === "claude"
        ) {
          proxyRequiredReason = t("notifications.proxyReasonOpenAIResponses", {
            defaultValue: "使用 OpenAI Responses 接口格式",
          });
        } else if (isCodexChatFormat) {
          proxyRequiredReason = t("notifications.proxyReasonOpenAIChat", {
            defaultValue: "使用 OpenAI Chat 接口格式",
          });
        } else if (
          provider.meta?.isFullUrl &&
          (activeApp === "claude" || activeApp === "codex")
        ) {
          proxyRequiredReason = t("notifications.proxyReasonFullUrl", {
            defaultValue: "开启了完整 URL 连接模式",
          });
        }
      }

      if (proxyRequiredReason) {
        toast.warning(
          t("notifications.proxyRequiredForSwitch", {
            reason: proxyRequiredReason,
            defaultValue:
              "此供应商{{reason}}，需要代理服务才能正常使用，请先启动代理",
          }),
        );
      }

      // Block official providers when proxy takeover is active
      if (isProxyTakeover && provider.category === "official") {
        toast.error(
          t("notifications.officialBlockedByProxy", {
            defaultValue:
              "代理接管模式下不能切换到官方供应商，使用代理访问官方 API 可能导致账号被封禁",
          }),
          { duration: 6000 },
        );
        return;
      }

      try {
        const result = await switchProviderMutation.mutateAsync(provider.id);
        await syncClaudePlugin(provider);

        // Show backfill warning if present
        if (result?.warnings?.length) {
          toast.warning(
            t("notifications.backfillWarning", {
              defaultValue:
                "切换成功，但旧供应商配置回填失败，您手动修改的配置可能未保存",
            }),
            { duration: 5000 },
          );
        }

        // 若已弹过 proxyRequired 警告则不再弹 success
        if (!proxyRequiredReason) {
          let messageKey = "notifications.switchSuccess";
          let defaultMessage = "切换成功！";
          if (activeApp === "codex") {
            messageKey = "notifications.codexRestartRequired";
            defaultMessage = "切换成功，请重启客户端以生效";
          } else if (activeApp === "opencode") {
            messageKey = "notifications.addToConfigSuccess";
            defaultMessage = "已添加到配置";
          }
          toast.success(t(messageKey, { defaultValue: defaultMessage }), {
            closeButton: true,
          });
        }
      } catch {
        // 错误提示由 mutation 处理
      }
    },
    [
      switchProviderMutation,
      syncClaudePlugin,
      activeApp,
      isProxyRunning,
      isProxyTakeover,
      t,
    ],
  );

  // 删除供应商
  const deleteProvider = useCallback(
    async (id: string) => {
      await deleteProviderMutation.mutateAsync(id);
    },
    [deleteProviderMutation],
  );

  // 保存用量脚本
  const saveUsageScript = useCallback(
    async (provider: Provider, script: UsageScript) => {
      try {
        const updatedProvider: Provider = {
          ...provider,
          meta: {
            ...provider.meta,
            usage_script: script,
          },
        };

        await providersApi.update(updatedProvider, activeApp);
        await queryClient.invalidateQueries({
          queryKey: ["providers", activeApp],
        });
        // 🔧 保存用量脚本后，也应该失效该 provider 的用量查询缓存
        // 这样主页列表会使用新配置重新查询，而不是使用测试时的缓存
        await queryClient.invalidateQueries({
          queryKey: ["usage", provider.id, activeApp],
        });
        await queryClient.invalidateQueries({
          queryKey: ["subscription", "quota", activeApp],
        });
        toast.success(
          t("provider.usageSaved", {
            defaultValue: "用量查询配置已保存",
          }),
          { closeButton: true },
        );
      } catch (error) {
        const detail =
          extractErrorMessage(error) ||
          t("provider.usageSaveFailed", {
            defaultValue: "用量查询配置保存失败",
          });
        toast.error(detail);
      }
    },
    [activeApp, queryClient, t],
  );

  return {
    addProvider,
    updateProvider,
    switchProvider,
    deleteProvider,
    saveUsageScript,
    isLoading:
      addProviderMutation.isPending ||
      updateProviderMutation.isPending ||
      deleteProviderMutation.isPending ||
      switchProviderMutation.isPending,
  };
}
