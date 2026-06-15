import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { proxyApi } from "@/lib/api/proxy";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import type { ModelRoutingConfig } from "@/types/modelRouting";

/**
 * 获取路由层模型映射配置
 */
export function useModelRoutingConfig() {
  return useQuery({
    queryKey: ["modelRoutingConfig"],
    queryFn: () => proxyApi.getModelRoutingConfig(),
  });
}

/**
 * 保存路由层模型映射配置
 */
export function useUpdateModelRoutingConfig() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  return useMutation({
    mutationFn: (config: ModelRoutingConfig) =>
      proxyApi.setModelRoutingConfig(config),
    onSuccess: () => {
      toast.success(t("proxy.settings.toast.saved"), { closeButton: true });
      queryClient.invalidateQueries({ queryKey: ["modelRoutingConfig"] });
    },
    onError: (error: Error) => {
      toast.error(
        t("proxy.settings.toast.saveFailed", { error: error.message }),
      );
    },
  });
}
