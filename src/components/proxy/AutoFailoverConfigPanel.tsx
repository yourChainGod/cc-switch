import { useState, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Save, Loader2, Info } from "lucide-react";
import { toast } from "sonner";
import { useAppProxyConfig, useUpdateAppProxyConfig } from "@/lib/query/proxy";

export interface AutoFailoverConfigPanelProps {
  appType: string;
  disabled?: boolean;
}

export function AutoFailoverConfigPanel({
  appType,
  disabled = false,
}: AutoFailoverConfigPanelProps) {
  const { t } = useTranslation();
  const { data: config, isLoading, error } = useAppProxyConfig(appType);
  const updateConfig = useUpdateAppProxyConfig();

  // 使用字符串状态以支持完全清空数字输入框
  const [formData, setFormData] = useState({
    maxRetries: "3",
    streamingFirstByteTimeout: "60",
    streamingIdleTimeout: "120",
    nonStreamingTimeout: "600",
  });
  const lastSyncedConfigRef = useRef<typeof config | null>(null);
  const [isDirty, setIsDirty] = useState(false);

  useEffect(() => {
    if (!config) return;
    // 用户已编辑且 config 是上次同步过的相同引用 → 不覆盖输入
    if (isDirty && lastSyncedConfigRef.current === config) return;
    setFormData({
      maxRetries: String(config.maxRetries),
      streamingFirstByteTimeout: String(config.streamingFirstByteTimeout),
      streamingIdleTimeout: String(config.streamingIdleTimeout),
      nonStreamingTimeout: String(config.nonStreamingTimeout),
    });
    lastSyncedConfigRef.current = config;
    setIsDirty(false);
  }, [config, isDirty]);

  const handleSave = async () => {
    if (!config) return;
    // 解析数字，返回 NaN 表示无效输入
    const parseNum = (val: string) => {
      const trimmed = val.trim();
      // 必须是纯数字
      if (!/^-?\d+$/.test(trimmed)) return NaN;
      return parseInt(trimmed);
    };

    // 定义各字段的有效范围
    const ranges = {
      maxRetries: { min: 0, max: 10 },
      streamingFirstByteTimeout: { min: 1, max: 120 },
      streamingIdleTimeout: { min: 0, max: 600 },
      nonStreamingTimeout: { min: 60, max: 1200 },
    };

    // 解析原始值
    const raw = {
      maxRetries: parseNum(formData.maxRetries),
      streamingFirstByteTimeout: parseNum(formData.streamingFirstByteTimeout),
      streamingIdleTimeout: parseNum(formData.streamingIdleTimeout),
      nonStreamingTimeout: parseNum(formData.nonStreamingTimeout),
    };

    // 校验是否超出范围（NaN 也视为无效）
    const errors: string[] = [];
    const checkRange = (
      value: number,
      range: { min: number; max: number },
      label: string,
    ) => {
      if (isNaN(value) || value < range.min || value > range.max) {
        errors.push(`${label}: ${range.min}-${range.max}`);
      }
    };

    checkRange(
      raw.maxRetries,
      ranges.maxRetries,
      t("proxy.autoFailover.maxRetries", "最大重试次数"),
    );
    checkRange(
      raw.streamingFirstByteTimeout,
      ranges.streamingFirstByteTimeout,
      t("proxy.autoFailover.streamingFirstByte", "流式首字节超时"),
    );
    checkRange(
      raw.streamingIdleTimeout,
      ranges.streamingIdleTimeout,
      t("proxy.autoFailover.streamingIdle", "流式静默超时"),
    );
    checkRange(
      raw.nonStreamingTimeout,
      ranges.nonStreamingTimeout,
      t("proxy.autoFailover.nonStreaming", "非流式超时"),
    );

    if (errors.length > 0) {
      toast.error(
        t("proxy.autoFailover.validationFailed", {
          fields: errors.join("; "),
          defaultValue: `以下字段超出有效范围: ${errors.join("; ")}`,
        }),
      );
      return;
    }

    try {
      // 熔断/冷却策略已内置（key 级指数退避），不再暴露配置项，原值透传
      await updateConfig.mutateAsync({
        appType,
        enabled: config.enabled,
        autoFailoverEnabled: config.autoFailoverEnabled,
        maxRetries: raw.maxRetries,
        streamingFirstByteTimeout: raw.streamingFirstByteTimeout,
        streamingIdleTimeout: raw.streamingIdleTimeout,
        nonStreamingTimeout: raw.nonStreamingTimeout,
        circuitFailureThreshold: config.circuitFailureThreshold,
        circuitSuccessThreshold: config.circuitSuccessThreshold,
        circuitTimeoutSeconds: config.circuitTimeoutSeconds,
        circuitErrorRateThreshold: config.circuitErrorRateThreshold,
        circuitMinRequests: config.circuitMinRequests,
      });
      setIsDirty(false);
      lastSyncedConfigRef.current = config;
      toast.success(
        t("proxy.autoFailover.configSaved", "自动故障转移配置已保存"),
        { closeButton: true },
      );
    } catch (e) {
      toast.error(
        t("proxy.autoFailover.configSaveFailed", "保存失败") + ": " + String(e),
      );
    }
  };

  const handleReset = () => {
    if (config) {
      setFormData({
        maxRetries: String(config.maxRetries),
        streamingFirstByteTimeout: String(config.streamingFirstByteTimeout),
        streamingIdleTimeout: String(config.streamingIdleTimeout),
        nonStreamingTimeout: String(config.nonStreamingTimeout),
      });
    }
  };

  if (isLoading) {
    return (
      <div className="flex items-center justify-center p-4">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }

  const isDisabled = disabled || updateConfig.isPending;

  return (
    <div className="space-y-3">
      {error && (
        <Alert variant="destructive">
          <AlertDescription>{String(error)}</AlertDescription>
        </Alert>
      )}

      <p className="flex items-start gap-1.5 text-xs text-muted-foreground">
        <Info className="mt-0.5 h-3.5 w-3.5 flex-shrink-0" />
        <span>
          {t(
            "proxy.autoFailover.info",
            "每个 Key 都是一条独立通道：请求失败时只冷却出错的 Key（按错误类型指数退避，自动恢复），同一供应商的其他 Key 不受影响，并按队列顺序继续尝试。",
          )}
        </span>
      </p>

      {/* 重试与超时设置（合并为单块四字段） */}
      <div className="space-y-3 rounded-lg border border-border/60 bg-muted/30 p-3">
        <h4 className="text-sm font-semibold">
          {t("proxy.autoFailover.retrySettings", "重试与超时设置")}
        </h4>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor={`maxRetries-${appType}`}>
              {t("proxy.autoFailover.maxRetries", "最大重试次数")}
            </Label>
            <Input
              id={`maxRetries-${appType}`}
              type="number"
              min="0"
              max="10"
              value={formData.maxRetries}
              onChange={(e) => {
                setFormData({ ...formData, maxRetries: e.target.value });
                setIsDirty(true);
              }}
              disabled={isDisabled}
            />
            <p className="text-xs text-muted-foreground">
              {t(
                "proxy.autoFailover.maxRetriesHint",
                "请求失败时的重试次数（0-10）",
              )}
            </p>
          </div>

          <div className="space-y-1.5">
            <Label htmlFor={`streamingFirstByte-${appType}`}>
              {t(
                "proxy.autoFailover.streamingFirstByte",
                "流式首字节超时（秒）",
              )}
            </Label>
            <Input
              id={`streamingFirstByte-${appType}`}
              type="number"
              min="1"
              max="120"
              value={formData.streamingFirstByteTimeout}
              onChange={(e) => {
                setFormData({
                  ...formData,
                  streamingFirstByteTimeout: e.target.value,
                });
                setIsDirty(true);
              }}
              disabled={isDisabled}
            />
            <p className="text-xs text-muted-foreground">
              {t(
                "proxy.autoFailover.streamingFirstByteHint",
                "等待首个数据块的最大时间，范围 1-120 秒，默认 60 秒",
              )}
            </p>
          </div>

          <div className="space-y-1.5">
            <Label htmlFor={`streamingIdle-${appType}`}>
              {t("proxy.autoFailover.streamingIdle", "流式静默超时（秒）")}
            </Label>
            <Input
              id={`streamingIdle-${appType}`}
              type="number"
              min="0"
              max="600"
              value={formData.streamingIdleTimeout}
              onChange={(e) => {
                setFormData({
                  ...formData,
                  streamingIdleTimeout: e.target.value,
                });
                setIsDirty(true);
              }}
              disabled={isDisabled}
            />
            <p className="text-xs text-muted-foreground">
              {t(
                "proxy.autoFailover.streamingIdleHint",
                "数据块之间的最大间隔，范围 60-600 秒，填 0 禁用（防止中途卡住）",
              )}
            </p>
          </div>

          <div className="space-y-1.5">
            <Label htmlFor={`nonStreaming-${appType}`}>
              {t("proxy.autoFailover.nonStreaming", "非流式超时（秒）")}
            </Label>
            <Input
              id={`nonStreaming-${appType}`}
              type="number"
              min="60"
              max="1200"
              value={formData.nonStreamingTimeout}
              onChange={(e) => {
                setFormData({
                  ...formData,
                  nonStreamingTimeout: e.target.value,
                });
                setIsDirty(true);
              }}
              disabled={isDisabled}
            />
            <p className="text-xs text-muted-foreground">
              {t(
                "proxy.autoFailover.nonStreamingHint",
                "非流式请求的总超时时间，范围 60-1200 秒，默认 600 秒（10 分钟）",
              )}
            </p>
          </div>
        </div>
      </div>

      {/* 操作按钮 */}
      <div className="flex justify-end gap-2">
        <Button variant="outline" onClick={handleReset} disabled={isDisabled}>
          {t("common.reset", "重置")}
        </Button>
        <Button onClick={handleSave} disabled={isDisabled}>
          {updateConfig.isPending ? (
            <>
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              {t("common.saving", "保存中...")}
            </>
          ) : (
            <>
              <Save className="mr-2 h-4 w-4" />
              {t("common.save", "保存")}
            </>
          )}
        </Button>
      </div>
    </div>
  );
}
