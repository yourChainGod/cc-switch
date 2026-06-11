import type { ProviderCategory, OpenCodeProviderConfig } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

export interface OpenCodeProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: OpenCodeProviderConfig;
  isOfficial?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  category?: ProviderCategory;
  templateValues?: Record<string, TemplateValueConfig>;
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
  isCustomTemplate?: boolean;
}

export const opencodeNpmPackages = [
  { value: "@ai-sdk/openai", label: "OpenAI Responses" },
  { value: "@ai-sdk/openai-compatible", label: "OpenAI Compatible" },
  { value: "@ai-sdk/anthropic", label: "Anthropic" },
  { value: "@ai-sdk/amazon-bedrock", label: "Amazon Bedrock" },
  { value: "@ai-sdk/google", label: "Google (Gemini)" },
] as const;

export interface PresetModelVariant {
  id: string;
  name?: string;
  contextLimit?: number;
  outputLimit?: number;
  modalities?: { input: string[]; output: string[] };
  options?: Record<string, unknown>;
  variants?: Record<string, Record<string, unknown>>;
}

export const OPENCODE_PRESET_MODEL_VARIANTS: Record<
  string,
  PresetModelVariant[]
> = {
  "@ai-sdk/openai-compatible": [
    {
      id: "MiniMax-M2.7",
      name: "MiniMax M2.7",
      contextLimit: 204800,
      outputLimit: 131072,
      modalities: { input: ["text"], output: ["text"] },
    },
    {
      id: "glm-5.1",
      name: "GLM 5.1",
      contextLimit: 204800,
      outputLimit: 131072,
      modalities: { input: ["text"], output: ["text"] },
    },
    {
      id: "kimi-k2.6",
      name: "Kimi K2.6",
      contextLimit: 262144,
      outputLimit: 262144,
      modalities: { input: ["text", "image", "video"], output: ["text"] },
    },
    {
      id: "step-3.5-flash-2603",
      name: "Step 3.5 Flash 2603",
      contextLimit: 262144,
    },
    {
      id: "step-3.5-flash",
      name: "Step 3.5 Flash",
      contextLimit: 262144,
    },
  ],
  "@ai-sdk/google": [
    {
      id: "gemini-2.5-flash-lite",
      name: "Gemini 2.5 Flash Lite",
      contextLimit: 1048576,
      outputLimit: 65536,
      modalities: {
        input: ["text", "image", "pdf", "video", "audio"],
        output: ["text"],
      },
      variants: {
        auto: {
          thinkingConfig: { includeThoughts: true, thinkingBudget: -1 },
        },
        "no-thinking": { thinkingConfig: { thinkingBudget: 0 } },
      },
    },
    {
      id: "gemini-3.5-flash",
      name: "Gemini 3.5 Flash",
      contextLimit: 1048576,
      outputLimit: 65536,
      modalities: {
        input: ["text", "image", "pdf", "video", "audio"],
        output: ["text"],
      },
      variants: {
        minimal: {
          thinkingConfig: { includeThoughts: true, thinkingLevel: "minimal" },
        },
        low: {
          thinkingConfig: { includeThoughts: true, thinkingLevel: "low" },
        },
        medium: {
          thinkingConfig: { includeThoughts: true, thinkingLevel: "medium" },
        },
        high: {
          thinkingConfig: { includeThoughts: true, thinkingLevel: "high" },
        },
      },
    },
  ],
  "@ai-sdk/openai": [
    {
      id: "gpt-5.5",
      name: "GPT-5.5",
      contextLimit: 400000,
      outputLimit: 128000,
      modalities: { input: ["text", "image"], output: ["text"] },
      variants: {
        low: {
          reasoningEffort: "low",
          reasoningSummary: "auto",
          textVerbosity: "medium",
        },
        medium: {
          reasoningEffort: "medium",
          reasoningSummary: "auto",
          textVerbosity: "medium",
        },
        high: {
          reasoningEffort: "high",
          reasoningSummary: "auto",
          textVerbosity: "medium",
        },
        xhigh: {
          reasoningEffort: "xhigh",
          reasoningSummary: "auto",
          textVerbosity: "medium",
        },
      },
    },
  ],
  "@ai-sdk/amazon-bedrock": [
    {
      id: "global.anthropic.claude-opus-4-8",
      name: "Claude Opus 4.8",
      contextLimit: 1000000,
      outputLimit: 128000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
    },
    {
      id: "global.anthropic.claude-sonnet-4-6",
      name: "Claude Sonnet 4.6",
      contextLimit: 1000000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
    },
    {
      id: "global.anthropic.claude-haiku-4-5-20251001-v1:0",
      name: "Claude Haiku 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
    },
    {
      id: "us.amazon.nova-pro-v1:0",
      name: "Amazon Nova Pro",
      contextLimit: 300000,
      outputLimit: 5000,
      modalities: { input: ["text", "image"], output: ["text"] },
    },
    {
      id: "us.meta.llama4-maverick-17b-instruct-v1:0",
      name: "Meta Llama 4 Maverick",
      contextLimit: 131072,
      outputLimit: 131072,
      modalities: { input: ["text"], output: ["text"] },
    },
    {
      id: "us.deepseek.r1-v1:0",
      name: "DeepSeek R1",
      contextLimit: 131072,
      outputLimit: 131072,
      modalities: { input: ["text"], output: ["text"] },
    },
  ],
  "@ai-sdk/anthropic": [
    {
      id: "claude-sonnet-4-5-20250929",
      name: "Claude Sonnet 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
      variants: {
        low: { effort: "low" },
        medium: { effort: "medium" },
        high: { effort: "high" },
      },
    },
    {
      id: "claude-opus-4-5-20251101",
      name: "Claude Opus 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
      variants: {
        low: { thinking: { budgetTokens: 5000, type: "enabled" } },
        medium: { thinking: { budgetTokens: 13000, type: "enabled" } },
        high: { thinking: { budgetTokens: 18000, type: "enabled" } },
      },
    },
    {
      id: "claude-opus-4-8",
      name: "Claude Opus 4.8",
      contextLimit: 1000000,
      outputLimit: 128000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
      variants: {
        low: { effort: "low" },
        medium: { effort: "medium" },
        high: { effort: "high" },
        max: { effort: "max" },
      },
    },
    {
      id: "claude-haiku-4-5-20251001",
      name: "Claude Haiku 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
    },
    {
      id: "gemini-claude-opus-4-5-thinking",
      name: "Antigravity - Claude Opus 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
      variants: {
        low: { effort: "low" },
        medium: { effort: "medium" },
        high: { effort: "high" },
      },
    },
    {
      id: "gemini-claude-sonnet-4-5-thinking",
      name: "Antigravity - Claude Sonnet 4.5",
      contextLimit: 200000,
      outputLimit: 64000,
      modalities: { input: ["text", "image", "pdf"], output: ["text"] },
      variants: {
        low: { thinking: { budgetTokens: 5000, type: "enabled" } },
        medium: { thinking: { budgetTokens: 13000, type: "enabled" } },
        high: { thinking: { budgetTokens: 18000, type: "enabled" } },
      },
    },
  ],
};

/**
 * Look up preset metadata for a model by npm package and model ID.
 * Returns enrichment fields (options, limit, modalities) that can be
 * merged into a model definition when the user's config doesn't already
 * provide them.
 */
export function getPresetModelDefaults(
  npm: string,
  modelId: string,
): PresetModelVariant | undefined {
  const models = OPENCODE_PRESET_MODEL_VARIANTS[npm];
  if (!models) return undefined;
  return models.find((m) => m.id === modelId);
}

export const opencodeProviderPresets: OpenCodeProviderPreset[] = [
  {
    name: "OpenAI Compatible",
    websiteUrl: "",
    settingsConfig: {
      npm: "@ai-sdk/openai-compatible",
      options: {
        baseURL: "",
        apiKey: "",
        setCacheKey: true,
      },
      models: {},
    },
    category: "custom",
    isCustomTemplate: true,
    icon: "generic",
    iconColor: "#6B7280",
    templateValues: {
      baseURL: {
        label: "Base URL",
        placeholder: "https://api.example.com/v1",
        editorValue: "",
      },
      apiKey: {
        label: "API Key",
        placeholder: "",
        editorValue: "",
      },
    },
  },

  {
    name: "Oh My OpenCode",
    websiteUrl: "https://github.com/code-yeongyu/oh-my-openagent",
    settingsConfig: {
      npm: "",
      options: {},
      models: {},
    },
    category: "omo" as ProviderCategory,
    icon: "opencode",
    iconColor: "#8B5CF6",
    isCustomTemplate: true,
  },
  {
    name: "Oh My OpenCode Slim",
    websiteUrl: "https://github.com/alvinunreal/oh-my-opencode-slim",
    settingsConfig: {
      npm: "",
      options: {},
      models: {},
    },
    category: "omo-slim" as ProviderCategory,
    icon: "opencode",
    iconColor: "#6366F1",
    isCustomTemplate: true,
  },
];
