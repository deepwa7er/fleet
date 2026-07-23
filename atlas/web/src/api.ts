// Typed wrappers over atlas's JSON API. In production the same axum binary
// serves this bundle, so all calls are same-origin under /api.

export interface Project {
  id: number;
  name: string;
  root: string;
  indexed_at: string | null;
  commit_hash: string | null;
  duration_ms: number | null;
  symbols: number;
  call_edges: number;
  indexing: boolean;
}

export interface ModuleRow {
  crate_name: string;
  module_path: string;
  items: number;
}

export interface SymbolSummary {
  id: number;
  name: string;
  display: string;
  kind: string;
  crate_name: string;
  module_path: string;
  container: string | null;
  trait_name: string | null;
  signature: string | null;
  file: string | null;
  start_line: number | null;
  is_external: boolean;
}

export interface LinkedSymbol extends SymbolSummary {
  edge_kind: "call" | "use";
  count: number;
}

export interface SymbolDetail extends SymbolSummary {
  end_line: number | null;
  docs: string | null;
  callers: LinkedSymbol[];
  callees: LinkedSymbol[];
  callers_truncated: boolean;
  callees_truncated: boolean;
  implementations: SymbolSummary[];
  declaration: SymbolSummary | null;
}

export interface TraceNode extends SymbolSummary {
  depth: number;
}

export interface TraceEdge {
  from: number;
  to: number;
  count: number;
}

export interface TraceGraph {
  root: number;
  direction: "out" | "in";
  nodes: TraceNode[];
  edges: TraceEdge[];
  truncated: boolean;
}

async function getJSON<T>(url: string): Promise<T> {
  const res = await fetch(url, { cache: "no-store" });
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // non-JSON error body; keep the status line
    }
    throw new Error(message);
  }
  return (await res.json()) as T;
}

export function fetchProjects(): Promise<Project[]> {
  return getJSON("/api/projects");
}

export function fetchModules(project: string): Promise<ModuleRow[]> {
  return getJSON(`/api/projects/${encodeURIComponent(project)}/modules`);
}

export function fetchItems(
  project: string,
  crate: string,
  module: string,
): Promise<SymbolSummary[]> {
  const params = new URLSearchParams({ crate, module });
  return getJSON(`/api/projects/${encodeURIComponent(project)}/items?${params}`);
}

export function fetchSymbol(id: number): Promise<SymbolDetail> {
  return getJSON(`/api/symbols/${id}`);
}

export function fetchTrace(
  id: number,
  dir: "out" | "in",
  depth: number,
  externals: boolean,
): Promise<TraceGraph> {
  const params = new URLSearchParams({ dir, depth: String(depth) });
  if (externals) params.set("externals", "true");
  return getJSON(`/api/symbols/${id}/trace?${params}`);
}

export function searchSymbols(project: string, q: string): Promise<SymbolSummary[]> {
  const params = new URLSearchParams({ q });
  return getJSON(`/api/projects/${encodeURIComponent(project)}/search?${params}`);
}

export async function reindex(project: string): Promise<void> {
  const res = await fetch(`/api/projects/${encodeURIComponent(project)}/reindex`, {
    method: "POST",
  });
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // keep status line
    }
    throw new Error(message);
  }
}
