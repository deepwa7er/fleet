import type { DeployStatus, ServiceStatus } from "../types.ts";
import { FleetCard } from "./FleetCard.tsx";

interface Props {
  services: ServiceStatus[];
  deployByUnit: Map<string, DeployStatus>;
  error: string | null;
  /** Open a service's detail (logs / deploy) view. */
  onSelect: (unit: string) => void;
}

/** The fleet home: every monitored service as a card — live status, what it is,
 * and where to reach it — in a responsive grid. */
export function FleetHome({ services, deployByUnit, error, onSelect }: Props) {
  return (
    <div className="mx-auto max-w-6xl p-4 sm:p-6">
      {error && (
        <div className="mb-4 border-l-2 border-failed bg-failed/10 px-3 py-2 text-xs text-failed">
          {error}
        </div>
      )}
      {services.length === 0 && !error ? (
        <div className="text-xs uppercase tracking-wide text-ink-faint">Loading…</div>
      ) : (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
          {services.map((service) => (
            <FleetCard
              key={service.unit}
              service={service}
              deploy={deployByUnit.get(service.unit) ?? null}
              onSelect={() => onSelect(service.unit)}
            />
          ))}
        </div>
      )}
    </div>
  );
}
