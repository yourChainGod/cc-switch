import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      refetchOnWindowFocus: true,
      // 默认 30s 内视为新鲜：避免每次窗口聚焦时所有挂载查询同时重拉（聚焦风暴）。
      // 真正的数据变更由各 mutation 显式 invalidate 或 refetchInterval 轮询覆盖，
      // 不依赖 staleTime:0 + focus。需要更短新鲜度的查询自行覆盖此值。
      staleTime: 30_000,
    },
    mutations: {
      retry: false,
    },
  },
});
