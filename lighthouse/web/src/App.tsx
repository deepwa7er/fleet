import { useCallback, useEffect, useState } from "react";

import type { ServiceStatus } from "./types.ts";
import { controlService, fetchServices, type ServiceAction } from "./api.ts";
import { ServiceCard } from "./components/ServiceCard.tsx";
import { ServiceDetail } from "./components/ServiceDetail.tsx";

const POLL_INTERVAL_MS = 5_000;

const ACTIONS: ServiceAction[] = ["start", "stop", "restart"];

/** A valid control action from the `?action=` query, or null. Lets a link such
 * as ferry's `b lh <svc> restart` issue the action on arrival. */
function actionFromQuery(): ServiceAction | null {
  const value = new URLSearchParams(window.location.search).get("action");
  return ACTIONS.includes(value as ServiceAction) ? (value as ServiceAction) : null;
}

// Client-side routing on `/services/<unit>`. The unit in the path may be the
// full unit name or its `.service`-stripped short form (what you'd type into
// ferry, e.g. `lh ferry`); both resolve to the same service.

/** The unit slug from the current path, or null when not on a service route. */
function unitFromPath(): string | null {
  const match = window.location.pathname.match(/^\/services\/(.+)$/);
  return match ? decodeURIComponent(match[1]) : null;
}

/** Resolve a path slug to a real unit, matching the full or short name. */
function matchUnit(services: ServiceStatus[], slug: string | null): string | null {
  if (!slug) return null;
  const found = services.find(
    (s) => s.unit === slug || s.unit.replace(/\.service$/, "") === slug,
  );
  return found ? found.unit : null;
}

/** The short, URL-friendly form of a unit name. */
function slugFor(unit: string): string {
  return unit.replace(/\.service$/, "");
}

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

  // Select a service and reflect it in the URL so it's deep-linkable and
  // back/forward works.
  const select = useCallback((unit: string) => {
    setSelected(unit);
    const path = `/services/${encodeURIComponent(slugFor(unit))}`;
    if (window.location.pathname !== path) {
      history.pushState(null, "", path);
    }
  }, []);

  // Poll service status on an interval.
  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // Once services load, select the one named in the URL (if any), else the
  // first. Runs only while nothing is selected yet.
  useEffect(() => {
    if (selected === null && services.length > 0) {
      setSelected(matchUnit(services, unitFromPath()) ?? services[0].unit);
    }
  }, [services, selected]);

  // Follow browser back/forward between service routes.
  useEffect(() => {
    function onPopState() {
      setSelected(matchUnit(services, unitFromPath()) ?? services[0]?.unit ?? null);
    }
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [services]);

  // Run a control action supplied in the URL (e.g. ferry's `b lh <svc> restart`).
  // It fires ONLY when the path resolves to a real service — never a fallback —
  // so a typo'd name can't act on the wrong one. The query param is cleared
  // first, so it runs once and a refresh won't re-issue it.
  useEffect(() => {
    if (services.length === 0) return;
    const action = actionFromQuery();
    const unit = matchUnit(services, unitFromPath());
    if (!action || !unit) return;
    history.replaceState(null, "", `/services/${encodeURIComponent(slugFor(unit))}`);
    controlService(unit, action).then(
      () => void refresh(),
      (err: unknown) => setError(String(err)),
    );
  }, [services, refresh]);

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
              onSelect={() => select(service.unit)}
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
