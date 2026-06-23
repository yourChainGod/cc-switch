import { invoke } from "@tauri-apps/api/core";

/// 隐私过滤配置（与后端 PrivacyFilterConfig 对应，camelCase）
export interface PrivacyFilterConfig {
  /** 总开关 */
  enabled: boolean;
  /** 邮箱 → [邮箱] */
  email: boolean;
  /** 手机号（中国大陆） → [电话] */
  phone: boolean;
  /** 身份证号 → [身份证] */
  idCard: boolean;
  /** 银行卡号（Luhn） → [银行卡] */
  bankCard: boolean;
  /** IP 地址 → [IP] */
  ip: boolean;
  /** API 密钥 / 凭证 / 高熵 Token → [密钥] */
  secret: boolean;
}

export interface PrivacyFilterTestResult {
  /** 脱敏后的文本 */
  redacted: string;
  /** 命中的敏感片段数量 */
  count: number;
}

export const privacyFilterApi = {
  /** 读取隐私过滤配置 */
  async getConfig(): Promise<PrivacyFilterConfig> {
    return await invoke("get_privacy_filter_config");
  },
  /** 保存隐私过滤配置 */
  async setConfig(config: PrivacyFilterConfig): Promise<boolean> {
    return await invoke("set_privacy_filter_config", { config });
  },
  /** 用给定配置对文本做一次脱敏测试（不落库） */
  async test(
    config: PrivacyFilterConfig,
    text: string,
  ): Promise<PrivacyFilterTestResult> {
    return await invoke("test_privacy_filter", { config, text });
  },
};
