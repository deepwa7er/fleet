import type { ServiceStatus } from "../types.ts";
import { cn, formatBytes, statusColor } from "../lib/utils.ts";

interface Props {
  service: ServiceStatus;
  selected: boolean;
  onSelect: () => void;
}

export function ServiceCard({ service, selected, onSelect }: Props) {
  const color = statusColor(service.active_state);
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "w-full border p-3 text-left",
        selected
          ? "border-accent bg-surface-2"
          : "border-rule bg-surface hover:border-rule-strong",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="truncate font-medium text-ink">{service.name}</span>
        <span className="flex shrink-0 items-center gap-2 text-[11px]">
          <span className={cn("h-2 w-2 shrink-0", color.dot)} />
          <span className={cn("uppercase tracking-wide", color.text)}>
            {service.active_state}
            {service.sub_state ? ` · ${service.sub_state}` : ""}
          </span>
        </span>
      </div>
      <div className="mt-2 space-y-0.5 text-[11px] text-ink-muted">
        {service.since && <div className="truncate">since {service.since}</div>}
        <div className="flex gap-4">
          {service.pid != null && <span>pid {service.pid}</span>}
          {service.memory_bytes != null && (
            <span>{formatBytes(service.memory_bytes)}</span>
          )}
        </div>
      </div>
    </button>
  );
}
