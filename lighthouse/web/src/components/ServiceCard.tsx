import type { DeployStatus, ServiceStatus } from "../types.ts";
import { cn, deployVerdictStyle, formatBytes, statusColor } from "../lib/utils.ts";

interface Props {
  service: ServiceStatus;
  /** Deploy freshness, or null when the service isn't deployable. */
  deploy: DeployStatus | null;
  selected: boolean;
  onSelect: () => void;
}

export function ServiceCard({ service, deploy, selected, onSelect }: Props) {
  const color = statusColor(service.active_state);
  // Only flag services that have undeployed work; current/unknown stay quiet.
  const deployStyle = deploy ? deployVerdictStyle(deploy.verdict) : null;
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
        <span className="flex min-w-0 items-center gap-2">
          {deployStyle?.attention && (
            <span
              className={cn("h-1.5 w-1.5 shrink-0 rounded-full", deployStyle.dot)}
              title={`deploy: ${deploy?.verdict}`}
            />
          )}
          <span className="truncate font-medium text-ink">{service.name}</span>
        </span>
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
