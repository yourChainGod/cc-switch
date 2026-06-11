import { createContext, useContext } from "react";

/**
 * Key 池入口上下文
 *
 * 由 EditProviderDialog 提供，ApiKeySection 消费：
 * 把"单 API Key 输入"和"多 Key 池"融合成一个管理入口——
 * 输入框下方展示池状态摘要，并提供打开 Key 池弹窗的入口。
 * 新建供应商等没有 Key 池的场景不提供该上下文，入口自动隐藏。
 */
export interface KeyPoolEntryValue {
  /** 池中 Key 总数 */
  total: number;
  /** 可用（已启用且状态正常）的 Key 数 */
  available: number;
  /** 异常（停用/冷却/不稳定）的 Key 数 */
  issues: number;
  /** 直连配置 Key 的跟随模式 */
  configKeyMode: "auto" | "manual";
  isLoading: boolean;
  /** 打开 Key 池管理弹窗 */
  open: () => void;
}

export const KeyPoolEntryContext = createContext<KeyPoolEntryValue | null>(
  null,
);

export function useKeyPoolEntry(): KeyPoolEntryValue | null {
  return useContext(KeyPoolEntryContext);
}
