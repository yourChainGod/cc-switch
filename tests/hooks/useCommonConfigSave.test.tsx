import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useEffect, useState } from "react";
import { useCommonConfigSnippet } from "@/components/providers/forms/hooks/useCommonConfigSnippet";
import { useCodexCommonConfig } from "@/components/providers/forms/hooks/useCodexCommonConfig";
import { useGeminiCommonConfig } from "@/components/providers/forms/hooks/useGeminiCommonConfig";

const getCommonConfigSnippetMock = vi.fn();
const setCommonConfigSnippetMock = vi.fn();
const extractCommonConfigSnippetMock = vi.fn();

vi.mock("@/lib/api", () => ({
  configApi: {
    getCommonConfigSnippet: (...args: unknown[]) =>
      getCommonConfigSnippetMock(...args),
    setCommonConfigSnippet: (...args: unknown[]) =>
      setCommonConfigSnippetMock(...args),
    extractCommonConfigSnippet: (...args: unknown[]) =>
      extractCommonConfigSnippetMock(...args),
  },
}));

function useClaudeCommonConfigHarness(initialData: {
  settingsConfig: Record<string, unknown>;
}) {
  const [settingsConfig, setSettingsConfig] = useState(() =>
    JSON.stringify(initialData.settingsConfig, null, 2),
  );

  useEffect(() => {
    setSettingsConfig(JSON.stringify(initialData.settingsConfig, null, 2));
  }, [initialData]);

  const common = useCommonConfigSnippet({
    settingsConfig,
    onConfigChange: setSettingsConfig,
    initialData,
    initialEnabled: true,
  });

  return { ...common, settingsConfig };
}

function useCodexCommonConfigHarness(initialData: {
  settingsConfig: Record<string, unknown>;
}) {
  const [codexConfig, setCodexConfig] = useState(() =>
    typeof initialData.settingsConfig.config === "string"
      ? initialData.settingsConfig.config
      : "",
  );

  useEffect(() => {
    setCodexConfig(
      typeof initialData.settingsConfig.config === "string"
        ? initialData.settingsConfig.config
        : "",
    );
  }, [initialData]);

  const common = useCodexCommonConfig({
    codexConfig,
    onConfigChange: setCodexConfig,
    initialData,
    initialEnabled: true,
  });

  return { ...common, codexConfig };
}

function envStringToObj(envString: string): Record<string, string> {
  return envString.split("\n").reduce<Record<string, string>>((acc, line) => {
    const index = line.indexOf("=");
    if (index <= 0) return acc;
    acc[line.slice(0, index)] = line.slice(index + 1);
    return acc;
  }, {});
}

function envObjToString(envObj: Record<string, unknown>): string {
  return Object.entries(envObj)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

function useGeminiCommonConfigHarness(initialData: {
  settingsConfig: Record<string, unknown>;
}) {
  const initialEnv =
    typeof initialData.settingsConfig.env === "object" &&
    initialData.settingsConfig.env !== null &&
    !Array.isArray(initialData.settingsConfig.env)
      ? (initialData.settingsConfig.env as Record<string, unknown>)
      : {};
  const [envValue, setEnvValue] = useState(() => envObjToString(initialEnv));

  useEffect(() => {
    const nextEnv =
      typeof initialData.settingsConfig.env === "object" &&
      initialData.settingsConfig.env !== null &&
      !Array.isArray(initialData.settingsConfig.env)
        ? (initialData.settingsConfig.env as Record<string, unknown>)
        : {};
    setEnvValue(envObjToString(nextEnv));
  }, [initialData]);

  const common = useGeminiCommonConfig({
    envValue,
    onEnvChange: setEnvValue,
    envStringToObj,
    envObjToString,
    initialData,
    initialEnabled: true,
  });

  return { ...common, envValue };
}

describe("common config snippet saving", () => {
  beforeEach(() => {
    getCommonConfigSnippetMock.mockResolvedValue("");
    setCommonConfigSnippetMock.mockResolvedValue(undefined);
    extractCommonConfigSnippetMock.mockResolvedValue("");
  });

  it("does not persist an invalid Codex common config snippet", async () => {
    const onConfigChange = vi.fn();
    const { result } = renderHook(() =>
      useCodexCommonConfig({
        codexConfig: 'model = "gpt-5"',
        onConfigChange,
      }),
    );

    await waitFor(() => expect(result.current.isLoading).toBe(false));

    let saved = false;
    act(() => {
      saved = result.current.handleCommonConfigSnippetChange(
        "base_url = https://bad.example/v1",
      );
    });

    expect(saved).toBe(false);
    expect(setCommonConfigSnippetMock).not.toHaveBeenCalled();
    expect(onConfigChange).not.toHaveBeenCalled();
    expect(result.current.commonConfigError).toContain("invalid value");
  });

  it("does not persist an invalid Gemini common config snippet", async () => {
    const onEnvChange = vi.fn();
    const { result } = renderHook(() =>
      useGeminiCommonConfig({
        envValue: "",
        onEnvChange,
        envStringToObj: () => ({}),
        envObjToString: () => "",
      }),
    );

    await waitFor(() => expect(result.current.isLoading).toBe(false));

    let saved = false;
    act(() => {
      saved = result.current.handleCommonConfigSnippetChange(
        JSON.stringify({ GEMINI_MODEL: 123 }),
      );
    });

    expect(saved).toBe(false);
    expect(setCommonConfigSnippetMock).not.toHaveBeenCalled();
    expect(onEnvChange).not.toHaveBeenCalled();
    expect(result.current.commonConfigError).toBe(
      "geminiConfig.commonConfigInvalidValues",
    );
  });

  it("re-applies Claude common config after the stored provider config is refreshed", async () => {
    getCommonConfigSnippetMock.mockImplementation(async (app: string) =>
      app === "claude" ? JSON.stringify({ includeCoAuthoredBy: false }) : "",
    );

    const first = {
      settingsConfig: { env: { ANTHROPIC_AUTH_TOKEN: "sk-first" } },
    };
    const second = {
      settingsConfig: { env: { ANTHROPIC_AUTH_TOKEN: "sk-second" } },
    };

    const { result, rerender } = renderHook(
      ({ initialData }) => useClaudeCommonConfigHarness(initialData),
      { initialProps: { initialData: first } },
    );

    await waitFor(() => {
      expect(result.current.settingsConfig).toContain("includeCoAuthoredBy");
      expect(result.current.useCommonConfig).toBe(true);
    });

    rerender({ initialData: second });

    await waitFor(() => {
      expect(result.current.settingsConfig).toContain("sk-second");
      expect(result.current.settingsConfig).toContain("includeCoAuthoredBy");
      expect(result.current.useCommonConfig).toBe(true);
    });
  });

  it("re-applies Codex common config after the stored provider config is refreshed", async () => {
    getCommonConfigSnippetMock.mockImplementation(async (app: string) =>
      app === "codex" ? "disable_response_storage = true\n" : "",
    );

    const first = {
      settingsConfig: {
        config:
          'model_provider = "custom"\n[model_providers.custom]\nbase_url = "https://first.example/v1"\n',
      },
    };
    const second = {
      settingsConfig: {
        config:
          'model_provider = "custom"\n[model_providers.custom]\nbase_url = "https://second.example/v1"\n',
      },
    };

    const { result, rerender } = renderHook(
      ({ initialData }) => useCodexCommonConfigHarness(initialData),
      { initialProps: { initialData: first } },
    );

    await waitFor(() => {
      expect(result.current.codexConfig).toContain(
        "disable_response_storage = true",
      );
      expect(result.current.useCommonConfig).toBe(true);
    });

    rerender({ initialData: second });

    await waitFor(() => {
      expect(result.current.codexConfig).toContain(
        'base_url = "https://second.example/v1"',
      );
      expect(result.current.codexConfig).toContain(
        "disable_response_storage = true",
      );
      expect(result.current.useCommonConfig).toBe(true);
    });
  });

  it("re-applies Gemini common config after the stored provider config is refreshed", async () => {
    getCommonConfigSnippetMock.mockImplementation(async (app: string) =>
      app === "gemini"
        ? JSON.stringify({ GEMINI_MODEL: "gemini-2.5-pro" })
        : "",
    );

    const first = {
      settingsConfig: { env: { GEMINI_API_KEY: "sk-first" } },
    };
    const second = {
      settingsConfig: { env: { GEMINI_API_KEY: "sk-second" } },
    };

    const { result, rerender } = renderHook(
      ({ initialData }) => useGeminiCommonConfigHarness(initialData),
      { initialProps: { initialData: first } },
    );

    await waitFor(() => {
      expect(result.current.envValue).toContain("GEMINI_MODEL=gemini-2.5-pro");
      expect(result.current.useCommonConfig).toBe(true);
    });

    rerender({ initialData: second });

    await waitFor(() => {
      expect(result.current.envValue).toContain("GEMINI_API_KEY=sk-second");
      expect(result.current.envValue).toContain("GEMINI_MODEL=gemini-2.5-pro");
      expect(result.current.useCommonConfig).toBe(true);
    });
  });
});
