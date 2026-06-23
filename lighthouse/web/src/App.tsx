import { useCallback, useEffect, useMemo, useState } from "react";

import type { DeployStatus, ServiceStatus } from "./types.ts";
import {
  controlService,
  fetchDeployStatus,
  fetchServices,
  type ServiceAction,
} from "./api.ts";
import { FleetHome } from "./components/FleetHome.tsx";
import { ServiceDetail } from "./components/ServiceDetail.tsx";

const POLL_INTERVAL_MS = 5_000;
// Deploy freshness changes only on a deploy or a local git change, so it polls
// far less often than status — and is also refreshed right after a deploy.
const DEPLOY_STATUS_INTERVAL_MS = 30_000;

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
  const [deployStatus, setDeployStatus] = useState<DeployStatus[]>([]);
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

  // Return to the fleet home (the card grid).
  const goHome = useCallback(() => {
    setSelected(null);
    if (window.location.pathname !== "/") {
      history.pushState(null, "", "/");
    }
  }, []);

  // Poll service status on an interval.
  useEffect(() => {
    void refresh();
    const id = setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refresh]);

  // Deploy freshness: poll slowly, and expose a manual refresh for right after a
  // deploy. A failure just means no buttons/badges — not a dashboard error.
  const refreshDeployStatus = useCallback(async () => {
    try {
      setDeployStatus(await fetchDeployStatus());
    } catch {
      setDeployStatus([]);
    }
  }, []);

  useEffect(() => {
    void refreshDeployStatus();
    const id = setInterval(() => void refreshDeployStatus(), DEPLOY_STATUS_INTERVAL_MS);
    return () => clearInterval(id);
  }, [refreshDeployStatus]);

  const deployByUnit = useMemo(() => {
    const map = new Map<string, DeployStatus>();
    for (const entry of deployStatus) map.set(entry.unit, entry);
    return map;
  }, [deployStatus]);

  // Once services load, resolve the unit named in the URL (if any). On `/` this
  // stays null — the home grid. Runs only while nothing is selected yet, so it
  // doesn't fight navigation.
  useEffect(() => {
    if (selected === null && services.length > 0) {
      const fromUrl = matchUnit(services, unitFromPath());
      if (fromUrl) setSelected(fromUrl);
    }
  }, [services, selected]);

  // Follow browser back/forward between the home and service routes.
  useEffect(() => {
    function onPopState() {
      setSelected(matchUnit(services, unitFromPath()));
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
      <header className="flex items-baseline gap-3 border-b border-rule-strong px-4 py-4 sm:px-6">
        <button
          type="button"
          onClick={goHome}
          className="text-lg font-bold uppercase tracking-[0.2em] text-ink hover:text-accent"
        >
          Lighthouse
        </button>
        <span className="text-xs uppercase tracking-wider text-ink-muted">
          Fleet · deepwa7er
        </span>
        {/* Decorative doc stamp — dropped on narrow screens where space is for content. */}
        <span className="ml-auto hidden text-[10px] uppercase tracking-wider text-ink-faint sm:block">
          DOC. LH-001 · REV 0.1.0
        </span>
      </header>

      {selectedService ? (
        <div className="flex min-h-0 flex-1 flex-col">
          <nav className="flex items-center gap-1 border-b border-rule px-4 py-2 text-xs sm:px-6">
            <button
              type="button"
              onClick={goHome}
              className="text-ink-muted hover:text-accent"
            >
              ← Fleet
            </button>
            <span className="text-ink-faint">/</span>
            <span className="text-ink">{selectedService.name}</span>
          </nav>
          <main className="min-h-0 flex-1">
            <ServiceDetail
              key={selectedService.unit}
              service={selectedService}
              deploy={deployByUnit.get(selectedService.unit) ?? null}
              onChanged={() => void refresh()}
              onDeployed={() => void refreshDeployStatus()}
            />
          </main>
        </div>
      ) : (
        <main className="min-h-0 flex-1 overflow-auto">
          <FleetHome
            services={services}
            deployByUnit={deployByUnit}
            error={error}
            onSelect={select}
          />
        </main>
      )}
    </div>
  );
}
