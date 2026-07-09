import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { settingsApi, type CodexContinueConfig } from "@/lib/api/settings";

const SAVE_DEBOUNCE_MS = 600;

export function CodexContinueConfigPanel() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<CodexContinueConfig | null>(null);
  // 串行化保存：始终只持久化最新快照，防止乱序完成把旧值落库
  const latestRef = useRef<CodexContinueConfig | null>(null);
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const savingRef = useRef(false);
  const dirtyRef = useRef(false);

  useEffect(() => {
    settingsApi
      .getCodexContinueConfig()
      .then((remote) => {
        latestRef.current = remote;
        setConfig(remote);
      })
      .catch((error) => {
        console.error("Failed to load Codex Continue config:", error);
        toast.error(String(error));
      });
    return () => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    };
  }, []);

  const flushSave = useCallback(async () => {
    if (savingRef.current) {
      dirtyRef.current = true;
      return;
    }
    const snapshot = latestRef.current;
    if (!snapshot) return;
    savingRef.current = true;
    try {
      await settingsApi.setCodexContinueConfig(snapshot);
    } catch (error) {
      console.error("Failed to save Codex Continue config:", error);
      toast.error(String(error));
    } finally {
      savingRef.current = false;
      if (dirtyRef.current) {
        dirtyRef.current = false;
        void flushSave();
      }
    }
  }, []);

  const scheduleSave = useCallback(
    (immediate = false) => {
      if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
      if (immediate) {
        void flushSave();
      } else {
        saveTimerRef.current = setTimeout(() => void flushSave(), SAVE_DEBOUNCE_MS);
      }
    },
    [flushSave],
  );

  const applyChange = useCallback(
    (updates: Partial<CodexContinueConfig>, immediate = false) => {
      setConfig((prev) => {
        if (!prev) return prev;
        const next = { ...prev, ...updates };
        latestRef.current = next;
        return next;
      });
      scheduleSave(immediate);
    },
    [scheduleSave],
  );

  const handleNumberChange = (
    key: "maxContinuations" | "step",
    value: string,
  ) => {
    const parsed = Number.parseInt(value, 10);
    if (!Number.isFinite(parsed)) {
      return;
    }
    applyChange({
      [key]: Math.min(Math.max(key === "step" ? 3 : 0, parsed), 100_000),
    });
  };

  if (!config) return null;

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between gap-4">
        <div className="space-y-0.5">
          <Label>{t("settings.advanced.codexContinue.enabled")}</Label>
          <p className="text-xs text-muted-foreground">
            {t("settings.advanced.codexContinue.enabledDescription")}
          </p>
        </div>
        <Switch
          checked={config.enabled}
          onCheckedChange={(checked) => applyChange({ enabled: checked }, true)}
        />
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor="codex-continue-step">
            {t("settings.advanced.codexContinue.step")}
          </Label>
          <Input
            id="codex-continue-step"
            type="number"
            min={3}
            value={config.step}
            disabled={!config.enabled}
            onChange={(event) => handleNumberChange("step", event.target.value)}
            onBlur={() => scheduleSave(true)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="codex-continue-max">
            {t("settings.advanced.codexContinue.maxContinuations")}
          </Label>
          <Input
            id="codex-continue-max"
            type="number"
            min={0}
            value={config.maxContinuations}
            disabled={!config.enabled}
            onChange={(event) =>
              handleNumberChange("maxContinuations", event.target.value)
            }
            onBlur={() => scheduleSave(true)}
          />
        </div>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="codex-continue-marker">
          {t("settings.advanced.codexContinue.marker")}
        </Label>
        <Textarea
          id="codex-continue-marker"
          value={config.marker}
          disabled={!config.enabled}
          onChange={(event) => applyChange({ marker: event.target.value })}
          onBlur={() => scheduleSave(true)}
          className="min-h-[72px]"
        />
      </div>
    </div>
  );
}
