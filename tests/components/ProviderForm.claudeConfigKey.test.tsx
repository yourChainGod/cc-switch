import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import {
  ProviderForm,
  type ProviderConfigKeyPatch,
  type ProviderFormValues,
} from "@/components/providers/forms/ProviderForm";
import type { ProviderMeta } from "@/types";
import { createTestQueryClient } from "../utils/testQueryClient";

/**
 * 回归：claude 供应商在 Key 池切 key 后，apiFormat（API 格式）绝不能漂移。
 * 复现 anyrouter 场景：用户设置 ANTHROPIC 格式，切 key 只能换 key 值与认证字段，
 * 不能把 apiFormat 变成 openai_responses。
 */

interface HarnessProps {
  initialMeta: ProviderMeta;
  initialEnv: Record<string, string>;
  category?: string;
}

function renderClaudeForm({
  initialMeta,
  initialEnv,
  category = "third_party",
}: HarnessProps) {
  const onSubmit = vi.fn<[ProviderFormValues], Promise<void>>(() =>
    Promise.resolve(),
  );

  const initialData = {
    name: "Anyrouter",
    websiteUrl: "",
    notes: "",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://anyrouter.example/v1",
        ...initialEnv,
      },
    },
    category,
    meta: initialMeta,
    icon: "",
    iconColor: "",
  };

  const baseProps = {
    appId: "claude" as const,
    providerId: "anyrouter",
    submitLabel: "save",
    onSubmit,
    onCancel: vi.fn(),
    onSubmittingChange: vi.fn(),
    initialData,
    latestMeta: initialMeta,
    configKeyPatch: null as ProviderConfigKeyPatch | null,
    showButtons: false,
  };

  const queryClient = createTestQueryClient();
  const ui = (props: typeof baseProps) => (
    <QueryClientProvider client={queryClient}>
      <ProviderForm {...props} />
      <button type="submit" form="provider-form">
        submit
      </button>
    </QueryClientProvider>
  );

  const view = render(ui(baseProps));
  const rerenderWith = (patch: Partial<typeof baseProps>) =>
    view.rerender(ui({ ...baseProps, ...patch }));

  return { onSubmit, rerenderWith };
}

async function submitAndGetMeta(
  onSubmit: ReturnType<typeof vi.fn>,
): Promise<{ meta: ProviderMeta; settingsConfig: Record<string, unknown> }> {
  fireEvent.click(screen.getByRole("button", { name: "submit" }));
  await waitFor(() => expect(onSubmit).toHaveBeenCalled());
  const payload = onSubmit.mock.calls[0][0] as ProviderFormValues;
  return {
    meta: payload.meta as ProviderMeta,
    settingsConfig: JSON.parse(payload.settingsConfig),
  };
}

describe("ProviderForm claude 切 key 不漂移 apiFormat", () => {
  it("apiFormat=anthropic 时切 key 只换 key 值，apiFormat 保持 anthropic", async () => {
    const { onSubmit, rerenderWith } = renderClaudeForm({
      initialMeta: { apiFormat: "anthropic", commonConfigEnabled: false },
      initialEnv: { ANTHROPIC_AUTH_TOKEN: "sk-old" },
    });

    // 模拟 Key 池切 key：注入 configKeyPatch + 后端返回的最新 meta（仍 anthropic）
    rerenderWith({
      latestMeta: {
        apiFormat: "anthropic",
        commonConfigEnabled: false,
        configKeyId: "key-new",
        configKeyMode: "manual",
      },
      configKeyPatch: {
        id: 1,
        keyValue: "sk-new",
        authField: "ANTHROPIC_AUTH_TOKEN",
      },
    });

    const { meta, settingsConfig } = await submitAndGetMeta(onSubmit);
    expect(meta.apiFormat).toBe("anthropic");
    expect((settingsConfig.env as Record<string, string>).ANTHROPIC_AUTH_TOKEN).toBe(
      "sk-new",
    );
  });

  it("meta 缺失 apiFormat 时切 key 退化为 anthropic，绝不会变 openai_responses", async () => {
    const { onSubmit, rerenderWith } = renderClaudeForm({
      initialMeta: { commonConfigEnabled: false },
      initialEnv: { ANTHROPIC_AUTH_TOKEN: "sk-old" },
    });

    rerenderWith({
      latestMeta: {
        commonConfigEnabled: false,
        configKeyId: "key-new",
        configKeyMode: "auto",
      },
      configKeyPatch: {
        id: 1,
        keyValue: "sk-new",
        authField: "ANTHROPIC_AUTH_TOKEN",
      },
    });

    const { meta } = await submitAndGetMeta(onSubmit);
    expect(meta.apiFormat).toBe("anthropic");
    expect(meta.apiFormat).not.toBe("openai_responses");
  });

  it("连续切多个 key，apiFormat 始终 anthropic", async () => {
    const { onSubmit, rerenderWith } = renderClaudeForm({
      initialMeta: { apiFormat: "anthropic", commonConfigEnabled: false },
      initialEnv: { ANTHROPIC_AUTH_TOKEN: "sk-old" },
    });

    for (let i = 1; i <= 3; i++) {
      rerenderWith({
        latestMeta: {
          apiFormat: "anthropic",
          commonConfigEnabled: false,
          configKeyId: `key-${i}`,
          configKeyMode: "manual",
        },
        configKeyPatch: {
          id: i,
          keyValue: `sk-${i}`,
          authField: "ANTHROPIC_AUTH_TOKEN",
        },
      });
    }

    const { meta } = await submitAndGetMeta(onSubmit);
    expect(meta.apiFormat).toBe("anthropic");
  });
});
