import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import {
  ProviderForm,
  type ProviderFormValues,
} from "@/components/providers/forms/ProviderForm";
import type { ProviderMeta } from "@/types";
import { createTestQueryClient } from "../utils/testQueryClient";

/**
 * 回归：编辑弹窗复用同一个 ProviderForm 实例，切换编辑对象时
 * localApiFormat / localApiKeyField 这些“只在挂载时初始化”的本地状态
 * 必须随 initialData 重新同步，否则会残留上一个供应商的取值。
 *
 * 复现真实 bug：先编辑一个 apiFormat=openai_responses 的 Claude 供应商，
 * 再打开一个本应是 anthropic 的 Claude 供应商时，API 格式下拉框残留
 * openai_responses，保存即写坏 meta.apiFormat 引发错误。
 */

type SubmitHandler = (values: ProviderFormValues) => Promise<void>;

function makeClaudeData(meta: ProviderMeta, env: Record<string, string>) {
  return {
    name: "Some Claude Provider",
    websiteUrl: "",
    notes: "",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://example.com/v1",
        ...env,
      },
    },
    category: "third_party" as const,
    meta,
    icon: "",
    iconColor: "",
  };
}

function renderReusableForm(
  first: ReturnType<typeof makeClaudeData>,
) {
  const onSubmit = vi.fn<SubmitHandler>(() => Promise.resolve());

  const baseProps = {
    appId: "claude" as const,
    providerId: "p-edit",
    submitLabel: "save",
    onSubmit,
    onCancel: vi.fn(),
    onSubmittingChange: vi.fn(),
    initialData: first,
    latestMeta: first.meta,
    configKeyPatch: null,
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

  // 模拟“关闭后再打开，切换到另一个供应商”：initialData 换成新引用
  const reopenWith = (next: ReturnType<typeof makeClaudeData>) =>
    view.rerender(ui({ ...baseProps, initialData: next, latestMeta: next.meta }));

  return { onSubmit, reopenWith };
}

async function submitMeta(
  onSubmit: ReturnType<typeof renderReusableForm>["onSubmit"],
): Promise<ProviderMeta> {
  fireEvent.click(screen.getByRole("button", { name: "submit" }));
  await waitFor(() => expect(onSubmit).toHaveBeenCalled());
  const payload = onSubmit.mock.calls.at(-1)![0] as ProviderFormValues;
  return payload.meta as ProviderMeta;
}

describe("ProviderForm 切换编辑对象时 apiFormat/apiKeyField 不残留", () => {
  it("先编辑 openai_responses，再打开 anthropic：apiFormat 必须回到 anthropic", async () => {
    const { onSubmit, reopenWith } = renderReusableForm(
      makeClaudeData(
        { apiFormat: "openai_responses", commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-resp" },
      ),
    );

    reopenWith(
      makeClaudeData(
        { apiFormat: "anthropic", commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-anthropic" },
      ),
    );

    const meta = await submitMeta(onSubmit);
    expect(meta.apiFormat).toBe("anthropic");
    expect(meta.apiFormat).not.toBe("openai_responses");
  });

  it("反向：先 anthropic 再打开 openai_responses，apiFormat 必须读到新供应商的值", async () => {
    const { onSubmit, reopenWith } = renderReusableForm(
      makeClaudeData(
        { apiFormat: "anthropic", commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-anthropic" },
      ),
    );

    reopenWith(
      makeClaudeData(
        { apiFormat: "openai_responses", commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-resp" },
      ),
    );

    const meta = await submitMeta(onSubmit);
    expect(meta.apiFormat).toBe("openai_responses");
  });

  it("meta 缺失 apiFormat 时打开应退化为 anthropic，而非残留上一个的 openai_responses", async () => {
    const { onSubmit, reopenWith } = renderReusableForm(
      makeClaudeData(
        { apiFormat: "openai_responses", commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-resp" },
      ),
    );

    reopenWith(
      makeClaudeData(
        { commonConfigEnabled: false },
        { ANTHROPIC_AUTH_TOKEN: "sk-plain" },
      ),
    );

    const meta = await submitMeta(onSubmit);
    expect(meta.apiFormat).toBe("anthropic");
  });

  it("同胞 bug：apiKeyField 也必须随切换重新同步（API_KEY -> AUTH_TOKEN）", async () => {
    const { onSubmit, reopenWith } = renderReusableForm(
      makeClaudeData(
        { apiFormat: "anthropic", apiKeyField: "ANTHROPIC_API_KEY" },
        { ANTHROPIC_API_KEY: "sk-apikey" },
      ),
    );

    reopenWith(
      makeClaudeData(
        { apiFormat: "anthropic" },
        { ANTHROPIC_AUTH_TOKEN: "sk-token" },
      ),
    );

    const meta = await submitMeta(onSubmit);
    // 新供应商用 AUTH_TOKEN：apiKeyField 应回退默认（提交时默认值会省略为 undefined）
    expect(meta.apiKeyField).not.toBe("ANTHROPIC_API_KEY");
  });
});
