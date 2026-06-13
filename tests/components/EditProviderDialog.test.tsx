import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect, useState, type ReactElement } from "react";
import type { Provider } from "@/types";

const apiMocks = vi.hoisted(() => ({
  get: vi.fn(),
  getCurrent: vi.fn(),
  getKeys: vi.fn(),
  addKey: vi.fn(),
  setConfigKey: vi.fn(),
  setConfigKeyAuto: vi.fn(),
  getLiveProviderSettings: vi.fn(),
  getOpenClawLiveProvider: vi.fn(),
}));

vi.mock("@/lib/api", () => ({
  providersApi: {
    get: apiMocks.get,
    getCurrent: apiMocks.getCurrent,
    getKeys: apiMocks.getKeys,
    addKey: apiMocks.addKey,
    setConfigKey: apiMocks.setConfigKey,
    setConfigKeyAuto: apiMocks.setConfigKeyAuto,
  },
  vscodeApi: {
    getLiveProviderSettings: apiMocks.getLiveProviderSettings,
  },
  openclawApi: {
    getLiveProvider: apiMocks.getOpenClawLiveProvider,
  },
}));

vi.mock("@/components/common/FullScreenPanel", () => ({
  FullScreenPanel: ({
    isOpen,
    children,
    footer,
  }: {
    isOpen: boolean;
    children: React.ReactNode;
    footer?: React.ReactNode;
  }) =>
    isOpen ? (
      <div>
        <div>{children}</div>
        <div>{footer}</div>
      </div>
    ) : null,
}));

vi.mock("@/components/providers/forms/ProviderForm", () => ({
  // 模拟真实 ProviderForm 中 ApiKeySection 的行为：消费 KeyPoolEntryContext
  // 暴露“管理 Key 池”入口（真实实现位于 ApiKeySection）
  ProviderForm: ({
    initialData,
    onSubmit,
    isProxyTakeover,
    latestMeta,
    configKeyPatch,
  }: {
    initialData: {
      name?: string;
      websiteUrl?: string;
      notes?: string;
      settingsConfig?: Record<string, unknown>;
      meta?: Record<string, unknown>;
      icon?: string;
      iconColor?: string;
    };
    latestMeta?: Record<string, unknown>;
    configKeyPatch?: {
      id: number;
      keyValue: string;
      authField?: string;
    } | null;
    onSubmit: (values: {
      name: string;
      websiteUrl: string;
      notes?: string;
      settingsConfig: string;
      meta?: Record<string, unknown>;
      icon?: string;
      iconColor?: string;
    }) => void;
    isProxyTakeover?: boolean;
  }) => {
    const keyPoolEntry = useKeyPoolEntry();
    const [settingsConfig, setSettingsConfig] = useState<
      Record<string, unknown>
    >(() => initialData.settingsConfig ?? {});

    useEffect(() => {
      setSettingsConfig(initialData.settingsConfig ?? {});
    }, [initialData]);

    useEffect(() => {
      if (!configKeyPatch) return;
      setSettingsConfig((previous) => {
        const next = JSON.parse(JSON.stringify(previous)) as Record<
          string,
          unknown
        >;
        const authField =
          configKeyPatch.authField === "ANTHROPIC_API_KEY"
            ? "ANTHROPIC_API_KEY"
            : "ANTHROPIC_AUTH_TOKEN";
        const env =
          typeof next.env === "object" && next.env !== null
            ? (next.env as Record<string, unknown>)
            : {};
        if (authField === "ANTHROPIC_API_KEY") {
          delete env.ANTHROPIC_AUTH_TOKEN;
        } else {
          delete env.ANTHROPIC_API_KEY;
        }
        env[authField] = configKeyPatch.keyValue;
        next.env = env;
        return next;
      });
    }, [configKeyPatch?.id]);

    return (
      <form
        id="provider-form"
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit({
            name: initialData.name ?? "",
            websiteUrl: initialData.websiteUrl ?? "",
            notes: initialData.notes,
            settingsConfig: JSON.stringify(settingsConfig),
            meta: latestMeta ?? initialData.meta,
            icon: initialData.icon,
            iconColor: initialData.iconColor,
          });
        }}
      >
        <output data-testid="settings-config">
          {JSON.stringify(settingsConfig)}
        </output>
        <output data-testid="is-proxy-takeover">
          {isProxyTakeover ? "true" : "false"}
        </output>
        <output data-testid="key-pool-total">
          {keyPoolEntry ? String(keyPoolEntry.total) : "none"}
        </output>
        <output data-testid="key-pool-mode">
          {keyPoolEntry ? keyPoolEntry.configKeyMode : "none"}
        </output>
        <button
          type="button"
          data-testid="open-key-pool"
          onClick={() => keyPoolEntry?.open()}
        >
          open key pool
        </button>
        <button
          type="button"
          data-testid="edit-custom-config"
          onClick={() =>
            setSettingsConfig((previous) => ({
              ...previous,
              customConfig: "draft-value",
            }))
          }
        >
          edit custom config
        </button>
      </form>
    );
  },
}));

import { EditProviderDialog } from "@/components/providers/EditProviderDialog";
import { useKeyPoolEntry } from "@/components/providers/keyPool/KeyPoolEntryContext";

async function openKeyPoolDialog() {
  fireEvent.click(await screen.findByTestId("open-key-pool"));
}

function renderWithQueryClient(ui: ReactElement) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });

  return render(
    <QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>,
  );
}

describe("EditProviderDialog", () => {
  beforeEach(() => {
    apiMocks.get.mockReset();
    apiMocks.get.mockImplementation(
      async (appId: string, providerId: string) => ({
        id: providerId,
        name: "Updated Provider",
        category: "custom",
        settingsConfig:
          appId === "codex"
            ? {
                auth: {
                  OPENAI_API_KEY: "sk-codex-embedded",
                },
                config: 'model_provider = "custom"\n',
              }
            : {
                env: {
                  ANTHROPIC_AUTH_TOKEN: "sk-ant-embedded",
                },
              },
        meta: {
          configKeyId: "key-1",
          configKeyMode: "manual",
        },
      }),
    );
    apiMocks.getCurrent.mockReset();
    apiMocks.getKeys.mockReset();
    apiMocks.getKeys.mockResolvedValue([]);
    apiMocks.addKey.mockReset();
    apiMocks.addKey.mockResolvedValue({
      id: "key-1",
      appType: "claude",
      providerId: "anthropic",
      name: "Imported key",
      keyValue: "sk-ant-1",
      enabled: true,
      priority: 0,
      weight: 1,
      status: "active",
      consecutiveFailures: 0,
      createdAt: 1,
      updatedAt: 1,
    });
    apiMocks.setConfigKey.mockReset();
    apiMocks.setConfigKey.mockImplementation(
      async (appId: string, providerId: string, keyId: string) => ({
        id: providerId,
        name: "Updated Provider",
        category: "custom",
        settingsConfig:
          appId === "codex"
            ? {
                auth: {
                  OPENAI_API_KEY: "sk-codex-embedded",
                },
                config: 'model_provider = "custom"\n',
              }
            : {
                env: {
                  ANTHROPIC_AUTH_TOKEN: "sk-ant-embedded",
                },
              },
        meta: {
          configKeyId: keyId,
          configKeyMode: "manual",
        },
      }),
    );
    apiMocks.setConfigKeyAuto.mockReset();
    apiMocks.setConfigKeyAuto.mockImplementation(
      async (appId: string, providerId: string) => ({
        id: providerId,
        name: "Updated Provider",
        category: "custom",
        settingsConfig:
          appId === "codex"
            ? {
                auth: {
                  OPENAI_API_KEY: "sk-codex-embedded",
                },
                config: 'model_provider = "custom"\n',
              }
            : {
                env: {
                  ANTHROPIC_AUTH_TOKEN: "sk-ant-auto",
                },
              },
        meta: {
          configKeyId: "key-auto",
          configKeyMode: "auto",
        },
      }),
    );
    apiMocks.getLiveProviderSettings.mockReset();
    apiMocks.getOpenClawLiveProvider.mockReset();
  });

  it("保留 Codex 数据库中的 modelCatalog，避免 live 配置缺字段时清空模型映射", async () => {
    const dbModelCatalog = {
      models: [
        {
          model: "deepseek-v4-flash",
          displayName: "DeepSeek V4 Flash",
          contextWindow: 1000000,
        },
      ],
    };
    const provider: Provider = {
      id: "deepseek",
      name: "DeepSeek",
      category: "aggregator",
      settingsConfig: {
        auth: {
          OPENAI_API_KEY: "db-key",
        },
        config: 'model_provider = "custom"\nmodel = "deepseek-v4-flash"\n',
        modelCatalog: dbModelCatalog,
      },
    };
    const liveSettings = {
      auth: {
        OPENAI_API_KEY: "live-key",
      },
      config: 'model_provider = "custom"\nmodel = "deepseek-v4-pro"\n',
    };
    const handleSubmit = vi.fn().mockResolvedValue(undefined);

    apiMocks.getCurrent.mockResolvedValue(provider.id);
    apiMocks.getLiveProviderSettings.mockResolvedValue(liveSettings);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={handleSubmit}
        appId="codex"
      />,
    );

    await waitFor(() => {
      expect(
        JSON.parse(screen.getByTestId("settings-config").textContent ?? "{}"),
      ).toEqual({
        ...liveSettings,
        modelCatalog: dbModelCatalog,
      });
    });

    fireEvent.click(screen.getByRole("button", { name: "common.save" }));

    await waitFor(() => expect(handleSubmit).toHaveBeenCalledTimes(1));
    expect(handleSubmit.mock.calls[0][0].provider.settingsConfig).toEqual({
      ...liveSettings,
      modelCatalog: dbModelCatalog,
    });
  });

  it("代理接管中编辑 Codex 供应商时展示数据库配置而不是读取 live 代理配置", async () => {
    const provider: Provider = {
      id: "deepseek",
      name: "DeepSeek",
      category: "custom",
      settingsConfig: {
        auth: {
          OPENAI_API_KEY: "db-key",
        },
        config:
          'model_provider = "custom"\n[model_providers.custom]\nbase_url = "https://api.deepseek.com/v1"\n',
      },
    };

    apiMocks.getCurrent.mockResolvedValue(provider.id);
    apiMocks.getLiveProviderSettings.mockResolvedValue({
      auth: {
        OPENAI_API_KEY: "PROXY_MANAGED",
      },
      config:
        'model_provider = "custom"\n[model_providers.custom]\nbase_url = "http://127.0.0.1:15721/v1"\nexperimental_bearer_token = "PROXY_MANAGED"\n',
    });

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="codex"
        isProxyTakeover
      />,
    );

    await waitFor(() => {
      expect(screen.getByTestId("is-proxy-takeover").textContent).toBe("true");
    });

    expect(apiMocks.getLiveProviderSettings).not.toHaveBeenCalled();
    expect(
      JSON.parse(screen.getByTestId("settings-config").textContent ?? "{}"),
    ).toEqual(provider.settingsConfig);
  });

  it("key pool 为空时可从已有嵌入 key 导入", async () => {
    const provider: Provider = {
      id: "anthropic",
      name: "Anthropic",
      category: "official",
      settingsConfig: {
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-ant-embedded",
        },
      },
    };
    apiMocks.getKeys.mockResolvedValueOnce([]).mockResolvedValueOnce([
      {
        id: "key-1",
        appType: "claude",
        providerId: "anthropic",
        name: "Imported key",
        keyValue: "sk-ant-embedded",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 0,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 1,
        updatedAt: 1,
      },
    ]);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="claude"
      />,
    );

    await openKeyPoolDialog();

    const importButton = await screen.findByRole("button", {
      name: "Import Existing Key",
    });
    fireEvent.click(importButton);

    await waitFor(() => {
      expect(apiMocks.addKey).toHaveBeenCalledWith("claude", "anthropic", {
        name: "Imported key",
        keyValue: "sk-ant-embedded",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 0,
        weight: 1,
      });
    });
    await waitFor(() => {
      expect(apiMocks.get).toHaveBeenCalledWith("claude", "anthropic");
    });
    expect(apiMocks.setConfigKey).not.toHaveBeenCalled();
    expect(await screen.findByText("Config key")).toBeInTheDocument();
  });

  it("已有 key 或代理占位 key 时不显示嵌入 key 导入", async () => {
    const provider: Provider = {
      id: "anthropic",
      name: "Anthropic",
      category: "official",
      settingsConfig: {
        env: {
          ANTHROPIC_AUTH_TOKEN: "PROXY_MANAGED",
        },
      },
    };

    const { unmount } = renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="claude"
      />,
    );

    await openKeyPoolDialog();
    await screen.findByText("No provider keys configured");
    expect(
      screen.queryByRole("button", { name: "Import Existing Key" }),
    ).not.toBeInTheDocument();

    unmount();

    apiMocks.getKeys.mockResolvedValueOnce([
      {
        id: "key-existing",
        appType: "claude",
        providerId: "anthropic",
        name: "Existing",
        keyValue: "sk-existing",
        enabled: true,
        priority: 0,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 1,
        updatedAt: 1,
      },
    ]);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={{
          ...provider,
          settingsConfig: {
            env: {
              ANTHROPIC_AUTH_TOKEN: "sk-ant-embedded",
            },
          },
        }}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="claude"
      />,
    );

    await openKeyPoolDialog();
    await screen.findByText("Existing");
    expect(
      screen.queryByRole("button", { name: "Import Existing Key" }),
    ).not.toBeInTheDocument();
  });

  it("Codex 可从 auth.OPENAI_API_KEY 导入嵌入 key", async () => {
    const provider: Provider = {
      id: "codex-provider",
      name: "Codex Provider",
      category: "custom",
      settingsConfig: {
        auth: {
          OPENAI_API_KEY: "sk-codex-embedded",
        },
        config: 'model_provider = "custom"\n',
      },
    };
    apiMocks.getKeys.mockResolvedValueOnce([]).mockResolvedValueOnce([
      {
        id: "key-1",
        appType: "codex",
        providerId: "codex-provider",
        name: "Imported key",
        keyValue: "sk-codex-embedded",
        authField: "OPENAI_API_KEY",
        enabled: true,
        priority: 0,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 1,
        updatedAt: 1,
      },
    ]);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="codex"
      />,
    );

    await openKeyPoolDialog();

    fireEvent.click(
      await screen.findByRole("button", { name: "Import Existing Key" }),
    );

    await waitFor(() => {
      expect(apiMocks.addKey).toHaveBeenCalledWith("codex", "codex-provider", {
        name: "Imported key",
        keyValue: "sk-codex-embedded",
        authField: "OPENAI_API_KEY",
        enabled: true,
        priority: 0,
        weight: 1,
      });
    });
    await waitFor(() => {
      expect(apiMocks.get).toHaveBeenCalledWith("codex", "codex-provider");
    });
    expect(apiMocks.setConfigKey).not.toHaveBeenCalled();
    expect(await screen.findByText("Config key")).toBeInTheDocument();
  });

  it("可将手动指定的配置 Key 改回自动跟随优先级", async () => {
    const provider: Provider = {
      id: "anthropic",
      name: "Anthropic",
      category: "official",
      settingsConfig: {
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-manual",
        },
      },
      meta: {
        configKeyId: "key-manual",
        configKeyMode: "manual",
      },
    };
    const keys = [
      {
        id: "key-manual",
        appType: "claude",
        providerId: "anthropic",
        name: "Manual key",
        keyValue: "sk-manual",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 10,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 1,
        updatedAt: 1,
      },
      {
        id: "key-auto",
        appType: "claude",
        providerId: "anthropic",
        name: "Auto key",
        keyValue: "sk-auto",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 1,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 2,
        updatedAt: 2,
      },
    ];
    apiMocks.getKeys.mockResolvedValue(keys);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={vi.fn()}
        appId="claude"
      />,
    );

    await openKeyPoolDialog();

    expect(await screen.findByText("Manual")).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: "Follow priority automatically",
      }),
    );

    await waitFor(() => {
      expect(apiMocks.setConfigKeyAuto).toHaveBeenCalledWith(
        "claude",
        "anthropic",
      );
    });
    expect(await screen.findByText("Auto")).toBeInTheDocument();
  });

  it("切换配置 Key 只替换 API Key，不覆盖正在编辑的自定义配置", async () => {
    const provider: Provider = {
      id: "anthropic",
      name: "Anthropic",
      category: "custom",
      settingsConfig: {
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-old",
        },
        preserved: "stored-value",
      },
      meta: {
        configKeyId: "key-old",
        configKeyMode: "manual",
      },
    };
    apiMocks.getKeys.mockResolvedValue([
      {
        id: "key-old",
        appType: "claude",
        providerId: "anthropic",
        name: "Old key",
        keyValue: "sk-old",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 0,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 1,
        updatedAt: 1,
      },
      {
        id: "key-new",
        appType: "claude",
        providerId: "anthropic",
        name: "New key",
        keyValue: "sk-new",
        authField: "ANTHROPIC_AUTH_TOKEN",
        enabled: true,
        priority: 1,
        weight: 1,
        status: "active",
        consecutiveFailures: 0,
        createdAt: 2,
        updatedAt: 2,
      },
    ]);
    apiMocks.setConfigKey.mockResolvedValueOnce({
      ...provider,
      settingsConfig: {
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-new",
        },
      },
      meta: {
        configKeyId: "key-new",
        configKeyMode: "manual",
      },
    });
    const handleSubmit = vi.fn().mockResolvedValue(undefined);

    renderWithQueryClient(
      <EditProviderDialog
        open
        provider={provider}
        onOpenChange={vi.fn()}
        onSubmit={handleSubmit}
        appId="claude"
      />,
    );

    fireEvent.click(await screen.findByTestId("edit-custom-config"));
    expect(
      JSON.parse(screen.getByTestId("settings-config").textContent ?? "{}"),
    ).toMatchObject({
      customConfig: "draft-value",
      preserved: "stored-value",
    });

    await openKeyPoolDialog();
    expect(await screen.findByText("New key")).toBeInTheDocument();

    const useKeyButtons = screen.getAllByRole("button", {
      name: "Use this key",
    });
    fireEvent.click(useKeyButtons[1]);

    await waitFor(() => {
      expect(apiMocks.setConfigKey).toHaveBeenCalledWith(
        "claude",
        "anthropic",
        "key-new",
      );
    });

    await waitFor(() => {
      expect(
        JSON.parse(screen.getByTestId("settings-config").textContent ?? "{}"),
      ).toMatchObject({
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-new",
        },
        customConfig: "draft-value",
        preserved: "stored-value",
      });
    });

    fireEvent.click(screen.getByRole("button", { name: /关闭|Close/ }));
    fireEvent.click(screen.getByRole("button", { name: "common.save" }));
    await waitFor(() => expect(handleSubmit).toHaveBeenCalledTimes(1));
    expect(handleSubmit.mock.calls[0][0].provider.settingsConfig).toMatchObject(
      {
        env: {
          ANTHROPIC_AUTH_TOKEN: "sk-new",
        },
        customConfig: "draft-value",
        preserved: "stored-value",
      },
    );
    expect(handleSubmit.mock.calls[0][0].provider.meta).toMatchObject({
      configKeyId: "key-new",
      configKeyMode: "manual",
    });
  });
});
