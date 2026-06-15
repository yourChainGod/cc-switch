import { useState, useEffect } from "react";
import { Save, Loader2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import { toast } from "sonner";
import {
  useGlobalProxyConfig,
  useUpdateGlobalProxyConfig,
} from "@/lib/query/proxy";
import { useTranslation } from "react-i18next";

/**
 * 服务设置（设置-only）
 *
 * 运行状态 / 启停 / 统计 / 服务地址由顶部服务区统一呈现，本组件内联在服务卡内，
 * 只负责可配置项：停止态编辑监听地址/端口；运行态展示当前供应商 + 日志开关。
 */
export function ProxyPanel() {
  const { t } = useTranslation();
  const { status, isRunning } = useProxyStatus();

  // 获取全局代理配置
  const { data: globalConfig } = useGlobalProxyConfig();
  const updateGlobalConfig = useUpdateGlobalProxyConfig();

  // 监听地址/端口的本地状态（端口用字符串以支持完全清空）
  const [listenAddress, setListenAddress] = useState("127.0.0.1");
  const [listenPort, setListenPort] = useState("15721");

  // 同步全局配置到本地状态
  useEffect(() => {
    if (globalConfig) {
      setListenAddress(globalConfig.listenAddress);
      setListenPort(String(globalConfig.listenPort));
    }
  }, [globalConfig]);

  const handleLoggingChange = async (enabled: boolean) => {
    if (!globalConfig) return;
    try {
      await updateGlobalConfig.mutateAsync({
        ...globalConfig,
        enableLogging: enabled,
      });
      toast.success(
        enabled
          ? t("proxy.logging.enabled", { defaultValue: "日志记录已启用" })
          : t("proxy.logging.disabled", { defaultValue: "日志记录已关闭" }),
        { closeButton: true },
      );
    } catch (error) {
      toast.error(
        t("proxy.logging.failed", { defaultValue: "切换日志状态失败" }),
      );
    }
  };

  const handleSaveBasicConfig = async () => {
    if (!globalConfig) return;

    // 校验地址格式（IPv4 / IPv6 字面量 / localhost）
    const addressTrimmed = listenAddress.trim();
    const ipv4Regex = /^(\d{1,3}\.){3}\d{1,3}$/;
    const isValidIpv4 = (addr: string): boolean =>
      ipv4Regex.test(addr) &&
      addr.split(".").every((n) => {
        const num = parseInt(n, 10);
        return num >= 0 && num <= 255;
      });
    // IPv6 字面量校验：必须含 `:` 且能在 [..] 包装后被 URL 解析器接受。
    // 后端 (services/proxy.rs) 会把 `::` 改写成 `::1`，所以这里也接受 `::`。
    const isValidIpv6 = (addr: string): boolean => {
      if (!addr.includes(":")) return false;
      try {
        new URL(`http://[${addr}]/`);
        return true;
      } catch {
        return false;
      }
    };
    const normalizedAddress =
      addressTrimmed === "localhost" ? "127.0.0.1" : addressTrimmed;
    const isValidAddress =
      addressTrimmed === "localhost" ||
      addressTrimmed === "0.0.0.0" ||
      isValidIpv4(addressTrimmed) ||
      isValidIpv6(addressTrimmed);
    if (!isValidAddress) {
      toast.error(
        t("proxy.settings.invalidAddress", {
          defaultValue:
            "地址无效，请输入 IPv4（如 127.0.0.1）、IPv6（如 ::1）或 localhost",
        }),
      );
      return;
    }

    // 严格校验端口：必须是纯数字
    const portTrimmed = listenPort.trim();
    if (!/^\d+$/.test(portTrimmed)) {
      toast.error(
        t("proxy.settings.invalidPort", {
          defaultValue: "端口无效，请输入 1024-65535 之间的数字",
        }),
      );
      return;
    }
    const port = parseInt(portTrimmed);
    if (isNaN(port) || port < 1024 || port > 65535) {
      toast.error(
        t("proxy.settings.invalidPort", {
          defaultValue: "端口无效，请输入 1024-65535 之间的数字",
        }),
      );
      return;
    }
    try {
      await updateGlobalConfig.mutateAsync({
        ...globalConfig,
        listenAddress: normalizedAddress,
        listenPort: port,
      });
      toast.success(
        t("proxy.settings.configSaved", { defaultValue: "代理配置已保存" }),
        { closeButton: true },
      );
    } catch (error) {
      toast.error(
        t("proxy.settings.configSaveFailed", { defaultValue: "保存配置失败" }),
      );
    }
  };

  // 运行态：当前供应商 + 日志开关（地址/统计在顶部服务区）
  if (isRunning && status) {
    return (
      <div className="space-y-4">
        <div className="space-y-2">
          <p className="text-xs text-muted-foreground">{t("provider.inUse")}</p>
          {status.active_targets && status.active_targets.length > 0 ? (
            <div className="grid gap-2 sm:grid-cols-2">
              {status.active_targets.map((target) => (
                <div
                  key={target.app_type}
                  className="flex items-center justify-between rounded-md border border-border bg-background/60 px-2 py-1.5 text-xs"
                >
                  <span className="text-muted-foreground">
                    {target.app_type}
                  </span>
                  <span
                    className="ml-2 font-medium truncate text-foreground"
                    title={target.provider_name}
                  >
                    {target.provider_name}
                  </span>
                </div>
              ))}
            </div>
          ) : status.current_provider ? (
            <p className="text-sm text-muted-foreground">
              {t("proxy.panel.currentProvider", {
                defaultValue: "当前 Provider：",
              })}{" "}
              <span className="font-medium text-foreground">
                {status.current_provider}
              </span>
            </p>
          ) : (
            <p className="text-sm text-yellow-600 dark:text-yellow-400">
              {t("proxy.panel.waitingFirstRequest", {
                defaultValue: "当前 Provider：等待首次请求…",
              })}
            </p>
          )}
        </div>

        <div className="flex items-center justify-between rounded-md border border-border bg-background/60 px-3 py-2">
          <div className="space-y-0.5">
            <Label className="text-sm font-medium">
              {t("proxy.settings.fields.enableLogging.label", {
                defaultValue: "启用日志记录",
              })}
            </Label>
            <p className="text-xs text-muted-foreground">
              {t("proxy.settings.fields.enableLogging.description", {
                defaultValue: "记录所有代理请求，便于排查问题",
              })}
            </p>
          </div>
          <Switch
            checked={globalConfig?.enableLogging ?? true}
            onCheckedChange={handleLoggingChange}
            disabled={updateGlobalConfig.isPending}
          />
        </div>
      </div>
    );
  }

  // 停止态：可编辑监听地址/端口
  return (
    <div className="space-y-4">
      <div className="grid gap-4 md:grid-cols-2">
        <div className="space-y-2">
          <Label htmlFor="listen-address">
            {t("proxy.settings.fields.listenAddress.label", {
              defaultValue: "监听地址",
            })}
          </Label>
          <Input
            id="listen-address"
            value={listenAddress}
            onChange={(e) => setListenAddress(e.target.value)}
            placeholder={t("proxy.settings.fields.listenAddress.placeholder", {
              defaultValue: "127.0.0.1",
            })}
          />
          <p className="text-xs text-muted-foreground">
            {t("proxy.settings.fields.listenAddress.description", {
              defaultValue: "代理服务器监听的 IP 地址（推荐 127.0.0.1）",
            })}
          </p>
        </div>

        <div className="space-y-2">
          <Label htmlFor="listen-port">
            {t("proxy.settings.fields.listenPort.label", {
              defaultValue: "监听端口",
            })}
          </Label>
          <Input
            id="listen-port"
            type="number"
            value={listenPort}
            onChange={(e) => setListenPort(e.target.value)}
            placeholder={t("proxy.settings.fields.listenPort.placeholder", {
              defaultValue: "15721",
            })}
          />
          <p className="text-xs text-muted-foreground">
            {t("proxy.settings.fields.listenPort.description", {
              defaultValue: "代理服务器监听的端口号（1024 ~ 65535）",
            })}
          </p>
        </div>
      </div>

      <div className="flex justify-end">
        <Button
          size="sm"
          onClick={handleSaveBasicConfig}
          disabled={updateGlobalConfig.isPending}
        >
          {updateGlobalConfig.isPending ? (
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
    </div>
  );
}
