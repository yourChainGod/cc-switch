import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { Save } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { FullScreenPanel } from "@/components/common/FullScreenPanel";
import UsageScriptModal from "@/components/UsageScriptModal";
import type { Provider, ProviderKey, UsageScript } from "@/types";
import {
  ProviderForm,
  type ProviderConfigKeyPatch,
  type ProviderFormValues,
} from "@/components/providers/forms/ProviderForm";
import {
  KeyPoolEntryContext,
  type KeyPoolEntryValue,
} from "@/components/providers/keyPool/KeyPoolEntryContext";
import {
  ProviderKeyPoolDialog,
  type ProviderKeyPoolController,
} from "@/components/providers/keyPool/ProviderKeyPoolDialog";
import { providersApi, vscodeApi, type AppId } from "@/lib/api";
import { usageApi } from "@/lib/api/usage";
import { autoConfigureSub2apiUsage } from "@/lib/usage/autoDetectSub2api";
import { isAdditiveApp } from "@/config/additiveApps";

interface EditProviderDialogProps {
  open: boolean;
  provider: Provider | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (payload: {
    provider: Provider;
    originalId?: string;
  }) => Promise<void> | void;
  appId: AppId;
  isProxyTakeover?: boolean; // 代理接管模式下不读取 live（避免显示被接管后的代理配置）
}

export function EditProviderDialog({
  open,
  provider,
  onOpenChange,
  onSubmit,
  appId,
  isProxyTakeover = false,
}: EditProviderDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [currentProvider, setCurrentProvider] = useState(provider);
  const [isFormSubmitting, setIsFormSubmitting] = useState(false);
  const [providerKeys, setProviderKeys] = useState<ProviderKey[]>([]);
  const [isKeysLoading, setIsKeysLoading] = useState(false);
  const [isKeysSaving, setIsKeysSaving] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const [isKeyPoolOpen, setIsKeyPoolOpen] = useState(false);
  // key 级用量配置：当前正在配置用量脚本的 key（null 表示弹窗关闭）
  const [usageKey, setUsageKey] = useState<ProviderKey | null>(null);
  const [configKeyPatch, setConfigKeyPatch] =
    useState<ProviderConfigKeyPatch | null>(null);
  const configKeyPatchSeqRef = useRef(0);

  // 默认使用传入的 provider.settingsConfig，若当前编辑对象是"当前生效供应商"，则尝试读取实时配置替换初始值
  const [liveSettings, setLiveSettings] = useState<Record<
    string,
    unknown
  > | null>(null);

  // 使用 ref 标记是否已经加载过，防止重复读取覆盖用户编辑
  const [hasLoadedLive, setHasLoadedLive] = useState(false);

  useEffect(() => {
    if (open) {
      setCurrentProvider(provider);
      setConfigKeyPatch(null);
    } else {
      setIsKeyPoolOpen(false);
    }
  }, [open, provider]);

  const activeProvider = currentProvider ?? provider;
  const activeProviderId = activeProvider?.id;

  const queueConfigKeyPatch = useCallback(
    (
      updatedProvider: Provider,
      explicitKey?: Pick<ProviderKey, "keyValue" | "authField"> | null,
    ) => {
      let keyValue = explicitKey?.keyValue;
      let authField = explicitKey?.authField;

      if (keyValue === undefined) {
        const embedded = extractEmbeddedProviderKey(
          appId,
          updatedProvider.settingsConfig,
          updatedProvider.meta,
        );
        keyValue = embedded?.value;
        authField = authField ?? embedded?.authField;
      }

      if (typeof keyValue !== "string") return;

      setConfigKeyPatch({
        id: ++configKeyPatchSeqRef.current,
        keyValue,
        authField,
      });
    },
    [appId],
  );

  const applyKeyPoolProviderUpdate = useCallback(
    (
      updatedProvider: Provider,
      explicitKey?: Pick<ProviderKey, "keyValue" | "authField"> | null,
    ) => {
      const shouldPatchConfigKey =
        explicitKey !== undefined ||
        hasConfigKeyBindingChanged(appId, activeProvider, updatedProvider);

      setLiveSettings(null);
      setCurrentProvider((previous) => {
        if (!previous || previous.id !== updatedProvider.id) {
          return updatedProvider;
        }

        return {
          ...previous,
          meta: updatedProvider.meta,
          inFailoverQueue: updatedProvider.inFailoverQueue,
        };
      });
      if (shouldPatchConfigKey) {
        queueConfigKeyPatch(updatedProvider, explicitKey);
      }
    },
    [activeProvider, appId, queueConfigKeyPatch],
  );

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      if (!open || !provider) {
        setLiveSettings(null);
        setHasLoadedLive(false);
        return;
      }

      // 关键修复：只在首次打开时加载一次
      if (hasLoadedLive) {
        return;
      }

      // 代理接管模式：Live 配置已被代理改写，读取 live 会导致编辑界面展示代理地址/占位符等内容
      // 因此直接回退到 SSOT（数据库）配置，避免用户困惑与误保存
      if (isProxyTakeover) {
        if (!cancelled) {
          setLiveSettings(null);
          setHasLoadedLive(true);
        }
        return;
      }

      // OpenCode uses additive mode - each provider's config is stored independently in DB
      // Reading live config would return the full opencode.json (with $schema, provider, mcp etc.)
      // instead of just the provider fragment, causing incorrect nested structure on save
      if (appId === "opencode") {
        if (!cancelled) {
          setLiveSettings(null);
          setHasLoadedLive(true);
        }
        return;
      }

      try {
        const currentId = await providersApi.getCurrent(appId);
        if (currentId && provider.id === currentId) {
          try {
            const live = (await vscodeApi.getLiveProviderSettings(
              appId,
            )) as Record<string, unknown>;
            if (!cancelled && live && typeof live === "object") {
              setLiveSettings(live);
              setHasLoadedLive(true);
            }
          } catch {
            // 读取实时配置失败则回退到 SSOT（不打断编辑流程）
            if (!cancelled) {
              setLiveSettings(null);
              setHasLoadedLive(true);
            }
          }
        } else {
          if (!cancelled) {
            setLiveSettings(null);
            setHasLoadedLive(true);
          }
        }
      } finally {
        // no-op
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [open, provider?.id, appId, hasLoadedLive, isProxyTakeover]); // 只依赖 provider.id，不依赖整个 provider 对象

  const refreshActiveProvider = useCallback(async () => {
    if (!open || !activeProviderId) {
      return null;
    }

    try {
      const refreshed = await providersApi.get(appId, activeProviderId);
      if (refreshed) {
        applyKeyPoolProviderUpdate(refreshed);
      }
      return refreshed;
    } catch (error) {
      console.error("Failed to refresh provider:", error);
      return null;
    }
  }, [appId, open, activeProviderId, applyKeyPoolProviderUpdate]);

  const loadProviderKeys = useCallback(async () => {
    if (!open || !activeProviderId) {
      setProviderKeys([]);
      setKeyDraft("");
      return;
    }
    setIsKeysLoading(true);
    try {
      const keys = await providersApi.getKeys(appId, activeProviderId);
      setProviderKeys(keys);
    } catch (error) {
      console.error("Failed to load provider keys:", error);
      toast.error(
        t("providerKeys.loadFailed", {
          defaultValue: "Failed to load provider keys",
        }),
      );
    } finally {
      setIsKeysLoading(false);
    }
  }, [appId, open, activeProviderId, t]);

  const refreshProviderKeySummaries = useCallback(
    () =>
      queryClient.invalidateQueries({
        queryKey: ["providerKeySummaries", appId],
      }),
    [appId, queryClient],
  );

  useEffect(() => {
    void loadProviderKeys();
  }, [loadProviderKeys]);

  const initialSettingsConfig = useMemo(() => {
    const base = (liveSettings ??
      activeProvider?.settingsConfig ??
      {}) as Record<string, unknown>;

    // Codex 的 modelCatalog 是 cc-switch 私有字段，SSOT 在数据库。Live 的 config.toml
    // 仅在写入时投影出 model_catalog_json 指针；Codex.app 改写配置、代理接管/恢复周期、
    // 来回切换供应商都可能让 Live 丢失该投影，从而 read_live_settings 反解为空。
    // 若放任 Live 覆盖，编辑界面会显示空映射表，保存后连同数据库里的映射一起清空（数据丢失）。
    // 因此始终以数据库 SSOT 的 modelCatalog 为准，仅在数据库确实没有时才回退到 Live 反解结果。
    if (
      appId === "codex" &&
      liveSettings &&
      activeProvider?.settingsConfig &&
      typeof activeProvider.settingsConfig === "object"
    ) {
      const dbCatalog = (
        activeProvider.settingsConfig as Record<string, unknown>
      ).modelCatalog;
      if (dbCatalog !== undefined) {
        return { ...base, modelCatalog: dbCatalog };
      }
    }

    return base;
  }, [liveSettings, activeProvider?.settingsConfig, appId]); // 只依赖 settingsConfig，不依赖整个 provider

  // 固定 initialData，防止 provider 对象更新时重置表单
  const initialData = useMemo(() => {
    if (!activeProvider) return null;
    return {
      name: activeProvider.name,
      notes: activeProvider.notes,
      websiteUrl: activeProvider.websiteUrl,
      settingsConfig: initialSettingsConfig,
      category: activeProvider.category,
      meta: activeProvider.meta,
      icon: activeProvider.icon,
      iconColor: activeProvider.iconColor,
    };
  }, [
    open, // 修复：编辑保存后再次打开显示旧数据，依赖 open 确保每次打开时重新读取最新 provider 数据
    activeProvider?.id, // 只依赖 ID，provider 对象更新不会触发重新计算
    activeProvider?.name,
    activeProvider?.notes,
    activeProvider?.websiteUrl,
    activeProvider?.category,
    activeProvider?.icon,
    activeProvider?.iconColor,
    initialSettingsConfig,
  ]);

  const embeddedKey = useMemo(
    () =>
      extractEmbeddedProviderKey(
        appId,
        initialSettingsConfig,
        activeProvider?.meta,
      ),
    [appId, initialSettingsConfig, activeProvider?.meta],
  );

  const canImportEmbeddedKey =
    providerKeys.length === 0 && embeddedKey !== null && !isKeysLoading;

  const configKeyId = activeProvider?.meta?.configKeyId;
  const effectiveConfigKeyMode: "auto" | "manual" =
    activeProvider?.meta?.configKeyMode ?? (configKeyId ? "manual" : "auto");
  const effectiveConfigKeyId = useMemo(() => {
    if (configKeyId) return configKeyId;
    const embeddedValue = embeddedKey?.value;
    if (!embeddedValue) return null;
    return (
      providerKeys.find((key) => key.keyValue === embeddedValue)?.id ?? null
    );
  }, [configKeyId, embeddedKey?.value, providerKeys]);

  const handleSetConfigKey = useCallback(
    async (key: ProviderKey) => {
      if (!activeProvider) return;
      setIsKeysSaving(true);
      try {
        const updatedProvider = await providersApi.setConfigKey(
          appId,
          activeProvider.id,
          key.id,
        );
        applyKeyPoolProviderUpdate(updatedProvider, key);
        await loadProviderKeys();
        await refreshProviderKeySummaries();
        toast.success(
          t("providerKeys.configKeySet", {
            defaultValue: "Configuration key updated",
          }),
          { closeButton: true },
        );
      } catch (error) {
        console.error("Failed to set provider config key:", error);
        toast.error(
          t("providerKeys.configKeySetFailed", {
            defaultValue: "Failed to update configuration key",
          }),
        );
      } finally {
        setIsKeysSaving(false);
      }
    },
    [
      activeProvider,
      appId,
      applyKeyPoolProviderUpdate,
      loadProviderKeys,
      refreshProviderKeySummaries,
      t,
    ],
  );

  const handleSetConfigKeyAuto = useCallback(async () => {
    if (!activeProvider) return;
    setIsKeysSaving(true);
    try {
      const updatedProvider = await providersApi.setConfigKeyAuto(
        appId,
        activeProvider.id,
      );
      applyKeyPoolProviderUpdate(updatedProvider);
      await loadProviderKeys();
      await refreshProviderKeySummaries();
      toast.success(
        t("providerKeys.configKeyAutoSet", {
          defaultValue: "Configuration key follows priority",
        }),
        { closeButton: true },
      );
    } catch (error) {
      console.error("Failed to set provider config key auto mode:", error);
      toast.error(
        t("providerKeys.configKeySetFailed", {
          defaultValue: "Failed to update configuration key",
        }),
      );
    } finally {
      setIsKeysSaving(false);
    }
  }, [
    activeProvider,
    appId,
    applyKeyPoolProviderUpdate,
    loadProviderKeys,
    refreshProviderKeySummaries,
    t,
  ]);

  const handleSubmit = useCallback(
    async (values: ProviderFormValues) => {
      if (!activeProvider) return;

      // 注意：values.settingsConfig 已经是最终的配置字符串
      // ProviderForm 已经为不同的 app 类型（Claude/Codex/Gemini）正确组装了配置
      const parsedConfig = JSON.parse(values.settingsConfig) as Record<
        string,
        unknown
      >;
      // providerKey 型应用（OpenCode）：providerKey 即主键 ID，
      // 编辑未锁定的供应商时允许通过修改 providerKey 重命名 ID
      const nextProviderId =
        isAdditiveApp(appId) && values.providerKey?.trim()
          ? values.providerKey.trim()
          : activeProvider.id;

      const updatedProvider: Provider = {
        ...activeProvider,
        id: nextProviderId,
        name: values.name.trim(),
        notes: values.notes?.trim() || undefined,
        websiteUrl: values.websiteUrl?.trim() || undefined,
        settingsConfig: parsedConfig,
        icon: values.icon?.trim() || undefined,
        iconColor: values.iconColor?.trim() || undefined,
        ...(values.presetCategory ? { category: values.presetCategory } : {}),
        // 保留或更新 meta 字段
        ...(values.meta ? { meta: values.meta } : {}),
      };

      await onSubmit({
        provider: updatedProvider,
        originalId: activeProvider.id,
      });
      onOpenChange(false);
    },
    [appId, onSubmit, onOpenChange, activeProvider],
  );

  const handleAddKeys = useCallback(async () => {
    if (!activeProvider) return;
    const values = keyDraft
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
    const uniqueValues = Array.from(new Set(values));
    if (uniqueValues.length === 0) return;

    setIsKeysSaving(true);
    try {
      const created: ProviderKey[] = [];
      for (const [index, value] of uniqueValues.entries()) {
        const key = await providersApi.addKey(appId, activeProvider.id, {
          name: `Key ${providerKeys.length + index + 1}`,
          keyValue: value,
          enabled: true,
          priority: 0,
          weight: 1,
        });
        created.push(key);
      }
      setKeyDraft("");
      // sub2api 自动探测：命中结构则自动为该 key 启用用量查询。
      // 须先于 loadProviderKeys 完成，刷新后 key 列表即可显示已启用。
      const detections = await Promise.allSettled(
        created.map((key) =>
          autoConfigureSub2apiUsage(
            activeProvider,
            key.id,
            key.keyValue,
            appId,
          ),
        ),
      );
      const autoEnabledCount = detections.filter(
        (d) => d.status === "fulfilled" && d.value,
      ).length;
      await refreshActiveProvider();
      await loadProviderKeys();
      await refreshProviderKeySummaries();
      if (autoEnabledCount > 0) {
        await queryClient.invalidateQueries({
          queryKey: ["usage", "aggregated", activeProvider.id, appId],
        });
      }
      toast.success(
        t("providerKeys.added", {
          count: uniqueValues.length,
          defaultValue: "Provider keys added",
        }),
      );
      if (autoEnabledCount > 0) {
        toast.success(
          t("usage.sub2apiAutoEnabled", {
            defaultValue: "已自动启用 sub2api 用量查询",
          }),
        );
      }
    } catch (error) {
      console.error("Failed to add provider keys:", error);
      toast.error(
        t("providerKeys.addFailed", {
          defaultValue: "Failed to add provider keys",
        }),
      );
    } finally {
      setIsKeysSaving(false);
    }
  }, [
    activeProvider,
    appId,
    keyDraft,
    loadProviderKeys,
    providerKeys.length,
    queryClient,
    refreshActiveProvider,
    refreshProviderKeySummaries,
    t,
  ]);

  const handleImportEmbeddedKey = useCallback(async () => {
    if (!activeProvider || !embeddedKey) return;

    setIsKeysSaving(true);
    try {
      const key = await providersApi.addKey(appId, activeProvider.id, {
        name: t("providerKeys.importedName", {
          defaultValue: "Imported key",
        }),
        keyValue: embeddedKey.value,
        authField: embeddedKey.authField,
        enabled: true,
        priority: 0,
        weight: 1,
      });
      // sub2api 自动探测：命中结构则自动为该 key 启用用量查询（先于 loadProviderKeys）
      const autoEnabled = await autoConfigureSub2apiUsage(
        activeProvider,
        key.id,
        key.keyValue,
        appId,
      );
      await refreshActiveProvider();
      await loadProviderKeys();
      await refreshProviderKeySummaries();
      if (autoEnabled) {
        await queryClient.invalidateQueries({
          queryKey: ["usage", "aggregated", activeProvider.id, appId],
        });
      }
      toast.success(
        t("providerKeys.imported", {
          defaultValue: "Embedded key imported",
        }),
      );
      if (autoEnabled) {
        toast.success(
          t("usage.sub2apiAutoEnabled", {
            defaultValue: "已自动启用 sub2api 用量查询",
          }),
        );
      }
    } catch (error) {
      console.error("Failed to import embedded provider key:", error);
      toast.error(
        t("providerKeys.importFailed", {
          defaultValue: "Failed to import embedded key",
        }),
      );
    } finally {
      setIsKeysSaving(false);
    }
  }, [
    activeProvider,
    appId,
    embeddedKey,
    loadProviderKeys,
    queryClient,
    refreshActiveProvider,
    refreshProviderKeySummaries,
    t,
  ]);

  const handleToggleKey = useCallback(
    async (key: ProviderKey, enabled: boolean) => {
      if (!activeProvider) return;
      setIsKeysSaving(true);
      try {
        await providersApi.updateKey(appId, activeProvider.id, key.id, {
          name: key.name,
          keyValue: key.keyValue,
          authField: key.authField,
          enabled,
          priority: key.priority,
          weight: key.weight,
        });
        await refreshActiveProvider();
        await loadProviderKeys();
        await refreshProviderKeySummaries();
      } catch (error) {
        console.error("Failed to update provider key:", error);
        toast.error(
          t("providerKeys.updateFailed", {
            defaultValue: "Failed to update provider key",
          }),
        );
      } finally {
        setIsKeysSaving(false);
      }
    },
    [
      activeProvider,
      appId,
      loadProviderKeys,
      refreshActiveProvider,
      refreshProviderKeySummaries,
      t,
    ],
  );

  const handleUpdateKeySchedule = useCallback(
    async (
      key: ProviderKey,
      updates: Partial<Pick<ProviderKey, "priority" | "weight">>,
    ) => {
      if (!activeProvider) return;
      const priority = updates.priority ?? key.priority;
      const weight = Math.max(1, updates.weight ?? key.weight);
      if (priority === key.priority && weight === key.weight) return;

      setIsKeysSaving(true);
      try {
        await providersApi.updateKey(appId, activeProvider.id, key.id, {
          name: key.name,
          keyValue: key.keyValue,
          authField: key.authField,
          enabled: key.enabled,
          priority,
          weight,
        });
        await refreshActiveProvider();
        await loadProviderKeys();
        await refreshProviderKeySummaries();
      } catch (error) {
        console.error("Failed to update provider key schedule:", error);
        toast.error(
          t("providerKeys.updateFailed", {
            defaultValue: "Failed to update provider key",
          }),
        );
      } finally {
        setIsKeysSaving(false);
      }
    },
    [
      activeProvider,
      appId,
      loadProviderKeys,
      refreshActiveProvider,
      refreshProviderKeySummaries,
      t,
    ],
  );

  const handleDeleteKey = useCallback(
    async (key: ProviderKey) => {
      if (!activeProvider) return;
      // 确认交互由 ProviderKeyPoolDialog 的 ConfirmDialog 完成后才会调用到这里
      setIsKeysSaving(true);
      try {
        await providersApi.deleteKey(appId, activeProvider.id, key.id);
        await refreshActiveProvider();
        await loadProviderKeys();
        await refreshProviderKeySummaries();
      } catch (error) {
        console.error("Failed to delete provider key:", error);
        toast.error(
          t("providerKeys.deleteFailed", {
            defaultValue: "Failed to delete provider key",
          }),
        );
      } finally {
        setIsKeysSaving(false);
      }
    },
    [
      activeProvider,
      appId,
      loadProviderKeys,
      refreshActiveProvider,
      refreshProviderKeySummaries,
      t,
    ],
  );

  const handleResetKey = useCallback(
    async (key: ProviderKey) => {
      if (!activeProvider) return;
      setIsKeysSaving(true);
      try {
        await providersApi.resetKeyHealth(appId, activeProvider.id, key.id);
        await loadProviderKeys();
        await refreshProviderKeySummaries();
      } catch (error) {
        console.error("Failed to reset provider key:", error);
        toast.error(
          t("providerKeys.resetFailed", {
            defaultValue: "Failed to reset provider key",
          }),
        );
      } finally {
        setIsKeysSaving(false);
      }
    },
    [activeProvider, appId, loadProviderKeys, refreshProviderKeySummaries, t],
  );

  const handleResetAllKeys = useCallback(async () => {
    if (!activeProvider) return;
    setIsKeysSaving(true);
    try {
      const count = await providersApi.resetAllKeysHealth(
        appId,
        activeProvider.id,
      );
      await loadProviderKeys();
      await refreshProviderKeySummaries();
      toast.success(
        t("providerKeys.resetAllDone", {
          count,
          defaultValue: "Reset health state for {{count}} keys",
        }),
      );
    } catch (error) {
      console.error("Failed to reset provider keys:", error);
      toast.error(
        t("providerKeys.resetFailed", {
          defaultValue: "Failed to reset provider key",
        }),
      );
    } finally {
      setIsKeysSaving(false);
    }
  }, [activeProvider, appId, loadProviderKeys, refreshProviderKeySummaries, t]);

  const keyIssueCount = useMemo(
    () =>
      providerKeys.filter((key) => !key.enabled || key.status !== "active")
        .length,
    [providerKeys],
  );

  const keyPoolEntry = useMemo<KeyPoolEntryValue>(
    () => ({
      total: providerKeys.length,
      available: providerKeys.length - keyIssueCount,
      issues: keyIssueCount,
      configKeyMode: effectiveConfigKeyMode,
      isLoading: isKeysLoading,
      open: () => setIsKeyPoolOpen(true),
    }),
    [providerKeys.length, keyIssueCount, effectiveConfigKeyMode, isKeysLoading],
  );

  const handleSaveKeyUsage = useCallback(
    async (key: ProviderKey, script: UsageScript) => {
      try {
        await usageApi.setKeyUsageScript(key.providerId, key.id, appId, script);
        await loadProviderKeys();
        // 失效聚合用量（卡片求和）与该 key 的单独用量缓存
        await queryClient.invalidateQueries({
          queryKey: ["usage", "aggregated", key.providerId, appId],
        });
        await queryClient.invalidateQueries({
          queryKey: ["usage", "key", key.providerId, key.id, appId],
        });
        toast.success(
          t("provider.usageSaved", { defaultValue: "用量查询配置已保存" }),
          { closeButton: true },
        );
      } catch (error) {
        console.error("Failed to save key usage script:", error);
        toast.error(
          t("provider.usageSaveFailed", {
            defaultValue: "用量查询配置保存失败",
          }),
        );
      }
    },
    [appId, loadProviderKeys, queryClient, t],
  );

  const keyPoolController = useMemo<ProviderKeyPoolController>(
    () => ({
      keys: providerKeys,
      isLoading: isKeysLoading,
      isSaving: isKeysSaving,
      draft: keyDraft,
      setDraft: setKeyDraft,
      effectiveConfigKeyId,
      effectiveConfigKeyMode,
      canImportEmbeddedKey,
      reload: () => void loadProviderKeys(),
      addKeys: () => void handleAddKeys(),
      importEmbeddedKey: () => void handleImportEmbeddedKey(),
      toggleKey: (key, enabled) => void handleToggleKey(key, enabled),
      updateKeySchedule: (key, updates) =>
        void handleUpdateKeySchedule(key, updates),
      deleteKey: (key) => void handleDeleteKey(key),
      resetKey: (key) => void handleResetKey(key),
      resetAllKeys: () => void handleResetAllKeys(),
      setConfigKey: (key) => void handleSetConfigKey(key),
      setConfigKeyAuto: () => void handleSetConfigKeyAuto(),
      // key 级用量配置弹窗（FullScreenPanel，portal 到 body）与 key 池
      // (Radix Dialog，带 focus-trap) 不能同时打开，否则两者抢焦点导致死循环。
      // 打开用量配置时先关 key 池，关闭时再重开（全屏面板本就盖住 key 池，视觉无损）。
      configureKeyUsage: (key) => {
        setIsKeyPoolOpen(false);
        setUsageKey(key);
      },
    }),
    [
      providerKeys,
      isKeysLoading,
      isKeysSaving,
      keyDraft,
      effectiveConfigKeyId,
      effectiveConfigKeyMode,
      canImportEmbeddedKey,
      loadProviderKeys,
      handleAddKeys,
      handleImportEmbeddedKey,
      handleToggleKey,
      handleUpdateKeySchedule,
      handleDeleteKey,
      handleResetKey,
      handleResetAllKeys,
      handleSetConfigKey,
      handleSetConfigKeyAuto,
    ],
  );

  if (!activeProvider || !initialData) {
    return null;
  }

  return (
    <FullScreenPanel
      isOpen={open}
      title={t("provider.editProvider")}
      onClose={() => onOpenChange(false)}
      footer={
        <Button
          type="submit"
          form="provider-form"
          disabled={isFormSubmitting}
          className="bg-primary text-primary-foreground hover:bg-primary/90"
        >
          <Save className="h-4 w-4 mr-2" />
          {t("common.save")}
        </Button>
      }
    >
      <KeyPoolEntryContext.Provider value={keyPoolEntry}>
        <ProviderForm
          appId={appId}
          providerId={activeProvider.id}
          submitLabel={t("common.save")}
          onSubmit={handleSubmit}
          onCancel={() => onOpenChange(false)}
          onSubmittingChange={setIsFormSubmitting}
          initialData={initialData}
          latestMeta={activeProvider.meta}
          configKeyPatch={configKeyPatch}
          showButtons={false}
          isProxyTakeover={isProxyTakeover}
        />
      </KeyPoolEntryContext.Provider>

      <ProviderKeyPoolDialog
        open={isKeyPoolOpen}
        onOpenChange={setIsKeyPoolOpen}
        providerName={activeProvider.name}
        pool={keyPoolController}
      />

      {usageKey && (
        <UsageScriptModal
          key={usageKey.id}
          provider={activeProvider}
          appId={appId}
          providerKey={usageKey}
          isOpen={Boolean(usageKey)}
          onClose={() => {
            setUsageKey(null);
            setIsKeyPoolOpen(true);
          }}
          onSave={(script) => void handleSaveKeyUsage(usageKey, script)}
        />
      )}
    </FullScreenPanel>
  );
}

type EmbeddedProviderKey = {
  authField: string;
  value: string;
};

function extractEmbeddedProviderKey(
  appId: AppId,
  settingsConfig: Record<string, unknown>,
  meta?: Provider["meta"],
): EmbeddedProviderKey | null {
  const candidateFields = buildCandidateAuthFields(appId, settingsConfig, meta);

  for (const authField of candidateFields) {
    const value = readAuthFieldValue(appId, settingsConfig, authField);
    if (isImportableKeyValue(value)) {
      return {
        authField,
        value,
      };
    }
  }

  return null;
}

function buildCandidateAuthFields(
  appId: AppId,
  settingsConfig: Record<string, unknown>,
  meta?: Provider["meta"],
): string[] {
  const fields = new Set<string>();
  const add = (field?: string) => {
    const trimmed = field?.trim();
    if (trimmed) fields.add(trimmed);
  };

  add(meta?.apiKeyField);

  const env = asRecord(settingsConfig.env);
  for (const field of [
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
  ]) {
    if (env && typeof env[field] === "string") {
      add(field);
    }
  }

  if (appId === "codex") {
    add("OPENAI_API_KEY");
  } else if (appId === "gemini") {
    add("GEMINI_API_KEY");
  } else if (appId === "opencode") {
    add("options.apiKey");
  } else {
    add("ANTHROPIC_AUTH_TOKEN");
    add("ANTHROPIC_API_KEY");
  }

  return Array.from(fields);
}

function readAuthFieldValue(
  appId: AppId,
  settingsConfig: Record<string, unknown>,
  authField: string,
): string | null {
  if (
    authField === "ANTHROPIC_AUTH_TOKEN" ||
    authField === "ANTHROPIC_API_KEY" ||
    authField === "OPENAI_API_KEY" ||
    authField === "GEMINI_API_KEY"
  ) {
    const auth =
      appId === "codex" && authField === "OPENAI_API_KEY"
        ? asRecord(settingsConfig.auth)
        : asRecord(settingsConfig.env);
    const value = auth?.[authField];
    return typeof value === "string" ? value : null;
  }

  if (authField.includes(".")) {
    return readNestedString(settingsConfig, authField.split("."));
  }

  const value = settingsConfig[authField];
  return typeof value === "string" ? value : null;
}

function readNestedString(
  value: Record<string, unknown>,
  path: string[],
): string | null {
  let current: unknown = value;
  for (const segment of path) {
    const record = asRecord(current);
    if (!record) return null;
    current = record[segment];
  }
  return typeof current === "string" ? current : null;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object"
    ? (value as Record<string, unknown>)
    : null;
}

function isImportableKeyValue(value: string | null): value is string {
  if (!value) return false;
  const trimmed = value.trim();
  return trimmed.length > 0 && trimmed !== "PROXY_MANAGED";
}

function hasConfigKeyBindingChanged(
  appId: AppId,
  previous: Provider | null | undefined,
  next: Provider,
): boolean {
  if (!previous || previous.id !== next.id) return true;
  const previousKey = extractEmbeddedProviderKey(
    appId,
    previous.settingsConfig,
    previous.meta,
  );
  const nextKey = extractEmbeddedProviderKey(
    appId,
    next.settingsConfig,
    next.meta,
  );

  return (
    previous.meta?.configKeyId !== next.meta?.configKeyId ||
    previous.meta?.configKeyMode !== next.meta?.configKeyMode ||
    previousKey?.authField !== nextKey?.authField ||
    previousKey?.value !== nextKey?.value
  );
}
