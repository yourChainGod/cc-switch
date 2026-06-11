import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Copy,
  KeyRound,
  ListRestart,
  Plus,
  RefreshCcw,
  RotateCcw,
  Star,
  TimerOff,
  Trash2,
  Upload,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import type { ProviderKey } from "@/types";
import { cn } from "@/lib/utils";
import { copyText } from "@/lib/clipboard";

/**
 * Key 池管理弹窗
 *
 * 状态与请求逻辑由 EditProviderDialog 持有（controller 模式传入），
 * 弹窗只负责呈现：列表、优先级/权重、启停、配置 Key、批量添加、导入。
 */
export interface ProviderKeyPoolController {
  keys: ProviderKey[];
  isLoading: boolean;
  isSaving: boolean;
  draft: string;
  setDraft: (value: string) => void;
  effectiveConfigKeyId: string | null;
  effectiveConfigKeyMode: "auto" | "manual";
  canImportEmbeddedKey: boolean;
  reload: () => void;
  addKeys: () => void;
  importEmbeddedKey: () => void;
  toggleKey: (key: ProviderKey, enabled: boolean) => void;
  updateKeySchedule: (
    key: ProviderKey,
    updates: Partial<Pick<ProviderKey, "priority" | "weight">>,
  ) => void;
  deleteKey: (key: ProviderKey) => void;
  resetKey: (key: ProviderKey) => void;
  setConfigKey: (key: ProviderKey) => void;
  setConfigKeyAuto: () => void;
}

interface ProviderKeyPoolDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  providerName: string;
  pool: ProviderKeyPoolController;
}

function parseIntegerInput(value: string, fallback: number, min?: number) {
  const parsed = Number.parseInt(value, 10);
  if (Number.isNaN(parsed)) return fallback;
  return min === undefined ? parsed : Math.max(min, parsed);
}

export function ProviderKeyPoolDialog({
  open,
  onOpenChange,
  providerName,
  pool,
}: ProviderKeyPoolDialogProps) {
  const { t } = useTranslation();
  // 删除确认走应用内 ConfirmDialog。不要用 window.confirm：Tauri 的 dialog
  // 插件会把它改写成返回 Promise 的异步函数，同步判断恒为真，会在用户确认前就删除。
  const [pendingDeleteKey, setPendingDeleteKey] = useState<ProviderKey | null>(
    null,
  );

  const maskKey = useCallback((value: string) => {
    if (value.length <= 10) return "••••••";
    return `${value.slice(0, 6)}••••••${value.slice(-4)}`;
  }, []);

  const handleCopyKey = useCallback(
    async (key: ProviderKey) => {
      try {
        await copyText(key.keyValue);
        toast.success(
          t("providerKeys.copied", { defaultValue: "Key copied" }),
        );
      } catch (error) {
        console.error("Failed to copy provider key:", error);
        toast.error(
          t("providerKeys.copyFailed", { defaultValue: "Failed to copy key" }),
        );
      }
    },
    [t],
  );

  const getStatusClassName = useCallback((key: ProviderKey) => {
    if (!key.enabled || key.status === "disabled") {
      return "bg-muted text-muted-foreground";
    }
    if (key.status === "active") {
      return "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400";
    }
    if (key.status === "cooldown") {
      return "bg-amber-500/10 text-amber-600 dark:text-amber-400";
    }
    return "bg-orange-500/10 text-orange-600 dark:text-orange-400";
  }, []);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent zIndex="top" className="max-w-3xl">
        <DialogHeader>
          <DialogTitle className="flex flex-wrap items-center gap-2">
            <KeyRound className="h-4 w-4 text-muted-foreground" />
            {t("providerKeys.title", { defaultValue: "Key Pool" })}
            <span className="text-sm font-normal text-muted-foreground">
              {providerName}
            </span>
            <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
              {t("providerKeys.count", {
                count: pool.keys.length,
                defaultValue: "{{count}} keys",
              })}
            </span>
            <span
              className={cn(
                "rounded px-1.5 py-0.5 text-xs font-medium",
                pool.effectiveConfigKeyMode === "auto"
                  ? "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
                  : "bg-amber-500/10 text-amber-600 dark:text-amber-400",
              )}
            >
              {t(`providerKeys.configMode.${pool.effectiveConfigKeyMode}`, {
                defaultValue:
                  pool.effectiveConfigKeyMode === "auto" ? "Auto" : "Manual",
              })}
            </span>
          </DialogTitle>
          <DialogDescription>
            {t("providerKeys.description", {
              defaultValue:
                "The first available key is used for direct app config, and routed requests use the same pool for failover.",
            })}
          </DialogDescription>
        </DialogHeader>

        <div className="flex-1 space-y-4 overflow-y-auto px-6 py-4">
          <div className="flex items-center justify-end gap-2">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={() => pool.setConfigKeyAuto()}
              disabled={
                pool.isSaving ||
                pool.isLoading ||
                pool.keys.length === 0 ||
                pool.effectiveConfigKeyMode === "auto"
              }
              aria-label={t("providerKeys.setConfigKeyAuto", {
                defaultValue: "Follow priority automatically",
              })}
              title={t("providerKeys.setConfigKeyAuto", {
                defaultValue: "Follow priority automatically",
              })}
            >
              <ListRestart className="h-4 w-4" />
              {t("providerKeys.setConfigKeyAuto", {
                defaultValue: "Follow priority automatically",
              })}
            </Button>
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={() => pool.reload()}
              disabled={pool.isLoading || pool.isSaving}
              aria-label={t("providerKeys.refresh", {
                defaultValue: "Refresh keys",
              })}
              title={t("providerKeys.refresh", {
                defaultValue: "Refresh keys",
              })}
            >
              <RefreshCcw
                className={cn("h-4 w-4", pool.isLoading && "animate-spin")}
              />
            </Button>
          </div>

          <div className="rounded-md border border-border-default bg-background">
            {pool.keys.map((key, index) => {
              const canUseAsConfigKey =
                key.enabled && key.status !== "disabled";
              return (
                <div
                  key={key.id}
                  className={cn(
                    "grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-3 py-3 sm:grid-cols-[minmax(0,1fr)_auto_auto_auto]",
                    index > 0 && "border-t border-border-default",
                  )}
                >
                  <div className="min-w-0 space-y-1">
                    <div className="flex min-w-0 flex-wrap items-center gap-2">
                      <span className="min-w-0 truncate text-sm font-medium text-foreground">
                        {key.name}
                      </span>
                      <span
                        className={cn(
                          "shrink-0 rounded px-1.5 py-0.5 text-xs font-medium",
                          getStatusClassName(key),
                        )}
                      >
                        {t(`providerKeys.status.${key.status}`, {
                          defaultValue: key.status,
                        })}
                      </span>
                      {key.consecutiveFailures > 0 && (
                        <span className="shrink-0 text-xs text-muted-foreground">
                          {t("providerKeys.failures", {
                            count: key.consecutiveFailures,
                            defaultValue: "{{count}} failures",
                          })}
                        </span>
                      )}
                      {pool.effectiveConfigKeyId === key.id && (
                        <span className="shrink-0 rounded bg-primary/10 px-1.5 py-0.5 text-xs font-medium text-primary">
                          {t("providerKeys.configKey", {
                            defaultValue: "Config key",
                          })}
                        </span>
                      )}
                    </div>
                    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground">
                      <span className="truncate font-mono">
                        {maskKey(key.keyValue)}
                      </span>
                      <label className="inline-flex items-center gap-1">
                        <span>
                          {t("providerKeys.priorityLabel", {
                            defaultValue: "优先级",
                          })}
                        </span>
                        <Input
                          type="number"
                          defaultValue={key.priority}
                          disabled={pool.isSaving}
                          className="h-7 w-20 px-2 text-xs"
                          onBlur={(event) => {
                            const value = parseIntegerInput(
                              event.currentTarget.value,
                              key.priority,
                            );
                            event.currentTarget.value = String(value);
                            pool.updateKeySchedule(key, { priority: value });
                          }}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") {
                              event.preventDefault();
                              event.currentTarget.blur();
                            }
                          }}
                        />
                      </label>
                      {key.authField && (
                        <span className="truncate font-mono">
                          {key.authField}
                        </span>
                      )}
                    </div>
                  </div>
                  <div className="flex items-center gap-2 sm:justify-end">
                    <span className="hidden text-xs text-muted-foreground sm:inline">
                      {t("providerKeys.enabled", {
                        defaultValue: "Enabled",
                      })}
                    </span>
                    <Switch
                      checked={key.enabled}
                      disabled={pool.isSaving}
                      onCheckedChange={(checked) =>
                        pool.toggleKey(key, checked)
                      }
                      aria-label={t("providerKeys.enabled", {
                        defaultValue: "Enabled",
                      })}
                    />
                  </div>
                  <div className="col-span-2 flex items-center justify-end gap-1 sm:col-span-1">
                    <Button
                      type="button"
                      variant={
                        pool.effectiveConfigKeyId === key.id
                          ? "secondary"
                          : "ghost"
                      }
                      size="icon"
                      onClick={() => pool.setConfigKey(key)}
                      disabled={
                        pool.isSaving ||
                        pool.effectiveConfigKeyId === key.id ||
                        !canUseAsConfigKey
                      }
                      aria-label={t("providerKeys.setConfigKey", {
                        defaultValue: "Use this key",
                      })}
                      title={t("providerKeys.setConfigKey", {
                        defaultValue: "Use this key",
                      })}
                    >
                      <Star className="h-4 w-4" />
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => void handleCopyKey(key)}
                      aria-label={t("providerKeys.copy", {
                        defaultValue: "Copy key",
                      })}
                      title={t("providerKeys.copy", {
                        defaultValue: "Copy key",
                      })}
                    >
                      <Copy className="h-4 w-4" />
                    </Button>
                    {key.status === "cooldown" ? (
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="text-amber-600 hover:text-amber-600 dark:text-amber-400 dark:hover:text-amber-400"
                        onClick={() => pool.resetKey(key)}
                        disabled={pool.isSaving}
                        aria-label={t("providerKeys.clearCooldown", {
                          defaultValue: "Clear cooldown",
                        })}
                        title={t("providerKeys.clearCooldown", {
                          defaultValue: "Clear cooldown",
                        })}
                      >
                        <TimerOff className="h-4 w-4" />
                      </Button>
                    ) : (
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        onClick={() => pool.resetKey(key)}
                        disabled={pool.isSaving}
                        aria-label={t("providerKeys.reset", {
                          defaultValue: "Reset key health",
                        })}
                        title={t("providerKeys.reset", {
                          defaultValue: "Reset key health",
                        })}
                      >
                        <RotateCcw className="h-4 w-4" />
                      </Button>
                    )}
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => setPendingDeleteKey(key)}
                      disabled={pool.isSaving}
                      aria-label={t("providerKeys.delete", {
                        defaultValue: "Delete key",
                      })}
                      title={t("providerKeys.delete", {
                        defaultValue: "Delete key",
                      })}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
              );
            })}
            {!pool.isLoading && pool.keys.length === 0 && (
              <div className="flex flex-wrap items-center justify-between gap-3 px-3 py-4 text-sm text-muted-foreground">
                <div className="min-w-0">
                  {t("providerKeys.empty", {
                    defaultValue: "No provider keys configured",
                  })}
                </div>
                {pool.canImportEmbeddedKey && (
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => pool.importEmbeddedKey()}
                    disabled={pool.isSaving}
                  >
                    <Upload className="h-4 w-4" />
                    {t("providerKeys.importEmbedded", {
                      defaultValue: "Import Existing Key",
                    })}
                  </Button>
                )}
              </div>
            )}
          </div>

          <div className="grid gap-2">
            <Textarea
              value={pool.draft}
              onChange={(event) => pool.setDraft(event.target.value)}
              placeholder={t("providerKeys.pastePlaceholder", {
                defaultValue: "Paste one key per line",
              })}
              className="min-h-24 font-mono text-xs"
            />
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-xs text-muted-foreground">
                {t("providerKeys.pasteHint", {
                  defaultValue: "Duplicate lines are ignored when adding.",
                })}
              </p>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => pool.addKeys()}
                disabled={pool.isSaving || pool.draft.trim().length === 0}
              >
                <Plus className="h-4 w-4" />
                {t("providerKeys.add", { defaultValue: "Add Keys" })}
              </Button>
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {t("common.close", { defaultValue: "关闭" })}
          </Button>
        </DialogFooter>

        <ConfirmDialog
          isOpen={pendingDeleteKey !== null}
          title={t("providerKeys.delete", { defaultValue: "Delete key" })}
          message={t("providerKeys.deleteConfirm", {
            defaultValue: "Delete this provider key?",
          })}
          zIndex="top"
          onConfirm={() => {
            if (pendingDeleteKey) {
              pool.deleteKey(pendingDeleteKey);
            }
            setPendingDeleteKey(null);
          }}
          onCancel={() => setPendingDeleteKey(null)}
        />
      </DialogContent>
    </Dialog>
  );
}
