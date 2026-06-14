import { usageApi } from "@/lib/api/usage";
import type { AppId } from "@/lib/api";
import { getProviderBaseUrl } from "@/utils/providerConfigUtils";
import { SUB2API_USAGE_SCRIPT, TEMPLATE_TYPES } from "@/config/constants";
import { createUsageScript, type Provider, type UsageData } from "@/types";

function hasValidUsageData(data: UsageData[] | undefined): boolean {
  return (
    data?.some((item) => {
      if (item.isValid === false) return false;
      return [item.remaining, item.used, item.total].some(
        (value) => typeof value === "number" && Number.isFinite(value),
      );
    }) ?? false
  );
}

/**
 * 探测某个 key 是否为 sub2api 中转：对 `{{baseUrl}}/v1/usage` 实际发一次请求
 * （sub2api 只需 baseUrl + apiKey，无固定域名，只能靠实际探测判断）。
 *
 * 命中 sub2api 结构（testScript 成功且返回有效套餐数据）则为该 key 启用 sub2api
 * 用量查询并返回 true；否则（含探测失败 / 网络错误 / 非 sub2api）静默返回 false。
 *
 * 注意：此路径绕过 UsageScriptModal 的 `usageConfirmed` 同意弹窗——用户已显式
 * 选择「添加 Key 时自动探测」方案，视为同意。
 */
export async function autoConfigureSub2apiUsage(
  provider: Provider,
  keyId: string,
  keyValue: string,
  appId: AppId,
): Promise<boolean> {
  try {
    const baseUrl = getProviderBaseUrl(provider, appId);
    if (!baseUrl) return false;

    const result = await usageApi.testScript(
      provider.id,
      appId,
      SUB2API_USAGE_SCRIPT,
      10,
      keyValue,
      baseUrl,
      undefined,
      undefined,
      TEMPLATE_TYPES.SUB2API,
    );
    if (!(result.success && hasValidUsageData(result.data))) {
      return false;
    }

    await usageApi.setKeyUsageScript(
      provider.id,
      keyId,
      appId,
      createUsageScript({
        enabled: true,
        templateType: TEMPLATE_TYPES.SUB2API,
        code: SUB2API_USAGE_SCRIPT,
        baseUrl,
      }),
    );
    return true;
  } catch {
    // 探测失败 == 不是 sub2api（或网络不可达），静默跳过
    return false;
  }
}
