import { useCallback, useState } from "react";
import { LayoutGrid, List } from "lucide-react";

export type McpMode = "yours" | "discover";
export type McpLayout = "cards" | "list";

const DEFAULT_LAYOUT: Record<McpMode, McpLayout> = {
  yours: "list",
  discover: "cards",
};

const layoutKey = (mode: McpMode) => `opencompany.mcp.layout.${mode}`;

function readLayout(mode: McpMode): McpLayout {
  try {
    const stored = window.localStorage.getItem(layoutKey(mode));
    if (stored === "cards" || stored === "list") return stored;
  } catch {
    // Storage unavailable; the default stands.
  }
  return DEFAULT_LAYOUT[mode];
}

/** Each tab's chosen layout, remembered in this browser. */
export function useMcpLayouts(): [
  Record<McpMode, McpLayout>,
  (mode: McpMode, layout: McpLayout) => void,
] {
  const [layouts, setLayouts] = useState<Record<McpMode, McpLayout>>(() => ({
    yours: readLayout("yours"),
    discover: readLayout("discover"),
  }));
  const choose = useCallback((mode: McpMode, layout: McpLayout) => {
    setLayouts((prev) => ({ ...prev, [mode]: layout }));
    try {
      window.localStorage.setItem(layoutKey(mode), layout);
    } catch {
      // Storage unavailable; the choice lasts for this visit.
    }
  }, []);
  return [layouts, choose];
}

function Segment<T extends string>({
  value,
  options,
  onChange,
  label,
}: {
  value: T;
  options: { id: T; label: React.ReactNode; title: string; testId: string }[];
  onChange: (next: T) => void;
  label: string;
}) {
  return (
    <div
      role="group"
      aria-label={label}
      className="inline-flex shrink-0 items-center rounded-lg border border-border bg-muted/40 p-0.5"
    >
      {options.map((option) => (
        <button
          key={option.id}
          type="button"
          title={option.title}
          aria-pressed={value === option.id}
          data-testid={option.testId}
          onClick={() => onChange(option.id)}
          className={`inline-flex h-7 items-center gap-1 rounded-md px-2.5 text-sm transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none ${
            value === option.id
              ? "bg-background font-medium text-foreground shadow-xs"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function McpModeSwitch({
  mode,
  onChange,
}: {
  mode: McpMode;
  onChange: (mode: McpMode) => void;
}) {
  return (
    <Segment
      value={mode}
      onChange={onChange}
      label="Show"
      options={[
        { id: "yours", label: "Yours", title: "Your servers", testId: "mcp-mode-yours" },
        {
          id: "discover",
          label: "Discover",
          title: "Browse the directory",
          testId: "mcp-mode-discover",
        },
      ]}
    />
  );
}

export function McpLayoutSwitch({
  layout,
  onChange,
}: {
  layout: McpLayout;
  onChange: (layout: McpLayout) => void;
}) {
  return (
    <Segment
      value={layout}
      onChange={onChange}
      label="Layout"
      options={[
        {
          id: "list",
          label: <List className="size-4" aria-label="List view" />,
          title: "List view",
          testId: "mcp-layout-list",
        },
        {
          id: "cards",
          label: <LayoutGrid className="size-4" aria-label="Card view" />,
          title: "Card view",
          testId: "mcp-layout-cards",
        },
      ]}
    />
  );
}
