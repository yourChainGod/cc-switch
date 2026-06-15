import type { AppId } from "@/lib/api/types";

/**
 * providerKey 型（累加模式）应用清单。
 *
 * 这些应用的供应商以用户提供的 providerKey 作为主键 ID，
 * 配置以片段形式累加写入 live 配置文件，而非整体切换。
 * 新增同类渠道时只需在此处补充，避免在各处手写三渠道清单造成遗漏。
 */
export const ADDITIVE_APP_IDS = [
  "opencode",
] as const satisfies readonly AppId[];

export type AdditiveAppId = (typeof ADDITIVE_APP_IDS)[number];

/** 判断是否为 providerKey 型（累加模式）应用 */
export const isAdditiveApp = (appId: AppId): appId is AdditiveAppId =>
  (ADDITIVE_APP_IDS as readonly AppId[]).includes(appId);
