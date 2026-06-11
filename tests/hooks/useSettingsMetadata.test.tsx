import { renderHook, act } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { useSettingsMetadata } from "@/hooks/useSettingsMetadata";

describe("useSettingsMetadata", () => {
  it("allows updating restart flag via setters", async () => {
    const { result } = renderHook(() => useSettingsMetadata());

    expect(result.current.requiresRestart).toBe(false);

    await act(async () => {
      result.current.setRequiresRestart(true);
      await Promise.resolve();
    });

    expect(result.current.requiresRestart).toBe(true);

    await act(async () => {
      result.current.acknowledgeRestart();
      await Promise.resolve();
    });

    expect(result.current.requiresRestart).toBe(false);
  });
});
