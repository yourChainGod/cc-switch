import { useState } from "react";
import {
  Server,
  Activity,
  Zap,
  Globe,
  ShieldAlert,
  Shuffle,
  Power,
  TrendingUp,
  Clock,
} from "lucide-react";
import { motion } from "framer-motion";
import { useTranslation } from "react-i18next";
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Badge } from "@/components/ui/badge";
import { Switch } from "@/components/ui/switch";
import { ProxyPanel } from "@/components/proxy";
import { ModelMappingPanel } from "@/components/proxy/ModelMappingPanel";
import { AutoFailoverConfigPanel } from "@/components/proxy/AutoFailoverConfigPanel";
import { FailoverQueueManager } from "@/components/proxy/FailoverQueueManager";
import { RectifierConfigPanel } from "@/components/settings/RectifierConfigPanel";
import { GlobalProxySettings } from "@/components/settings/GlobalProxySettings";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ToggleRow } from "@/components/ui/toggle-row";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import {
  useProxyTakeoverStatus,
  useSetProxyTakeoverForApp,
} from "@/lib/query/proxy";
import { toast } from "sonner";
import { extractErrorMessage } from "@/utils/errorUtils";
import type { ModelRoutingClient } from "@/types/modelRouting";
import type { SettingsFormState } from "@/hooks/useSettings";

const CLIENTS: ModelRoutingClient[] = ["claude", "codex", "gemini"];
const CLIENT_LABEL: Record<ModelRoutingClient, string> = {
  claude: "Claude",
  codex: "Codex",
  gemini: "Gemini",
};

interface ProxyTabContentProps {
  settings: SettingsFormState;
  onAutoSave: (updates: Partial<SettingsFormState>) => Promise<void>;
}

export function ProxyTabContent({
  settings,
  onAutoSave,
}: ProxyTabContentProps) {
  const { t } = useTranslation();
  const [showProxyConfirm, setShowProxyConfirm] = useState(false);
  const [showFailoverConfirm, setShowFailoverConfirm] = useState(false);

  const {
    status,
    isRunning,
    startProxyServer,
    stopWithRestore,
    isPending: isProxyPending,
  } = useProxyStatus();

  const formatUptime = (seconds: number): string => {
    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = seconds % 60;
    if (h > 0) return `${h}h ${m}m`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  };

  const handleToggleProxy = async (checked: boolean) => {
    try {
      if (!checked) {
        await stopWithRestore();
      } else if (!settings?.proxyConfirmed) {
        setShowProxyConfirm(true);
      } else {
        await startProxyServer();
      }
    } catch (error) {
      console.error("Toggle proxy failed:", error);
    }
  };

  const handleProxyConfirm = async () => {
    setShowProxyConfirm(false);
    try {
      await onAutoSave({ proxyConfirmed: true });
      await startProxyServer();
    } catch (error) {
      console.error("Proxy confirm failed:", error);
    }
  };

  const handleFailoverToggleChange = (checked: boolean) => {
    if (checked && !settings?.failoverConfirmed) {
      setShowFailoverConfirm(true);
    } else {
      void onAutoSave({ enableFailoverToggle: checked });
    }
  };

  const handleFailoverConfirm = async () => {
    setShowFailoverConfirm(false);
    try {
      await onAutoSave({ failoverConfirmed: true, enableFailoverToggle: true });
    } catch (error) {
      console.error("Failover confirm failed:", error);
    }
  };

  return (
    <motion.div
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3 }}
      className="space-y-4"
    >
      {/* 路由状态条：常驻顶部，一眼看清运行状态并可一键启停 */}
      <div className="rounded-xl glass-card p-4">
        <div className="flex items-center gap-3">
          <div
            className={`flex h-10 w-10 items-center justify-center rounded-lg ring-1 ${
              isRunning
                ? "bg-green-500/15 ring-green-500/30"
                : "bg-muted ring-border"
            }`}
          >
            <Power
              className={`h-5 w-5 ${
                isRunning ? "text-green-500" : "text-muted-foreground"
              }`}
            />
          </div>
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold">
                {t("settings.advanced.proxy.title")}
              </h3>
              <Badge
                variant={isRunning ? "default" : "secondary"}
                className="h-5 gap-1 px-1.5 text-[11px]"
              >
                <Activity
                  className={`h-2.5 w-2.5 ${isRunning ? "animate-pulse" : ""}`}
                />
                {isRunning
                  ? t("settings.advanced.proxy.running")
                  : t("settings.advanced.proxy.stopped")}
              </Badge>
            </div>
            {isRunning && status ? (
              <code className="mt-0.5 block truncate text-xs text-muted-foreground">
                {status.address.includes(":")
                  ? `http://[${status.address}]:${status.port}`
                  : `http://${status.address}:${status.port}`}
              </code>
            ) : (
              <p className="mt-0.5 truncate text-xs text-muted-foreground">
                {t("proxy.panel.stoppedDescription", {
                  defaultValue: "使用右侧开关即可启动服务",
                })}
              </p>
            )}
          </div>
          <Switch
            checked={isRunning}
            onCheckedChange={handleToggleProxy}
            disabled={isProxyPending}
          />
        </div>

        {isRunning && status && (
          <div className="mt-3 grid grid-cols-3 gap-2 border-t border-border/50 pt-3">
            <HeaderStat
              icon={<TrendingUp className="h-3.5 w-3.5" />}
              label={t("proxy.panel.stats.totalRequests", {
                defaultValue: "总请求数",
              })}
              value={status.total_requests}
            />
            <HeaderStat
              icon={<Activity className="h-3.5 w-3.5" />}
              label={t("proxy.panel.stats.successRate", {
                defaultValue: "成功率",
              })}
              value={`${status.success_rate.toFixed(1)}%`}
            />
            <HeaderStat
              icon={<Clock className="h-3.5 w-3.5" />}
              label={t("proxy.panel.stats.uptime", {
                defaultValue: "运行时间",
              })}
              value={formatUptime(status.uptime_seconds)}
            />
          </div>
        )}
      </div>

      {/* 故障转移总开关（全局）+ 未运行提示 */}
      <div className="rounded-xl glass-card p-4 space-y-4">
        <ToggleRow
          icon={<ShieldAlert className="h-4 w-4 text-orange-500" />}
          title={t("settings.advanced.proxy.enableFailoverToggle")}
          description={t(
            "settings.advanced.proxy.enableFailoverToggleDescription",
          )}
          checked={settings?.enableFailoverToggle ?? false}
          onCheckedChange={handleFailoverToggleChange}
        />
        {!isRunning && (
          <div className="p-3 rounded-lg bg-blue-500/10 border border-blue-500/20">
            <p className="text-sm text-blue-600 dark:text-blue-400">
              {t("proxy.failover.proxyRequired", {
                defaultValue: "路由服务未运行：配置可随时修改，启动服务后生效",
              })}
            </p>
          </div>
        )}
      </div>

      {/* 顶层客户端维度：先选 Claude/Codex/Gemini，其下统一看 接管 / 模型映射 / 故障转移 */}
      <Tabs defaultValue="claude" className="w-full">
        <TabsList className="grid w-full grid-cols-3">
          {CLIENTS.map((c) => (
            <TabsTrigger key={c} value={c}>
              {CLIENT_LABEL[c]}
            </TabsTrigger>
          ))}
        </TabsList>
        {CLIENTS.map((client) => (
          <TabsContent
            key={client}
            value={client}
            className="mt-4 space-y-4"
          >
            {/* 接管 */}
            <section className="rounded-xl glass-card p-4 space-y-3">
              <SectionHeader
                icon={<Power className="h-4 w-4 text-green-500" />}
                title={t("proxyConfig.appTakeover", {
                  defaultValue: "应用接管",
                })}
                description={t("proxy.takeover.hint", {
                  defaultValue:
                    "启用后该客户端的请求将通过本地代理转发（需先启动路由服务）",
                })}
              />
              <ClientTakeoverToggle appType={client} />
            </section>

            {/* 模型映射 */}
            <section className="rounded-xl glass-card p-4 space-y-3">
              <SectionHeader
                icon={<Shuffle className="h-4 w-4 text-indigo-500" />}
                title={t("proxy.modelMapping.title", {
                  defaultValue: "模型映射",
                })}
                description={t("proxy.modelMapping.description", {
                  defaultValue:
                    "按客户端把请求模型精确/前缀/后缀/关键词/正则映射到目标上游模型",
                })}
              />
              <ModelMappingPanel client={client} />
            </section>

            {/* 故障转移 */}
            <section className="rounded-xl glass-card p-4 space-y-4">
              <SectionHeader
                icon={<Activity className="h-4 w-4 text-orange-500" />}
                title={t("settings.advanced.failover.title")}
                description={t("settings.advanced.failover.description")}
              />
              <div className="space-y-1">
                <h4 className="text-sm font-semibold">
                  {t("proxy.failoverQueue.title")}
                </h4>
                <p className="text-xs text-muted-foreground">
                  {t("proxy.failoverQueue.description")}
                </p>
              </div>
              <FailoverQueueManager appType={client} />
              <div className="border-t border-border/50 pt-4">
                <AutoFailoverConfigPanel appType={client} />
              </div>
            </section>
          </TabsContent>
        ))}
      </Tabs>

      {/* 全局段：与具体客户端无关的设置 */}
      <Accordion type="multiple" defaultValue={[]} className="w-full space-y-3">
        {/* Local Proxy / 服务设置 */}
        <AccordionItem
          value="proxy"
          className="rounded-xl glass-card overflow-hidden"
        >
          <AccordionTrigger className="px-4 py-3 hover:no-underline hover:bg-muted/50 data-[state=open]:bg-muted/50">
            <div className="flex items-center gap-3">
              <Server className="h-5 w-5 text-green-500" />
              <div className="text-left">
                <h3 className="text-sm font-semibold">
                  {t("settings.advanced.proxy.title")}
                </h3>
                <p className="text-xs text-muted-foreground font-normal">
                  {t("settings.advanced.proxy.description")}
                </p>
              </div>
              <Badge
                variant={isRunning ? "default" : "secondary"}
                className="gap-1.5 h-6 ml-auto mr-2"
              >
                <Activity
                  className={`h-3 w-3 ${isRunning ? "animate-pulse" : ""}`}
                />
                {isRunning
                  ? t("settings.advanced.proxy.running")
                  : t("settings.advanced.proxy.stopped")}
              </Badge>
            </div>
          </AccordionTrigger>
          <AccordionContent className="px-4 pb-4 pt-3 border-t border-border/50">
            <ProxyPanel
              enableLocalProxy={settings?.enableLocalProxy ?? false}
              onEnableLocalProxyChange={(checked) =>
                onAutoSave({ enableLocalProxy: checked })
              }
              onToggleProxy={handleToggleProxy}
              isProxyPending={isProxyPending}
            />
          </AccordionContent>
        </AccordionItem>

        {/* Rectifier */}
        <AccordionItem
          value="rectifier"
          className="rounded-xl glass-card overflow-hidden"
        >
          <AccordionTrigger className="px-4 py-3 hover:no-underline hover:bg-muted/50 data-[state=open]:bg-muted/50">
            <div className="flex items-center gap-3">
              <Zap className="h-5 w-5 text-purple-500" />
              <div className="text-left">
                <h3 className="text-sm font-semibold">
                  {t("settings.advanced.rectifier.title")}
                </h3>
                <p className="text-xs text-muted-foreground font-normal">
                  {t("settings.advanced.rectifier.description")}
                </p>
              </div>
            </div>
          </AccordionTrigger>
          <AccordionContent className="px-4 pb-4 pt-3 border-t border-border/50">
            <RectifierConfigPanel />
          </AccordionContent>
        </AccordionItem>

        {/* Global Outbound Proxy */}
        <AccordionItem
          value="globalProxy"
          className="rounded-xl glass-card overflow-hidden"
        >
          <AccordionTrigger className="px-4 py-3 hover:no-underline hover:bg-muted/50 data-[state=open]:bg-muted/50">
            <div className="flex items-center gap-3">
              <Globe className="h-5 w-5 text-cyan-500" />
              <div className="text-left">
                <h3 className="text-sm font-semibold">
                  {t("settings.advanced.globalProxy.title")}
                </h3>
                <p className="text-xs text-muted-foreground font-normal">
                  {t("settings.advanced.globalProxy.description")}
                </p>
              </div>
            </div>
          </AccordionTrigger>
          <AccordionContent className="px-4 pb-4 pt-3 border-t border-border/50">
            <GlobalProxySettings />
          </AccordionContent>
        </AccordionItem>
      </Accordion>

      <ConfirmDialog
        isOpen={showProxyConfirm}
        variant="info"
        title={t("confirm.proxy.title")}
        message={t("confirm.proxy.message")}
        confirmText={t("confirm.proxy.confirm")}
        onConfirm={() => void handleProxyConfirm()}
        onCancel={() => setShowProxyConfirm(false)}
      />

      <ConfirmDialog
        isOpen={showFailoverConfirm}
        variant="info"
        title={t("confirm.failover.title")}
        message={t("confirm.failover.message")}
        confirmText={t("confirm.failover.confirm")}
        onConfirm={() => void handleFailoverConfirm()}
        onCancel={() => setShowFailoverConfirm(false)}
      />
    </motion.div>
  );
}

interface HeaderStatProps {
  icon: React.ReactNode;
  label: string;
  value: string | number;
}

function HeaderStat({ icon, label, value }: HeaderStatProps) {
  return (
    <div className="rounded-lg bg-muted/40 px-2.5 py-1.5">
      <div className="flex items-center gap-1 text-muted-foreground">
        {icon}
        <span className="text-[11px]">{label}</span>
      </div>
      <p className="mt-0.5 text-sm font-semibold text-foreground">{value}</p>
    </div>
  );
}

interface SectionHeaderProps {
  icon: React.ReactNode;
  title: string;
  description?: string;
}

function SectionHeader({ icon, title, description }: SectionHeaderProps) {
  return (
    <div className="flex items-start gap-2 pb-2 border-b border-border/40">
      <span className="mt-0.5">{icon}</span>
      <div className="text-left">
        <h3 className="text-sm font-semibold">{title}</h3>
        {description ? (
          <p className="text-xs text-muted-foreground font-normal">
            {description}
          </p>
        ) : null}
      </div>
    </div>
  );
}

function ClientTakeoverToggle({ appType }: { appType: ModelRoutingClient }) {
  const { t } = useTranslation();
  const { data: takeoverStatus } = useProxyTakeoverStatus();
  const setTakeoverForApp = useSetProxyTakeoverForApp();
  const isEnabled = takeoverStatus?.[appType] ?? false;

  const handleChange = async (enabled: boolean) => {
    try {
      await setTakeoverForApp.mutateAsync({ appType, enabled });
      toast.success(
        enabled
          ? t("proxy.takeover.enabled", {
              app: appType,
              defaultValue: `${appType} 接管已启用`,
            })
          : t("proxy.takeover.disabled", {
              app: appType,
              defaultValue: `${appType} 接管已关闭`,
            }),
        { closeButton: true },
      );
    } catch (error) {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "未知错误" });
      toast.error(
        t("proxy.takeover.failed", {
          detail,
          defaultValue: "切换接管状态失败",
        }),
      );
    }
  };

  return (
    <div className="flex items-center justify-between rounded-md border border-primary/20 bg-background/60 px-3 py-2">
      <span className="text-sm font-medium">{CLIENT_LABEL[appType]}</span>
      <Switch
        checked={isEnabled}
        onCheckedChange={handleChange}
        disabled={setTakeoverForApp.isPending}
      />
    </div>
  );
}
