import { useCallback, useEffect, useState } from "react";

import type { ServiceStatus } from "./types.ts";
import { fetchServices } from "./api.ts";
import { ServiceCard } from "./components/ServiceCard.tsx";
import { ServiceDetail } from "./components/ServiceDetail.tsx";

const POLL_INTERVAL_MS = 5_000;

export function App() {
  const [services, setServices] = useState<ServiceStatus[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setServices(await fetchServices());
      setError(null);
    } catch (err: unknown) {
      setError(String(err));
    }
  }, []);

  // Poll service status on an interval.
  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // Default the selection to the first service once they load.
  useEffect(() => {
    if (selected === null && services.length > 0) {
      setSelected(services[0].unit);
    }
  }, [services, selected]);

  const selectedService = services.find((s) => s.unit === selected) ?? null;

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center gap-3 border-b border-slate-800 px-6 py-4">
        <h1 className="text-lg font-semibold text-slate-100">Lighthouse</h1>
        <span className="text-sm text-slate-500">service dashboard</span>
      </header>
      <div className="flex min-h-0 flex-1">
        <aside className="w-80 shrink-0 space-y-2 overflow-auto border-r border-slate-800 p-4">
          {error && (
            <div className="rounded bg-red-950/50 p-2 text-sm text-red-300">
              {error}
            </div>
          )}
          {services.map((service) => (
            <ServiceCard
              key={service.unit}
              service={service}
              selected={service.unit === selected}
              onSelect={() => setSelected(service.unit)}
            />
          ))}
        </aside>
        <main className="min-w-0 flex-1">
          {selectedService ? (
            <ServiceDetail
              key={selectedService.unit}
              service={selectedService}
              onChanged={() => void refresh()}
            />
          ) : (
            <div className="flex h-full items-center justify-center text-slate-600">
              Select a service
            </div>
          )}
        </main>
      </div>
    </div>
  );
}
