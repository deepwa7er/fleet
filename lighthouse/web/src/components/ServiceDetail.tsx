import { useState } from "react";

import type { ServiceStatus } from "../types.ts";
import { controlService, type ServiceAction } from "../api.ts";
import { cn, statusColor } from "../lib/utils.ts";
import { LogViewer } from "./LogViewer.tsx";

interface Props {
  service: ServiceStatus;
  /** Called after a successful action so the dashboard can refresh status. */
  onChanged: () => void;
}

const ACTION_STYLES: Record<ServiceAction, string> = {
  start: "bg-emerald-600 hover:bg-emerald-500",
  stop: "bg-red-700 hover:bg-red-600",
  restart: "bg-sky-700 hover:bg-sky-600",
};

export function ServiceDetail({ service, onChanged }: Props) {
  const [pending, setPending] = useState<ServiceAction | null>(null);
  const [error, setError] = useState<string | null>(null);
  const color = statusColor(service.active_state);
  // A crash-looping service is rarely caught in "active" — it cycles through
  // activating / auto-restart / failed. `systemctl stop` is exactly what breaks
  // that loop, so Stop must stay available in those states; only a cleanly
  // stopped service ("inactive") disables it. Start is the inverse: offered
  // only when the service isn't running or trying to.
  const state = service.active_state;
  const stopped = state === "inactive" || state === "failed";

  async function run(action: ServiceAction) {
    // Stopping or restarting causes downtime — confirm to avoid a misclick.
    if (
      (action === "stop" || action === "restart") &&
      !window.confirm(`${action} ${service.name}?`)
    ) {
      return;
    }
    setPending(action);
    setError(null);
    try {
      await controlService(service.unit, action);
      onChanged();
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setPending(null);
    }
  }

  const busy = pending !== null;
  const buttons: { action: ServiceAction; disabled: boolean }[] = [
    { action: "start", disabled: busy || !stopped },
    { action: "stop", disabled: busy || state === "inactive" },
    { action: "restart", disabled: busy },
  ];

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between gap-4 border-b border-slate-800 px-6 py-4">
        <div className="flex min-w-0 items-center gap-3">
          <span className={cn("h-2.5 w-2.5 shrink-0 rounded-full", color.dot)} />
          <h2 className="truncate text-lg font-medium text-slate-100">
            {service.name}
          </h2>
          <span className={cn("shrink-0 text-sm", color.text)}>
            {service.active_state} · {service.sub_state}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {buttons.map(({ action, disabled }) => (
            <button
              key={action}
              type="button"
              onClick={() => run(action)}
              disabled={disabled}
              className={cn(
                "rounded px-3 py-1.5 text-sm font-medium text-white capitalize transition",
                ACTION_STYLES[action],
                disabled && "cursor-not-allowed opacity-40",
              )}
            >
              {pending === action ? `${action}…` : action}
            </button>
          ))}
        </div>
      </div>
      {error && (
        <div className="bg-red-950/50 px-6 py-2 text-sm text-red-300">{error}</div>
      )}
      <div className="min-h-0 flex-1">
        <LogViewer unit={service.unit} />
      </div>
    </div>
  );
}
