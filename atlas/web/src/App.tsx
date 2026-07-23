import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchItems,
  fetchModules,
  fetchProjects,
  reindex,
  type ModuleRow,
  type Project,
  type SymbolSummary,
} from "./api";
import { ModuleTree } from "./components/ModuleTree";
import { SymbolTable } from "./components/SymbolTable";
import { SymbolPage } from "./components/SymbolPage";
import { TraceView } from "./components/TraceView";
import { SearchResults } from "./components/SearchResults";
import { navigate, parseRoute, type Route } from "./lib/route";

export function App() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [route, setRoute] = useState<Route | null>(() => parseRoute(location.hash));
  const [modules, setModules] = useState<ModuleRow[]>([]);
  const [modulesError, setModulesError] = useState<string | null>(null);
  const [items, setItems] = useState<SymbolSummary[] | null>(null);
  const [itemsError, setItemsError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [reindexError, setReindexError] = useState<string | null>(null);

  const modulesReq = useRef(0);
  const itemsReq = useRef(0);

  const project = route?.project ?? null;
  const active = projects.find((p) => p.name === project) ?? null;
  const anyIndexing = projects.some((p) => p.indexing);

  const loadProjects = useCallback(() => {
    fetchProjects()
      .then(setProjects)
      .catch((e) => setModulesError(String(e)));
  }, []);

  useEffect(loadProjects, [loadProjects]);

  // While an index runs, poll for completion; refresh modules when it lands.
  useEffect(() => {
    if (!anyIndexing) return;
    const timer = setInterval(loadProjects, 2000);
    return () => clearInterval(timer);
  }, [anyIndexing, loadProjects]);

  // The hash is the source of truth for the view.
  useEffect(() => {
    const onHash = () => setRoute(parseRoute(location.hash));
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  // No route yet (first load): land on the first project.
  useEffect(() => {
    if (!route && projects.length > 0) {
      navigate({ t: "home", project: projects[0].name });
    }
  }, [route, projects]);

  // The sidebar tree follows the routed project.
  const indexedAt = active?.indexed_at;
  useEffect(() => {
    if (!project) return;
    const r = ++modulesReq.current;
    setModulesError(null);
    fetchModules(project)
      .then((m) => {
        if (modulesReq.current === r) setModules(m);
      })
      .catch((e) => {
        if (modulesReq.current === r) {
          setModules([]);
          setModulesError(String(e));
        }
      });
  }, [project, indexedAt]);

  // Module items when the route shows a module.
  const moduleKey =
    route?.t === "module" ? `${route.crate}//${route.module}` : route?.t === "home" ? "" : null;
  useEffect(() => {
    if (!project || moduleKey === null || route?.t !== "module") {
      setItems(null);
      return;
    }
    const r = ++itemsReq.current;
    setItems(null);
    setItemsError(null);
    fetchItems(project, route.crate, route.module)
      .then((rows) => {
        if (itemsReq.current === r) setItems(rows);
      })
      .catch((e) => {
        if (itemsReq.current === r) setItemsError(String(e));
      });
  }, [project, moduleKey, route]);

  const openSymbol = useCallback(
    (id: number) => {
      if (project) navigate({ t: "symbol", project, id });
    },
    [project],
  );

  const startReindex = useCallback(() => {
    if (!project) return;
    setReindexError(null);
    reindex(project)
      .then(loadProjects)
      .catch((e) => setReindexError(String(e)));
  }, [project, loadProjects]);

  const submitSearch = (e: React.FormEvent) => {
    e.preventDefault();
    const q = query.trim();
    if (q && project) navigate({ t: "search", project, q });
  };

  return (
    <div className="app">
      <header>
        <div className="brand">
          <span className="wordmark">ATLAS</span>
          <span className="sub">rust code maps</span>
          <select
            className="project-select"
            value={project ?? ""}
            onChange={(e) => navigate({ t: "home", project: e.target.value })}
          >
            {projects.map((p) => (
              <option key={p.name} value={p.name}>
                {p.name}
              </option>
            ))}
            {projects.length === 0 && <option value="">—</option>}
          </select>
        </div>

        <form className="searchbar" onSubmit={submitSearch}>
          <input
            type="search"
            placeholder="Find a symbol…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
          <button type="submit" className="btn">
            search
          </button>
        </form>

        {active && (
          <div className="lcd">
            <div className="lcd-cell">
              <span className="lcd-label">SYMBOLS</span>
              <span className="lcd-value">{active.symbols.toLocaleString()}</span>
            </div>
            <div className="lcd-cell">
              <span className="lcd-label">CALLS</span>
              <span className="lcd-value">{active.call_edges.toLocaleString()}</span>
            </div>
            <div className="lcd-cell">
              <span className="lcd-label">INDEXED</span>
              <span className="lcd-value">
                {active.indexing
                  ? "RUNNING…"
                  : active.indexed_at
                    ? `${active.indexed_at.slice(0, 16).replace("T", " ")} @${active.commit_hash ?? "?"}`
                    : "NEVER"}
              </span>
            </div>
            <button
              className="btn lcd-btn"
              onClick={startReindex}
              disabled={active.indexing}
              title="Run rust-analyzer over the workspace again"
            >
              re-index
            </button>
          </div>
        )}
      </header>
      {reindexError && <div className="banner-error">{reindexError}</div>}

      <div className="body">
        <aside className="sidebar">
          {modulesError && <p className="error">{modulesError}</p>}
          {!modulesError && modules.length === 0 && (
            <p className="empty">
              {active && !active.indexed_at ? "not indexed yet — hit re-index" : "loading…"}
            </p>
          )}
          <ModuleTree
            modules={modules}
            active={route?.t === "module" ? `${route.crate}//${route.module}` : null}
            onOpen={(crate, module) =>
              project && navigate({ t: "module", project, crate, module })
            }
          />
        </aside>

        <main className="main">
          {(!route || route.t === "home") && (
            <div className="placeholder">
              <p>
                Pick a module to see its symbols, search for an entry point, or open a function
                and trace the flow from there.
              </p>
            </div>
          )}

          {route?.t === "module" && (
            <section className="pane-pad">
              <h2>
                {route.crate}
                {route.module && `::${route.module}`}{" "}
                {items && <span className="count">{items.length} items</span>}
              </h2>
              {itemsError && <p className="error">{itemsError}</p>}
              {!items && !itemsError && <p className="empty">loading…</p>}
              {items && <SymbolTable symbols={items} onOpen={openSymbol} />}
            </section>
          )}

          {route?.t === "symbol" && (
            <SymbolPage
              id={route.id}
              onOpen={openSymbol}
              onTrace={(id, dir) =>
                navigate({
                  t: "trace",
                  project: route.project,
                  id,
                  dir,
                  depth: 3,
                  externals: false,
                })
              }
            />
          )}

          {route?.t === "trace" && (
            <TraceView
              params={route}
              onOpen={openSymbol}
              onRetrace={(id) => navigate({ ...route, id })}
              onParams={(p) => navigate({ ...route, ...p })}
            />
          )}

          {route?.t === "search" && (
            <SearchResults project={route.project} query={route.q} onOpen={openSymbol} />
          )}
        </main>
      </div>

      <footer>
        <span className="doc-no">ATLAS · SYMBOL GRAPH OVER SCIP</span>
        <span className="quiet-mark">atlas</span>
      </footer>
    </div>
  );
}
