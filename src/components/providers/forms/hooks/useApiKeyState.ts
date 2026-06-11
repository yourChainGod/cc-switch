import { useEffect, useState, useCallback } from "react";
import type { ProviderCategory } from "@/types";
import {
  getApiKeyFromConfig,
  setApiKeyInConfig,
  hasApiKeyField,
  splitApiKeys,
} from "@/utils/providerConfigUtils";

interface UseApiKeyStateProps {
  initialConfig?: string;
  /** 读取表单当前配置；handleApiKeyChange 必须基于实时值打补丁，
   *  否则会用渲染时的过期快照整体覆盖用户刚编辑的 JSON */
  getConfig?: () => string;
  onConfigChange: (config: string) => void;
  selectedPresetId: string | null;
  category?: ProviderCategory;
  appType?: string;
  apiKeyField?: string;
}

/**
 * 管理 API Key 输入状态
 * 自动同步 API Key 和 JSON 配置
 */
export function useApiKeyState({
  initialConfig,
  getConfig,
  onConfigChange,
  selectedPresetId,
  category,
  appType,
  apiKeyField,
}: UseApiKeyStateProps) {
  const [apiKey, setApiKey] = useState(() => {
    if (initialConfig) {
      return getApiKeyFromConfig(initialConfig, appType);
    }
    return "";
  });

  // 当外部通过 form.reset / 读取 live 等方式更新配置时，同步回 API Key 状态
  // - 仅在 JSON 可解析时同步，避免用户编辑 JSON 过程中因临时无效导致输入框闪烁
  useEffect(() => {
    if (!initialConfig) return;

    try {
      JSON.parse(initialConfig);
    } catch {
      return;
    }

    // 从配置中提取 API Key（如果不存在则返回空字符串）
    // 多 Key 输入时配置里只写首个 Key，此时不要用首 Key 回写覆盖输入框里的完整列表
    const extracted = getApiKeyFromConfig(initialConfig, appType);
    const firstOfCurrent = splitApiKeys(apiKey)[0] ?? "";
    if (extracted !== apiKey && extracted !== firstOfCurrent) {
      setApiKey(extracted);
    }
  }, [initialConfig, appType, apiKey]);

  const handleApiKeyChange = useCallback(
    (key: string) => {
      setApiKey(key);

      // Claude 支持一次粘贴多个 Key（空格/逗号/换行分隔）：
      // 配置里只写首个 Key，完整列表由表单提交时组成 Key 池
      const keys = appType === "claude" ? splitApiKeys(key) : [key.trim()];
      const configKey = keys[0] ?? "";

      // 基于表单实时配置打补丁，保留用户已编辑的其他字段
      const currentConfig = getConfig?.() ?? initialConfig ?? "{}";
      const configString = setApiKeyInConfig(currentConfig || "{}", configKey, {
          // 最佳实践：仅在"新增模式"且"非官方类别"时补齐缺失字段
          // - 新增模式：selectedPresetId !== null
          // - 非官方类别：category !== undefined && category !== "official"
          // - 官方类别：不创建字段（UI 也会禁用输入框）
          // - 未传入 category：不创建字段（避免意外行为）
          createIfMissing:
            selectedPresetId !== null &&
            category !== undefined &&
            category !== "official",
          appType,
          apiKeyField,
        },
      );

      onConfigChange(configString);
    },
    [
      initialConfig,
      getConfig,
      selectedPresetId,
      category,
      appType,
      apiKeyField,
      onConfigChange,
    ],
  );

  const showApiKey = useCallback(
    (config: string, isEditMode: boolean) => {
      return (
        selectedPresetId !== null ||
        (isEditMode && hasApiKeyField(config, appType))
      );
    },
    [selectedPresetId, appType],
  );

  return {
    apiKey,
    setApiKey,
    handleApiKeyChange,
    showApiKey,
  };
}
