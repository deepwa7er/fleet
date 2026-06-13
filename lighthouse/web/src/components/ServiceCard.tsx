import type { ServiceStatus } from "../types.ts";
import { cn, formatBytes } from "../lib/utils.ts";

function statusColor(state: string): { dot: string; text: string } {
  switch (state) {
    case "active":
      return { dot: "bg-emerald-500", text: "text-emerald-400" };
    case "failed":
      return { dot: "bg-red-500", text: "text-red-400" };
    case "activating":
    case "deactivating":
    case "reloading":
      return { dot: "bg-amber-500", text: "text-amber-400" };
    default:
      return { dot: "bg-slate-500", text: "text-slate-400" };
  }
}

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
        "w-full rounded-lg border p-4 text-left transition",
        selected
          ? "border-sky-500 bg-slate-800/60"
          : "border-slate-700 bg-slate-800/30 hover:bg-slate-800/50",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="font-medium text-slate-100">{service.name}</span>
        <span className="flex items-center gap-2 text-xs">
          <span className={cn("h-2.5 w-2.5 rounded-full", color.dot)} />
          <span className={color.text}>
            {service.active_state}
            {service.sub_state ? ` · ${service.sub_state}` : ""}
          </span>
        </span>
      </div>
      <div className="mt-2 space-y-0.5 text-xs text-slate-400">
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
