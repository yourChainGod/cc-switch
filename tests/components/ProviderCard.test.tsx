import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ProviderCard } from "@/components/providers/ProviderCard";
import type { Provider } from "@/types";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (_key: string, options?: { defaultValue?: string }) =>
      options?.defaultValue ?? _key,
  }),
}));

vi.mock("@/lib/query/failover", () => ({
  useProviderHealth: () => ({ data: undefined }),
}));

vi.mock("@/lib/query/queries", () => ({
  useUsageQuery: () => ({ data: undefined }),
}));

vi.mock("@/components/ProviderIcon", () => ({
  ProviderIcon: ({ name }: { name: string }) => (
    <span data-testid="provider-icon">{name.slice(0, 1)}</span>
  ),
}));

vi.mock("@/components/UsageFooter", () => ({
  default: () => null,
}));

vi.mock("@/components/SubscriptionQuotaFooter", () => ({
  default: () => null,
}));

vi.mock("@/components/providers/ProviderActions", () => ({
  ProviderActions: () => <div data-testid="provider-actions" />,
}));

const provider: Provider = {
  id: "provider-a",
  name: "Provider A",
  category: "third_party",
  settingsConfig: {
    env: {
      ANTHROPIC_BASE_URL: "https://provider.example",
    },
  },
};

function renderCard(
  overrides: Partial<Parameters<typeof ProviderCard>[0]> = {},
) {
  return render(
    <ProviderCard
      provider={provider}
      isCurrent={false}
      appId="claude"
      onSwitch={vi.fn()}
      onEdit={vi.fn()}
      onDelete={vi.fn()}
      onOpenWebsite={vi.fn()}
      onDuplicate={vi.fn()}
      isProxyRunning={false}
      isInFailoverQueue={true}
      failoverPriority={2}
      {...overrides}
    />,
  );
}

describe("ProviderCard failover queue status", () => {
  it("shows queued providers as pending until failover is actually effective", () => {
    renderCard({
      isProxyRunning: false,
      isAutoFailoverEnabled: true,
    });

    expect(screen.getByText("P2")).toBeInTheDocument();
    expect(screen.getByText("待生效")).toBeInTheDocument();
    expect(screen.queryByText("生效中")).not.toBeInTheDocument();
  });

  it("shows queued providers as active when routing service and auto failover are both enabled", () => {
    renderCard({
      isProxyRunning: true,
      isAutoFailoverEnabled: true,
    });

    expect(screen.getByText("P2")).toBeInTheDocument();
    expect(screen.getByText("生效中")).toBeInTheDocument();
    expect(screen.queryByText("待生效")).not.toBeInTheDocument();
  });

  it("keeps the failover queue button visible on the provider card", () => {
    const onToggleFailover = vi.fn();
    renderCard({ onToggleFailover });

    const button = screen.getByRole("button", {
      name: "移出故障转移队列",
    });
    expect(button).toBeInTheDocument();

    fireEvent.click(button);

    expect(onToggleFailover).toHaveBeenCalledWith(false);
  });

  it("shows a visible add-to-queue action for providers outside the queue", () => {
    const onToggleFailover = vi.fn();
    renderCard({
      isInFailoverQueue: false,
      failoverPriority: undefined,
      onToggleFailover,
    });

    const button = screen.getByRole("button", {
      name: "加入故障转移队列",
    });
    expect(button).toBeInTheDocument();

    fireEvent.click(button);

    expect(onToggleFailover).toHaveBeenCalledWith(true);
  });
});
