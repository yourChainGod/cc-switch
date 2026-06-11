/**
 * OpenClaw provider presets configuration
 * OpenClaw uses models.providers structure with custom provider configs
 */
import type {
  ProviderCategory,
  OpenClawProviderConfig,
  OpenClawDefaultModel,
} from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

/** Suggested default model configuration for a preset */
export interface OpenClawSuggestedDefaults {
  /** Default model config to apply (agents.defaults.model) */
  model?: OpenClawDefaultModel;
  /** Model catalog entries to add (agents.defaults.models) */
  modelCatalog?: Record<string, { alias?: string }>;
}

export interface OpenClawProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  apiKeyUrl?: string;
  /** OpenClaw settings_config structure */
  settingsConfig: OpenClawProviderConfig;
  isOfficial?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  category?: ProviderCategory;
  /** Template variable definitions */
  templateValues?: Record<string, TemplateValueConfig>;
  /** Visual theme config */
  theme?: PresetTheme;
  /** Icon name */
  icon?: string;
  /** Icon color */
  iconColor?: string;
  /** Mark as custom template (for UI distinction) */
  isCustomTemplate?: boolean;
  /** Suggested default model configuration */
  suggestedDefaults?: OpenClawSuggestedDefaults;
}

function rebaseOpenClawModelRef(modelRef: string, providerKey: string): string {
  const slashIndex = modelRef.indexOf("/");
  return slashIndex === -1
    ? `${providerKey}/${modelRef}`
    : `${providerKey}${modelRef.slice(slashIndex)}`;
}

/**
 * OpenClaw default model refs are stored as "<provider-key>/<model-id>".
 * Presets carry stable built-in keys for display/tests, but the real key is
 * chosen in the add-provider form, so rewrite refs right before submission.
 */
export function rebaseOpenClawSuggestedDefaults(
  defaults: OpenClawSuggestedDefaults,
  providerKey: string,
): OpenClawSuggestedDefaults {
  const key = providerKey.trim();
  if (!key) return defaults;

  return {
    model: defaults.model
      ? {
          ...defaults.model,
          primary: rebaseOpenClawModelRef(defaults.model.primary, key),
          fallbacks: defaults.model.fallbacks?.map((modelRef) =>
            rebaseOpenClawModelRef(modelRef, key),
          ),
        }
      : undefined,
    modelCatalog: defaults.modelCatalog
      ? Object.fromEntries(
          Object.entries(defaults.modelCatalog).map(([modelRef, entry]) => [
            rebaseOpenClawModelRef(modelRef, key),
            entry,
          ]),
        )
      : undefined,
  };
}

/**
 * OpenClaw API protocol options
 * @see https://github.com/openclaw/openclaw/blob/main/docs/gateway/configuration.md
 */
export const openclawApiProtocols = [
  { value: "openai-completions", label: "OpenAI Completions" },
  { value: "openai-responses", label: "OpenAI Responses" },
  { value: "anthropic-messages", label: "Anthropic Messages" },
  { value: "google-generative-ai", label: "Google Generative AI" },
  { value: "bedrock-converse-stream", label: "AWS Bedrock" },
] as const;

/**
 * OpenClaw provider presets list
 */
export const openclawProviderPresets: OpenClawProviderPreset[] = [
  // ========== Custom Template ==========
  {
    name: "OpenAI Compatible",
    websiteUrl: "",
    settingsConfig: {
      baseUrl: "",
      apiKey: "",
      api: "openai-completions",
      models: [],
    },
    category: "custom",
    isCustomTemplate: true,
    icon: "generic",
    iconColor: "#6B7280",
    templateValues: {
      baseUrl: {
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
];
