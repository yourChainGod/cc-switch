import React, { useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Textarea } from "@/components/ui/textarea";

interface ApiKeyInputProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  required?: boolean;
  label?: string;
  id?: string;
  multiline?: boolean;
}

const ApiKeyInput: React.FC<ApiKeyInputProps> = ({
  value,
  onChange,
  placeholder,
  disabled = false,
  required = false,
  label = "API Key",
  id = "apiKey",
  multiline = false,
}) => {
  const { t } = useTranslation();
  const [showKey, setShowKey] = useState(false);

  const toggleShowKey = () => {
    setShowKey(!showKey);
  };

  const inputClass = `w-full px-3 py-2 pr-10 border rounded-lg text-sm transition-colors ${
    disabled
      ? "bg-muted border-border-default text-muted-foreground cursor-not-allowed"
      : "border-border-default bg-background text-foreground focus:outline-none focus:ring-2 focus:ring-blue-500/20 dark:focus:ring-blue-400/20"
  }`;
  const textareaClass = `min-h-28 resize-y pr-10 font-mono text-xs leading-5 ${
    disabled ? "cursor-not-allowed opacity-60" : ""
  }`;
  const concealedTextStyle =
    multiline && !showKey
      ? ({
          WebkitTextSecurity: "disc",
        } as React.CSSProperties & { WebkitTextSecurity: string })
      : undefined;

  return (
    <div className="space-y-2">
      <label htmlFor={id} className="block text-sm font-medium text-foreground">
        {label} {required && "*"}
      </label>
      <div className="relative">
        {multiline ? (
          <Textarea
            id={id}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={placeholder ?? t("apiKeyInput.placeholder")}
            disabled={disabled}
            required={required}
            className={textareaClass}
            style={concealedTextStyle}
          />
        ) : (
          <input
            type={showKey ? "text" : "password"}
            id={id}
            value={value}
            onChange={(e) => onChange(e.target.value)}
            placeholder={placeholder ?? t("apiKeyInput.placeholder")}
            disabled={disabled}
            required={required}
            autoComplete="off"
            className={inputClass}
          />
        )}
        {!disabled && value && (
          <button
            type="button"
            onClick={toggleShowKey}
            className={
              multiline
                ? "absolute right-2 top-2 rounded-md bg-background/80 p-1.5 text-muted-foreground shadow-sm transition-colors hover:text-foreground"
                : "absolute inset-y-0 right-0 flex items-center pr-3 text-muted-foreground hover:text-foreground transition-colors"
            }
            aria-label={showKey ? t("apiKeyInput.hide") : t("apiKeyInput.show")}
          >
            {showKey ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        )}
      </div>
    </div>
  );
};

export default ApiKeyInput;
