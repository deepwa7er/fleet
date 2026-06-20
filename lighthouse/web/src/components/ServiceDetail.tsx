import { useState } from "react";

import type { ServiceStatus } from "../types.ts";
import { controlService, startDeploy, type ServiceAction } from "../api.ts";
import { cn, statusColor } from "../lib/utils.ts";
import { DeployConsole } from "./DeployConsole.tsx";
import { LogViewer } from "./LogViewer.tsx";

interface Props {
  service: ServiceStatus;
  /** Whether this service can be deployed via the tugboat daemon. */
  canDeploy: boolean;
  /** Called after a successful action so the dashboard can refresh status. */
  onChanged: () => void;
}

const ACTION_STYLES: Record<ServiceAction, string> = {
  start: "text-active hover:border-active",
  stop: "text-failed hover:border-failed",
  restart: "text-accent hover:border-accent",
};

export function ServiceDetail({ service, canDeploy, onChanged }: Props) {
  const [pending, setPending] = useState<ServiceAction | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The active deploy job id, or null when not deploying. While set, the deploy
  // transcript replaces the log view.
  const [deployJob, setDeployJob] = useState<string | null>(null);
  const [deploying, setDeploying] = useState(false);
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

  async function deploy() {
    if (
      !window.confirm(
        `Deploy ${service.name}? This rebuilds and ships the current working tree from the build host.`,
      )
    ) {
      return;
    }
    setDeploying(true);
    setError(null);
    try {
      const { job_id } = await startDeploy(service.unit);
      setDeployJob(job_id);
    } catch (err: unknown) {
      setError(String(err));
    } finally {
      setDeploying(false);
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
      <div className="flex items-center justify-between gap-4 border-b border-rule-strong px-6 py-4">
        <div className="flex min-w-0 items-center gap-3">
          <span className={cn("h-2 w-2 shrink-0", color.dot)} />
          <h2 className="truncate text-lg font-bold text-ink">
            {service.name}
          </h2>
          <span className={cn("shrink-0 text-xs uppercase tracking-wide", color.text)}>
            {service.active_state} · {service.sub_state}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {canDeploy && (
            <button
              type="button"
              onClick={() => void deploy()}
              disabled={deploying || deployJob !== null}
              className={cn(
                "border border-rule-strong bg-surface px-3 py-1.5 text-xs uppercase tracking-wide text-accent hover:border-accent",
                (deploying || deployJob !== null) && "cursor-not-allowed opacity-40",
              )}
            >
              {deploying ? "deploy…" : "deploy"}
            </button>
          )}
          {buttons.map(({ action, disabled }) => (
            <button
              key={action}
              type="button"
              onClick={() => run(action)}
              disabled={disabled}
              className={cn(
                "border border-rule-strong bg-surface px-3 py-1.5 text-xs uppercase tracking-wide",
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
        <div className="border-l-2 border-failed bg-failed/10 px-6 py-2 text-xs text-failed">
          {error}
        </div>
      )}
      <div className="min-h-0 flex-1">
        {deployJob ? (
          <DeployConsole
            unit={service.unit}
            jobId={deployJob}
            onClose={() => setDeployJob(null)}
            onDone={onChanged}
          />
        ) : (
          <LogViewer unit={service.unit} />
        )}
      </div>
    </div>
  );
}
