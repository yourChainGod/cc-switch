import { useTranslation } from "react-i18next";
import { useMemo } from "react";
import type { ReactNode } from "react";
import { useQueries } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Activity,
  ArrowDownToLine,
  ArrowUpFromLine,
  BadgeCheck,
  ChevronRight,
  Clock3,
  Coins,
  Copy,
  Database,
  Fingerprint,
  Gauge,
  KeyRound,
  Layers3,
  Route,
  X,
} from "lucide-react";
import { useRequestDetail } from "@/lib/query/usage";
import { cn } from "@/lib/utils";
import { copyText } from "@/lib/clipboard";
import { providersApi, type AppId } from "@/lib/api";
import {
  getFreshInputTokens,
  isUnpricedUsage,
  type DecisionStep,
} from "@/types/usage";
import type { ProviderKey } from "@/types";

function parseDecisionTrace(raw: string | undefined): DecisionStep[] | null {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? (parsed as DecisionStep[]) : null;
  } catch {
    return null;
  }
}

const OUTCOME_BADGE: Record<DecisionStep["outcome"], string> = {
  success:
    "border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-300",
  failed:
    "border-rose-200 bg-rose-50 text-rose-700 dark:border-rose-900/60 dark:bg-rose-950/40 dark:text-rose-300",
  skipped_circuit_breaker:
    "border-slate-200 bg-slate-50 text-slate-600 dark:border-slate-800 dark:bg-slate-900/60 dark:text-slate-300",
  pending:
    "border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300",
};

const diagnosticPillClass =
  "inline-flex h-6 max-w-full items-center rounded-md border px-2 text-[11px] font-medium leading-none";

const surfaceClass =
  "rounded-xl border border-border/50 bg-card/50 shadow-sm backdrop-blur-sm";

const subtleSurfaceClass =
  "rounded-xl border border-border/40 bg-background/45 shadow-sm";

function formatUsd(value: string, digits = 4) {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? `$${parsed.toFixed(digits)}` : "$0.0000";
}

function formatDuration(ms?: number) {
  if (typeof ms !== "number") return "--";
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
  return `${ms}ms`;
}

function maskKey(value?: string) {
  if (!value) return "--";
  if (value.length <= 12) return value;
  return `${value.slice(0, 8)}••••••${value.slice(-6)}`;
}

function isAppId(value: string | undefined): value is AppId {
  return (
    value === "claude" ||
    value === "codex" ||
    value === "gemini" ||
    value === "opencode"
  );
}

function outcomeDotClass(outcome: DecisionStep["outcome"]) {
  switch (outcome) {
    case "success":
      return "border-emerald-300 bg-emerald-500 shadow-emerald-500/20";
    case "failed":
      return "border-rose-300 bg-rose-500 shadow-rose-500/20";
    case "pending":
      return "border-amber-300 bg-amber-500 shadow-amber-500/20";
    case "skipped_circuit_breaker":
      return "border-slate-300 bg-slate-400 shadow-slate-500/10";
    default:
      return "border-border bg-muted shadow-black/10";
  }
}

function statusCodeBadgeClass(statusCode: number) {
  if (statusCode >= 200 && statusCode < 300) {
    return "border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-300";
  }
  if (statusCode >= 400) {
    return "border-rose-200 bg-rose-50 text-rose-700 dark:border-rose-900/60 dark:bg-rose-950/40 dark:text-rose-300";
  }
  return "border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/40 dark:text-amber-300";
}

interface MetricTileProps {
  icon: ReactNode;
  label: string;
  value: ReactNode;
  accent?: string;
  muted?: boolean;
}

function MetricTile({
  icon,
  label,
  value,
  accent = "text-blue-500",
  muted,
}: MetricTileProps) {
  return (
    <div className={cn(subtleSurfaceClass, "min-w-0 p-3")}>
      <div className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <span className={cn("shrink-0", accent)}>{icon}</span>
        <span className="truncate">{label}</span>
      </div>
      <div
        className={cn(
          "mt-2 min-w-0 truncate text-base font-semibold tabular-nums text-foreground",
          muted && "text-muted-foreground",
        )}
        title={typeof value === "string" ? value : undefined}
      >
        {value}
      </div>
    </div>
  );
}

interface DetailRowProps {
  label: string;
  value: ReactNode;
  mono?: boolean;
  title?: string;
}

function DetailRow({ label, value, mono, title }: DetailRowProps) {
  return (
    <div className="min-w-0 rounded-lg border border-border/35 bg-background/35 px-3 py-2.5">
      <div className="text-xs font-medium text-muted-foreground">{label}</div>
      <div
        className={cn(
          "mt-1 min-w-0 break-words text-sm font-medium text-foreground",
          mono && "font-mono text-xs",
        )}
        title={title}
      >
        {value}
      </div>
    </div>
  );
}

interface SectionProps {
  title: string;
  icon: ReactNode;
  children: ReactNode;
  className?: string;
}

function Section({ title, icon, children, className }: SectionProps) {
  return (
    <section className={cn(surfaceClass, "min-w-0 p-4", className)}>
      <div className="mb-3 flex items-center gap-2 text-sm font-semibold">
        <span className="text-muted-foreground">{icon}</span>
        <h3>{title}</h3>
      </div>
      {children}
    </section>
  );
}

interface KeyBadgeProps {
  label: string;
  rawValue: string;
  onCopy: (value: string) => Promise<void>;
  muted?: boolean;
}

function KeyBadge({ label, rawValue, onCopy, muted }: KeyBadgeProps) {
  return (
    <button
      type="button"
      onClick={() => void onCopy(rawValue)}
      className={cn(
        diagnosticPillClass,
        "h-7 min-w-0 gap-1.5 rounded-lg bg-background/80 font-mono transition-colors hover:border-blue-300 hover:bg-blue-50 hover:text-blue-700 dark:hover:border-blue-800 dark:hover:bg-blue-950/30 dark:hover:text-blue-300",
        muted
          ? "border-slate-200 text-muted-foreground dark:border-slate-800"
          : "border-blue-200 text-blue-700 dark:border-blue-900/60 dark:text-blue-300",
      )}
      title="复制完整 Key"
    >
      <KeyRound className="h-3.5 w-3.5 shrink-0" />
      <span className="truncate">{label}</span>
      <Copy className="h-3 w-3 shrink-0 opacity-60" />
    </button>
  );
}

interface RequestDetailFrameProps {
  title: string;
  subtitle?: string;
  closeLabel: string;
  onClose: () => void;
  children: ReactNode;
}

function RequestDetailFrame({
  title,
  subtitle,
  closeLabel,
  onClose,
  children,
}: RequestDetailFrameProps) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent
        zIndex="panel"
        className="bottom-6 left-4 right-4 top-6 h-auto max-h-none w-auto max-w-none translate-x-0 translate-y-0 overflow-hidden border-border/60 bg-background/95 p-0 shadow-2xl backdrop-blur-xl sm:bottom-8 sm:left-6 sm:right-6 sm:top-8 sm:rounded-xl"
        overlayClassName="bg-black/45 backdrop-blur-sm"
      >
        <DialogHeader className="relative border-b border-border/50 bg-card/70 px-5 py-4 text-left backdrop-blur-xl">
          <div className="flex min-w-0 items-center gap-3 pr-10">
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-blue-200/70 bg-blue-50 text-blue-600 dark:border-blue-900/60 dark:bg-blue-950/35 dark:text-blue-300">
              <Route className="h-4 w-4" />
            </span>
            <div className="min-w-0">
              <DialogTitle className="text-base font-semibold text-foreground">
                {title}
              </DialogTitle>
              {subtitle && (
                <p className="truncate text-xs text-muted-foreground">
                  {subtitle}
                </p>
              )}
            </div>
          </div>
          <DialogClose
            className="absolute right-4 top-1/2 inline-flex h-8 w-8 -translate-y-1/2 items-center justify-center rounded-lg border border-border/50 bg-background/80 text-muted-foreground shadow-sm transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            aria-label={closeLabel}
          >
            <X className="h-4 w-4" />
          </DialogClose>
        </DialogHeader>

        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-5">
          {children}
        </div>
      </DialogContent>
    </Dialog>
  );
}

function RequestDetailSkeleton() {
  return (
    <div className="space-y-4">
      <div className="grid gap-3 md:grid-cols-4">
        {Array.from({ length: 4 }).map((_, index) => (
          <div
            key={index}
            className={cn(subtleSurfaceClass, "h-[76px] animate-pulse p-3")}
          >
            <div className="h-3 w-16 rounded bg-muted" />
            <div className="mt-4 h-5 w-20 rounded bg-muted" />
          </div>
        ))}
      </div>
      <div className="grid gap-4 md:grid-cols-[0.9fr_1.1fr]">
        <div className={cn(surfaceClass, "h-72 animate-pulse p-4")}>
          <div className="h-4 w-24 rounded bg-muted" />
          <div className="mt-5 grid gap-2 sm:grid-cols-2">
            {Array.from({ length: 6 }).map((_, index) => (
              <div key={index} className="h-14 rounded-lg bg-muted/50" />
            ))}
          </div>
        </div>
        <div className={cn(surfaceClass, "h-44 animate-pulse p-4")}>
          <div className="h-4 w-20 rounded bg-muted" />
          <div className="mt-5 space-y-2">
            <div className="h-10 rounded-lg bg-muted/50" />
            <div className="h-10 rounded-lg bg-muted/50" />
          </div>
        </div>
      </div>
    </div>
  );
}

interface RequestDetailPanelProps {
  requestId: string;
  onClose: () => void;
}

export function RequestDetailPanel({
  requestId,
  onClose,
}: RequestDetailPanelProps) {
  const { t, i18n } = useTranslation();
  const { data: request, isLoading } = useRequestDetail(requestId);
  const parsedSteps = useMemo(
    () => parseDecisionTrace(request?.decisionTrace),
    [request?.decisionTrace],
  );
  const appIdForKeys = isAppId(request?.appType) ? request.appType : undefined;
  const providerIdsForKeys = useMemo(() => {
    if (!request) return [];
    const hasKeyReference =
      Boolean(request.providerKeyId) || parsedSteps?.some((step) => step.keyId);
    if (!hasKeyReference) return [];
    const ids = new Set<string>([request.providerId]);
    parsedSteps?.forEach((step) => ids.add(step.providerId));
    return Array.from(ids);
  }, [parsedSteps, request]);
  const providerKeyQueries = useQueries({
    queries: providerIdsForKeys.map((providerId) => ({
      queryKey: ["providerKeys", appIdForKeys, providerId],
      queryFn: () => providersApi.getKeys(appIdForKeys!, providerId),
      enabled: Boolean(appIdForKeys),
      staleTime: 30_000,
    })),
  });
  const providerKeyById = useMemo(() => {
    const map = new Map<string, ProviderKey>();
    providerKeyQueries.forEach((query) => {
      query.data?.forEach((key) => {
        map.set(key.id, key);
        map.set(`${key.providerId}:${key.id}`, key);
      });
    });
    return map;
  }, [providerKeyQueries]);
  const dateLocale =
    i18n.language === "zh"
      ? "zh-CN"
      : i18n.language === "zh-TW"
        ? "zh-TW"
        : i18n.language === "ja"
          ? "ja-JP"
          : "en-US";
  const detailTitle = t("usage.requestDetail", "请求详情");
  const closeLabel = t("common.close", { defaultValue: "关闭" });

  if (isLoading) {
    return (
      <RequestDetailFrame
        title={detailTitle}
        subtitle={t("common.loading", { defaultValue: "加载中" })}
        closeLabel={closeLabel}
        onClose={onClose}
      >
        <RequestDetailSkeleton />
      </RequestDetailFrame>
    );
  }

  if (!request) {
    return (
      <RequestDetailFrame
        title={detailTitle}
        subtitle={t("usage.requestNotFound", "请求未找到")}
        closeLabel={closeLabel}
        onClose={onClose}
      >
        <div
          className={cn(surfaceClass, "p-8 text-center text-muted-foreground")}
        >
          {t("usage.requestNotFound", "请求未找到")}
        </div>
      </RequestDetailFrame>
    );
  }

  const freshInput = getFreshInputTokens(request);
  const isCacheInclusive = request.inputTokens !== freshInput;
  const unpriced = isUnpricedUsage(request);
  const steps = parsedSteps;
  const statusOk = request.statusCode >= 200 && request.statusCode < 300;
  const totalGeneratedTokens = freshInput + request.outputTokens;
  const totalCostLabel = unpriced
    ? t("usage.unpriced", "未定价")
    : formatUsd(request.totalCostUsd, 4);
  const providerLabel =
    request.providerName || t("usage.unknownProvider", "未知");
  const getKeyInfo = (providerId: string, keyId?: string) => {
    if (!keyId) return null;
    const providerKey =
      providerKeyById.get(`${providerId}:${keyId}`) ??
      providerKeyById.get(keyId);
    const rawValue = providerKey?.keyValue || keyId;
    return {
      rawValue,
      label: providerKey?.name
        ? `${providerKey.name} ${maskKey(rawValue)}`
        : maskKey(rawValue),
      resolved: Boolean(providerKey?.keyValue),
    };
  };
  const handleCopyKey = async (value: string) => {
    try {
      await copyText(value);
      toast.success(t("providerKeys.copied", { defaultValue: "Key copied" }));
    } catch (error) {
      console.error("Failed to copy request provider key:", error);
      toast.error(
        t("providerKeys.copyFailed", { defaultValue: "Failed to copy key" }),
      );
    }
  };
  const requestKeyInfo = getKeyInfo(request.providerId, request.providerKeyId);
  return (
    <RequestDetailFrame
      title={detailTitle}
      subtitle={`${providerLabel} · ${request.model} · ${new Date(
        request.createdAt * 1000,
      ).toLocaleString(dateLocale)}`}
      closeLabel={closeLabel}
      onClose={onClose}
    >
      <div className="grid gap-3 md:grid-cols-4">
        <MetricTile
          icon={<BadgeCheck className="h-3.5 w-3.5" />}
          label={t("usage.status", "状态")}
          value={
            <span
              className={cn(
                diagnosticPillClass,
                "h-7 font-mono",
                statusCodeBadgeClass(request.statusCode),
              )}
            >
              {request.statusCode}
            </span>
          }
          accent={statusOk ? "text-emerald-500" : "text-rose-500"}
        />
        <MetricTile
          icon={<Coins className="h-3.5 w-3.5" />}
          label={t("usage.totalCost", "总成本")}
          value={totalCostLabel}
          accent="text-emerald-500"
          muted={unpriced}
        />
        <MetricTile
          icon={<Activity className="h-3.5 w-3.5" />}
          label={t("usage.generatedTokens", {
            defaultValue: "生成 Token",
          })}
          value={totalGeneratedTokens.toLocaleString()}
          accent="text-blue-500"
        />
        <MetricTile
          icon={<Gauge className="h-3.5 w-3.5" />}
          label={t("usage.latency", "延迟")}
          value={formatDuration(request.latencyMs)}
          accent="text-amber-500"
        />
      </div>

      <div className="mt-4 grid gap-4 md:grid-cols-[0.9fr_1.1fr]">
        <div className="min-w-0 space-y-4">
          <Section
            title={t("usage.basicInfo", "基本信息")}
            icon={<Fingerprint className="h-4 w-4" />}
          >
            <div className="grid gap-2 sm:grid-cols-2">
              <DetailRow
                label={t("usage.requestId", "请求ID")}
                value={request.requestId}
                mono
                title={request.requestId}
              />
              <DetailRow
                label={t("usage.time", "时间")}
                value={new Date(request.createdAt * 1000).toLocaleString(
                  dateLocale,
                )}
              />
              <DetailRow
                label={t("usage.provider", "供应商")}
                value={
                  <span className="flex min-w-0 flex-col gap-1">
                    <span className="truncate">{providerLabel}</span>
                    <span className="break-all font-mono text-xs text-muted-foreground">
                      {request.providerId}
                    </span>
                  </span>
                }
              />
              <DetailRow
                label={t("usage.appType", "应用类型")}
                value={request.appType}
                mono
              />
              <DetailRow
                label={t("usage.model", "模型")}
                value={request.model}
                mono
              />
              <DetailRow
                label={t("usage.source", {
                  defaultValue: "来源",
                })}
                value={request.dataSource || "--"}
                mono
              />
              <DetailRow
                label={t("usage.usedKey", {
                  defaultValue: "使用 Key",
                })}
                value={
                  requestKeyInfo ? (
                    <KeyBadge
                      label={requestKeyInfo.label}
                      rawValue={requestKeyInfo.rawValue}
                      onCopy={handleCopyKey}
                      muted={!requestKeyInfo.resolved}
                    />
                  ) : (
                    "--"
                  )
                }
              />
            </div>
          </Section>

          <Section
            title={t("usage.tokenUsage", "Token 使用量")}
            icon={<Layers3 className="h-4 w-4" />}
          >
            <div className="grid gap-2 sm:grid-cols-2">
              <DetailRow
                label={t("usage.inputTokens", "输入 Tokens")}
                value={
                  <>
                    {freshInput.toLocaleString()}
                    {isCacheInclusive && (
                      <span className="ml-2 text-xs font-normal text-muted-foreground">
                        {t("usage.rawInputLabel", "原始")}:{" "}
                        {request.inputTokens.toLocaleString()}
                      </span>
                    )}
                  </>
                }
                mono
              />
              <DetailRow
                label={t("usage.outputTokens", "输出 Tokens")}
                value={request.outputTokens.toLocaleString()}
                mono
              />
              <DetailRow
                label={t("usage.cacheReadTokens", "缓存读取")}
                value={request.cacheReadTokens.toLocaleString()}
                mono
              />
              <DetailRow
                label={t("usage.cacheCreationTokens", "缓存写入")}
                value={request.cacheCreationTokens.toLocaleString()}
                mono
              />
            </div>
            <div className="mt-3 rounded-lg border border-blue-200/70 bg-blue-50/60 px-3 py-2.5 dark:border-blue-900/50 dark:bg-blue-950/25">
              <div className="flex items-center justify-between gap-3">
                <span className="flex items-center gap-1.5 text-xs font-medium text-blue-700 dark:text-blue-300">
                  <ArrowDownToLine className="h-3.5 w-3.5" />
                  {t("usage.generatedTokens", {
                    defaultValue: "生成 Token",
                  })}
                </span>
                <span className="font-mono text-sm font-semibold tabular-nums text-blue-800 dark:text-blue-200">
                  {totalGeneratedTokens.toLocaleString()}
                </span>
              </div>
            </div>
          </Section>

          <Section
            title={t("usage.costBreakdown", "成本明细")}
            icon={<Coins className="h-4 w-4" />}
          >
            <div className="grid gap-2 sm:grid-cols-2">
              {[
                [
                  t("usage.inputCost", "输入成本"),
                  formatUsd(request.inputCostUsd, 6),
                  <ArrowDownToLine className="h-3.5 w-3.5 text-blue-500" />,
                ],
                [
                  t("usage.outputCost", "输出成本"),
                  formatUsd(request.outputCostUsd, 6),
                  <ArrowUpFromLine className="h-3.5 w-3.5 text-purple-500" />,
                ],
                [
                  t("usage.cacheReadCost", "缓存读取成本"),
                  formatUsd(request.cacheReadCostUsd, 6),
                  <Database className="h-3.5 w-3.5 text-emerald-500" />,
                ],
                [
                  t("usage.cacheCreationCost", "缓存写入成本"),
                  formatUsd(request.cacheCreationCostUsd, 6),
                  <Database className="h-3.5 w-3.5 text-amber-500" />,
                ],
              ].map(([label, value, icon]) => (
                <div
                  key={label as string}
                  className="flex min-w-0 items-center justify-between gap-3 rounded-lg border border-border/35 bg-background/35 px-3 py-2 text-sm"
                >
                  <span className="flex min-w-0 items-center gap-2 text-muted-foreground">
                    {icon}
                    <span className="truncate">{label}</span>
                  </span>
                  <span className="shrink-0 font-mono font-medium tabular-nums">
                    {value}
                  </span>
                </div>
              ))}
            </div>
            {request.costMultiplier &&
              parseFloat(request.costMultiplier) !== 1 && (
                <div className="mt-2 flex items-center justify-between gap-3 rounded-lg border border-border/35 bg-background/35 px-3 py-2 text-sm">
                  <span className="text-muted-foreground">
                    {t("usage.costMultiplier", "成本倍率")}
                  </span>
                  <span className="font-mono font-medium">
                    x{request.costMultiplier}
                  </span>
                </div>
              )}
            <div className="mt-3 rounded-lg border border-emerald-200/70 bg-emerald-50/70 px-3 py-3 dark:border-emerald-900/50 dark:bg-emerald-950/25">
              <div className="flex items-center justify-between gap-3">
                <span className="text-sm font-semibold text-emerald-800 dark:text-emerald-200">
                  {t("usage.totalCost", "总成本")}
                </span>
                <span
                  className={cn(
                    "font-mono text-base font-semibold tabular-nums",
                    unpriced
                      ? "text-muted-foreground"
                      : "text-emerald-700 dark:text-emerald-200",
                  )}
                >
                  {totalCostLabel}
                </span>
              </div>
            </div>
          </Section>

          <Section
            title={t("usage.performance", "性能信息")}
            icon={<Clock3 className="h-4 w-4" />}
          >
            <div className="grid gap-2 sm:grid-cols-3">
              <DetailRow
                label={t("usage.latency", "延迟")}
                value={formatDuration(request.latencyMs)}
                mono
              />
              <DetailRow
                label={t("usage.firstToken", {
                  defaultValue: "首字",
                })}
                value={formatDuration(request.firstTokenMs)}
                mono
              />
              <DetailRow
                label={t("usage.duration", {
                  defaultValue: "总用时",
                })}
                value={formatDuration(request.durationMs)}
                mono
              />
            </div>
          </Section>
        </div>

        <div className="flex min-w-0 flex-col gap-4">
          {request.errorMessage && (
            <Section
              title={t("usage.errorMessage", "错误信息")}
              icon={<Activity className="h-4 w-4" />}
              className="order-2 border-rose-200 bg-rose-50/70 dark:border-rose-900/60 dark:bg-rose-950/30"
            >
              <p className="break-words text-sm leading-6 text-rose-700 dark:text-rose-200/90">
                {request.errorMessage}
              </p>
            </Section>
          )}

          <Section
            title={t("usage.decisionChain", "决策链")}
            icon={<Route className="h-4 w-4" />}
            className="order-1"
          >
            <div className="mb-3 flex items-center justify-between gap-3">
              <p className="text-xs text-muted-foreground">
                {steps && steps.length > 0
                  ? t("usage.decisionTraceCount", {
                      defaultValue: "{{count}} 个路由尝试",
                      count: steps.length,
                    })
                  : t("usage.noDecisionTrace", {
                      defaultValue: "本次请求未发生重试或故障转移",
                    })}
              </p>
              <span
                className={cn(
                  diagnosticPillClass,
                  "shrink-0 border-slate-200 bg-background font-mono text-muted-foreground dark:border-slate-800",
                )}
              >
                {steps?.length ?? 1}
              </span>
            </div>
            {steps && steps.length > 0 ? (
              <ol className="relative space-y-3 before:absolute before:left-3 before:top-4 before:h-[calc(100%-2rem)] before:w-px before:bg-border/70">
                {steps.map((step, idx) => {
                  const stepKeyInfo = getKeyInfo(step.providerId, step.keyId);
                  return (
                    <li
                      key={`${step.index}-${step.providerId}-${idx}`}
                      className="relative grid grid-cols-[1.75rem_1fr] gap-3 text-sm"
                    >
                      <span
                        className={cn(
                          "z-10 mt-1 flex h-6 w-6 shrink-0 items-center justify-center rounded-full border-2 shadow-sm",
                          outcomeDotClass(step.outcome),
                        )}
                      >
                        <span className="h-1.5 w-1.5 rounded-full bg-white/90" />
                      </span>
                      <div className="min-w-0 rounded-lg border border-border/35 bg-background/35 p-3">
                        <div className="min-w-0 space-y-2">
                          <div className="flex min-w-0 items-center gap-2">
                            <span className="shrink-0 font-mono text-xs text-muted-foreground">
                              #{step.index}
                            </span>
                            <span className="min-w-0 flex-1 truncate font-medium text-foreground">
                              {step.providerName || providerLabel}
                            </span>
                            <span
                              className={cn(
                                diagnosticPillClass,
                                "shrink-0",
                                OUTCOME_BADGE[step.outcome] ??
                                  "border-border bg-muted text-muted-foreground",
                              )}
                            >
                              {t(
                                `usage.decisionOutcome.${step.outcome}`,
                                step.outcome,
                              )}
                            </span>
                            {typeof step.statusCode === "number" && (
                              <span
                                className={cn(
                                  diagnosticPillClass,
                                  "shrink-0 font-mono",
                                  statusCodeBadgeClass(step.statusCode),
                                )}
                              >
                                {step.statusCode}
                              </span>
                            )}
                          </div>
                          {(stepKeyInfo || step.error) && (
                            <Collapsible className="flex min-w-0 flex-wrap items-center gap-2">
                              {stepKeyInfo && (
                                <KeyBadge
                                  label={stepKeyInfo.label}
                                  rawValue={stepKeyInfo.rawValue}
                                  onCopy={handleCopyKey}
                                  muted={!stepKeyInfo.resolved}
                                />
                              )}
                              {step.error && (
                                <>
                                  <CollapsibleTrigger
                                    className={cn(
                                      diagnosticPillClass,
                                      "group h-7 gap-1.5 rounded-lg border-rose-200 bg-rose-50 text-rose-700 transition-colors hover:border-rose-300 hover:bg-rose-100 hover:text-rose-800 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring dark:border-rose-900/60 dark:bg-rose-950/35 dark:text-rose-200/90 dark:hover:border-rose-800 dark:hover:bg-rose-950/55 dark:hover:text-rose-100",
                                    )}
                                  >
                                    <ChevronRight className="h-3.5 w-3.5 shrink-0 transition-transform group-data-[state=open]:rotate-90" />
                                    {t("usage.errorDetail", {
                                      defaultValue: "错误详情",
                                    })}
                                  </CollapsibleTrigger>
                                  <CollapsibleContent className="basis-full min-w-0 pt-1">
                                    <pre className="max-h-36 overflow-auto whitespace-pre-wrap break-words rounded-md border border-rose-200/60 bg-rose-50/45 px-2.5 py-2 font-mono text-xs leading-5 text-rose-700 dark:border-rose-900/50 dark:bg-rose-950/20 dark:text-rose-200/90">
                                      {step.error}
                                    </pre>
                                  </CollapsibleContent>
                                </>
                              )}
                            </Collapsible>
                          )}
                        </div>
                      </div>
                    </li>
                  );
                })}
              </ol>
            ) : (
              <div className="rounded-lg border border-border/35 bg-background/35 p-3">
                <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
                  <span
                    className={cn(
                      diagnosticPillClass,
                      "border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-900/60 dark:bg-emerald-950/40 dark:text-emerald-300",
                    )}
                  >
                    single_attempt
                  </span>
                  <span>
                    {t("usage.noDecisionTrace", {
                      defaultValue: "本次请求未发生重试或故障转移",
                    })}
                  </span>
                  {requestKeyInfo && (
                    <KeyBadge
                      label={requestKeyInfo.label}
                      rawValue={requestKeyInfo.rawValue}
                      onCopy={handleCopyKey}
                      muted={!requestKeyInfo.resolved}
                    />
                  )}
                </div>
              </div>
            )}
          </Section>

          {request.upstreamErrorBody && (
            <Collapsible className="order-4 rounded-xl border border-rose-200 bg-rose-50/70 p-4 shadow-sm dark:border-rose-900/60 dark:bg-rose-950/30">
              <CollapsibleTrigger className="group flex w-full items-center gap-2 rounded-md text-left text-sm font-semibold text-rose-800 outline-none transition-colors hover:text-rose-700 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 dark:text-rose-200 dark:hover:text-rose-100">
                <ChevronRight className="h-4 w-4 transition-transform group-data-[state=open]:rotate-90" />
                {t("usage.upstreamErrorBody", "上游原始错误")}
              </CollapsibleTrigger>
              <CollapsibleContent className="mt-3">
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-rose-200/60 bg-background/80 p-3 font-mono text-xs leading-5 text-rose-700 dark:border-rose-900/50 dark:bg-background/60 dark:text-rose-200/90">
                  {request.upstreamErrorBody}
                </pre>
              </CollapsibleContent>
            </Collapsible>
          )}
        </div>
      </div>
    </RequestDetailFrame>
  );
}
