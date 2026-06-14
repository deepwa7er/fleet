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
      <header className="flex items-baseline gap-3 border-b border-rule-strong px-6 py-4">
        <h1 className="text-lg font-bold uppercase tracking-[0.2em] text-ink">
          Lighthouse
        </h1>
        <span className="text-xs uppercase tracking-wider text-ink-muted">
          Service dashboard · deepwa7er
        </span>
        <span className="ml-auto text-[10px] uppercase tracking-wider text-ink-faint">
          DOC. LH-001 · REV 0.1.0
        </span>
      </header>
      <div className="flex min-h-0 flex-1">
        <aside className="flex w-80 shrink-0 flex-col overflow-auto border-r border-rule">
          <div className="border-b border-rule px-4 py-2 text-[10px] font-bold uppercase tracking-[0.15em] text-ink-muted">
            Services · {services.length}
          </div>
          <div className="space-y-2 p-4">
            {error && (
              <div className="border-l-2 border-failed bg-failed/10 px-3 py-2 text-xs text-failed">
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
            {services.length === 0 && !error && (
              <div className="text-xs uppercase tracking-wide text-ink-faint">
                Loading…
              </div>
            )}
          </div>
        </aside>
        <main className="min-w-0 flex-1">
          {selectedService ? (
            <ServiceDetail
              key={selectedService.unit}
              service={selectedService}
              onChanged={() => void refresh()}
            />
          ) : (
            <div className="flex h-full items-center justify-center text-sm uppercase tracking-wide text-ink-faint">
              Select a service
            </div>
          )}
        </main>
      </div>
    </div>
  );
}
