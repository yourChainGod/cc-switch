// 用量脚本模板类型常量
export const TEMPLATE_TYPES = {
  CUSTOM: "custom",
  GENERAL: "general",
  NEW_API: "newapi",
  SUB2API: "sub2api",
  TOKEN_PLAN: "token_plan",
  BALANCE: "balance",
  OFFICIAL_SUBSCRIPTION: "official_subscription",
} as const;

export type TemplateType = (typeof TEMPLATE_TYPES)[keyof typeof TEMPLATE_TYPES];

// sub2api 用量查询脚本（探测 {{baseUrl}}/v1/usage）。
// 供 UsageScriptModal 预设下拉与「添加 Key 时自动探测」共用。
// 纯字符串字面量（无 i18n 插值），可安全抽为共享常量。
export const SUB2API_USAGE_SCRIPT = `({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}",
      "User-Agent": "cc-switch/1.0"
    }
  },
  extractor: function(response) {
    if (response.error) {
      return {
        isValid: false,
        invalidMessage: response.error.message || response.message || "查询失败"
      };
    }

    const planName = response.planName || response.plan_name || response.name || "Sub2API";

    if (response.mode === "quota_limited" && response.quota) {
      const used = Number(response.quota.used || 0);
      const total = Number(response.quota.limit || 0);
      const remaining = Number(response.quota.remaining ?? (total - used));

      return {
        planName,
        used,
        total,
        remaining,
        unit: "USD"
      };
    }

    if (response.subscription) {
      const s = response.subscription;

      const total =
        Number(s.monthly_limit_usd || 0) ||
        Number(s.weekly_limit_usd || 0) ||
        Number(s.daily_limit_usd || 0);

      const used =
        Number(s.monthly_usage_usd || 0) ||
        Number(s.weekly_usage_usd || 0) ||
        Number(s.daily_usage_usd || 0);

      return {
        planName,
        used,
        total,
        remaining: Number(response.remaining ?? (total - used)),
        unit: "USD"
      };
    }

    if (response.remaining != null || response.balance != null) {
      return {
        planName,
        remaining: Number(response.remaining ?? response.balance),
        unit: "USD"
      };
    }

    return {
      isValid: false,
      invalidMessage: "返回结构无法识别"
    };
  }
})`;
