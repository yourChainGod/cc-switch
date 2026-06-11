/**
 * Hermes Agent provider presets configuration
 * Hermes uses custom_providers array in config.yaml
 */
import type { ProviderCategory } from "../types";
import type { PresetTheme, TemplateValueConfig } from "./claudeProviderPresets";

/**
 * Marker field and source values that `hermes_config.rs::get_providers`
 * injects onto each settings payload. Kept in sync with the Rust constants
 * `PROVIDER_SOURCE_FIELD` / `PROVIDER_SOURCE_CUSTOM_LIST` / `PROVIDER_SOURCE_DICT`.
 */
export const HERMES_PROVIDER_SOURCE_FIELD = "_cc_source";
export const HERMES_PROVIDER_SOURCE_CUSTOM_LIST = "custom_providers";
export const HERMES_PROVIDER_SOURCE_DICT = "providers_dict";

/**
 * True when the provider was sourced from Hermes' v12+ `providers:` dict —
 * CC Switch renders those read-only and routes edits to Hermes Web UI.
 */
export function isHermesReadOnlyProvider(settingsConfig: unknown): boolean {
  if (!settingsConfig || typeof settingsConfig !== "object") {
    return false;
  }
  const marker = (settingsConfig as Record<string, unknown>)[
    HERMES_PROVIDER_SOURCE_FIELD
  ];
  return marker === HERMES_PROVIDER_SOURCE_DICT;
}

/**
 * A model entry under a Hermes custom_provider.
 *
 * Serialized to YAML as a dict keyed by `id`:
 *
 * ```yaml
 * models:
 *   anthropic/claude-opus-4-8:
 *     context_length: 200000
 * ```
 *
 * Hermes' `_VALID_CUSTOM_PROVIDER_FIELDS` (hermes_cli/config.py) does not include
 * `max_tokens` at the per-model level — writing it produces an "unknown field"
 * warning on Hermes startup. Max tokens is a per-request parameter, not a
 * provider-level config.
 */
export interface HermesModel {
  /** Model ID — becomes the YAML key and the value written to top-level model.default. */
  id: string;
  /** Optional display label (UI only, not serialized to YAML). */
  name?: string;
  /** Override the auto-detected context window. */
  context_length?: number;
}

/**
 * Top-level `model:` defaults suggested by a preset.
 *
 * Written to the YAML `model:` section when the user switches to this provider.
 * Per-model `context_length` lives on the individual `HermesModel` entries and
 * flows through `custom_providers[].models`, not this object.
 */
export interface HermesSuggestedDefaults {
  model: {
    /** Model ID for `model.default`. Typically equals `models[0].id`. */
    default: string;
    /** Value for `model.provider`. Omit to use the custom_provider name. */
    provider?: string;
  };
}

/** Hermes custom_provider protocol mode. Always written explicitly. */
export type HermesApiMode =
  | "chat_completions"
  | "anthropic_messages"
  | "codex_responses"
  | "bedrock_converse";

/** Default mode used when a provider has no stored value yet. */
export const HERMES_DEFAULT_API_MODE: HermesApiMode = "chat_completions";

/** Dropdown options for the API Mode selector. `labelKey` is looked up in i18n. */
export const hermesApiModes: Array<{
  value: HermesApiMode;
  labelKey: string;
}> = [
  { value: "chat_completions", labelKey: "hermes.form.apiModeChatCompletions" },
  {
    value: "anthropic_messages",
    labelKey: "hermes.form.apiModeAnthropicMessages",
  },
  { value: "codex_responses", labelKey: "hermes.form.apiModeCodexResponses" },
  {
    value: "bedrock_converse",
    labelKey: "hermes.form.apiModeBedrockConverse",
  },
];

export interface HermesProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  settingsConfig: HermesProviderSettingsConfig;
  isOfficial?: boolean;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  category?: ProviderCategory;
  templateValues?: Record<string, TemplateValueConfig>;
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
  isCustomTemplate?: boolean;
  /** Optional top-level `model:` defaults written on switch. */
  suggestedDefaults?: HermesSuggestedDefaults;
}

export interface HermesProviderSettingsConfig {
  name: string;
  base_url?: string;
  api_key?: string;
  api_mode?: HermesApiMode;
  /** UI-side ordered list; serialized to YAML as a dict keyed by id. */
  models?: HermesModel[];
  /** Delay in seconds between consecutive requests to this provider. */
  rate_limit_delay?: number;
  [key: string]: unknown;
}

export const hermesProviderPresets: HermesProviderPreset[] = [
  {
    name: "Nous Research",
    websiteUrl: "https://nousresearch.com",
    apiKeyUrl: "https://portal.nousresearch.com/",
    settingsConfig: {
      name: "nous",
      base_url: "https://inference-api.nousresearch.com/v1",
      api_key: "",
      api_mode: "chat_completions",
      models: [
        {
          id: "Hermes-4-405B",
          name: "Hermes 4 405B",
          context_length: 131072,
        },
        {
          id: "Hermes-4-70B",
          name: "Hermes 4 70B",
          context_length: 131072,
        },
      ],
    },
    isOfficial: true,
    category: "official",
    icon: "hermes",
    iconColor: "#7C3AED",
    suggestedDefaults: {
      model: { default: "Hermes-4-405B", provider: "nous" },
    },
  },
];
