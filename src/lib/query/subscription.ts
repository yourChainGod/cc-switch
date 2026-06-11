import { useQuery } from "@tanstack/react-query";
import { subscriptionApi } from "@/lib/api/subscription";
import type { AppId } from "@/lib/api/types";

const REFETCH_INTERVAL = 5 * 60 * 1000; // 5 minutes

export const subscriptionKeys = {
  all: ["subscription"] as const,
  quota: (appId: AppId) => [...subscriptionKeys.all, "quota", appId] as const,
};

export function useSubscriptionQuota(
  appId: AppId,
  enabled: boolean,
  autoQuery = false,
  autoQueryIntervalMinutes = 5,
) {
  const refetchInterval =
    autoQuery && autoQueryIntervalMinutes > 0
      ? Math.max(autoQueryIntervalMinutes, 1) * 60 * 1000
      : false;

  return useQuery({
    queryKey: subscriptionKeys.quota(appId),
    queryFn: () => subscriptionApi.getQuota(appId),
    enabled: enabled && ["claude", "codex", "gemini"].includes(appId),
    refetchInterval,
    refetchIntervalInBackground: Boolean(refetchInterval),
    refetchOnWindowFocus: Boolean(refetchInterval),
    staleTime:
      autoQueryIntervalMinutes > 0
        ? Math.max(autoQueryIntervalMinutes, 1) * 60 * 1000
        : REFETCH_INTERVAL,
    retry: 1,
  });
}
