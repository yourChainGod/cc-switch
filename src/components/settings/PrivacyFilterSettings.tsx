import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import {
  privacyFilterApi,
  type PrivacyFilterConfig,
} from "@/lib/api/privacy-filter";

/** 可单独开关的检测分类（顺序即 UI 展示顺序） */
const CATEGORIES = [
  "email",
  "phone",
  "idCard",
  "bankCard",
  "ip",
  "secret",
] as const;

const DEFAULT_TEST_INPUT =
  "我的邮箱是 contact@example.com，手机号是 13800138000";

export function PrivacyFilterSettings() {
  const { t } = useTranslation();
  const [config, setConfig] = useState<PrivacyFilterConfig>({
    enabled: false,
    email: true,
    phone: true,
    idCard: true,
    bankCard: true,
    ip: true,
    secret: true,
  });
  const [isLoading, setIsLoading] = useState(true);
  const [testInput, setTestInput] = useState(DEFAULT_TEST_INPUT);
  const [testOutput, setTestOutput] = useState<string | null>(null);
  const [testCount, setTestCount] = useState(0);

  useEffect(() => {
    privacyFilterApi
      .getConfig()
      .then(setConfig)
      .catch((e) => console.error("Failed to load privacy filter config:", e))
      .finally(() => setIsLoading(false));
  }, []);

  const handleChange = async (updates: Partial<PrivacyFilterConfig>) => {
    const newConfig = { ...config, ...updates };
    setConfig(newConfig);
    try {
      await privacyFilterApi.setConfig(newConfig);
    } catch (e) {
      console.error("Failed to save privacy filter config:", e);
      toast.error(String(e));
      setConfig(config);
    }
  };

  const runTest = async () => {
    try {
      const res = await privacyFilterApi.test(config, testInput);
      setTestOutput(res.redacted);
      setTestCount(res.count);
    } catch (e) {
      console.error("Failed to test privacy filter:", e);
      toast.error(String(e));
    }
  };

  if (isLoading) return null;

  return (
    <div className="space-y-6">
      {/* 总开关 */}
      <div className="flex items-center justify-between">
        <div className="space-y-0.5">
          <Label>{t("settings.advanced.privacy.enabled")}</Label>
          <p className="text-xs text-muted-foreground">
            {t("settings.advanced.privacy.enabledDescription")}
          </p>
        </div>
        <Switch
          checked={config.enabled}
          onCheckedChange={(checked) => handleChange({ enabled: checked })}
        />
      </div>

      {/* 分类开关 */}
      <div className="space-y-4">
        <h4 className="text-sm font-medium text-muted-foreground">
          {t("settings.advanced.privacy.categoriesGroup")}
        </h4>
        {CATEGORIES.map((key) => (
          <div key={key} className="flex items-center justify-between pl-4">
            <div className="space-y-0.5">
              <Label>{t(`settings.advanced.privacy.${key}`)}</Label>
              <p className="text-xs text-muted-foreground">
                {t(`settings.advanced.privacy.${key}Description`)}
              </p>
            </div>
            <Switch
              checked={config[key]}
              disabled={!config.enabled}
              onCheckedChange={(checked) =>
                handleChange({ [key]: checked } as Partial<PrivacyFilterConfig>)
              }
            />
          </div>
        ))}
        {config.enabled && config.secret && (
          <p className="pl-4 text-xs text-muted-foreground">
            {t("settings.advanced.privacy.secretCoverageNote")}
          </p>
        )}
      </div>

      {/* 测试过滤 */}
      <div className="border-t pt-6 mt-6 space-y-3">
        <div className="space-y-1">
          <h3 className="text-sm font-medium">
            {t("settings.advanced.privacy.testTitle")}
          </h3>
          <p className="text-xs text-muted-foreground">
            {t("settings.advanced.privacy.testDescription")}
          </p>
          <p className="text-xs text-amber-600 dark:text-amber-400">
            {t("settings.advanced.privacy.testTrustNote")}
          </p>
        </div>
        <textarea
          className="w-full min-h-[72px] rounded-md border border-input bg-background px-3 py-2 text-sm"
          value={testInput}
          onChange={(e) => setTestInput(e.target.value)}
        />
        <div className="flex items-center gap-3">
          <button
            type="button"
            className="h-9 rounded-md border border-input bg-background px-4 text-sm hover:bg-muted/50"
            onClick={runTest}
          >
            {t("settings.advanced.privacy.testRun")}
          </button>
          {testOutput !== null && (
            <span className="text-xs text-muted-foreground">
              {t("settings.advanced.privacy.testHits", { count: testCount })}
            </span>
          )}
        </div>
        {testOutput !== null && (
          <pre className="whitespace-pre-wrap break-all rounded-md bg-muted/50 px-3 py-2 text-sm">
            {testOutput}
          </pre>
        )}
      </div>
    </div>
  );
}
