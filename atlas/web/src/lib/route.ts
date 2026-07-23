// Hash routing: the whole view state lives in location.hash so the browser's
// back button and deep links work. Shapes:
//
//   #/<project>                                  project home (module explorer)
//   #/<project>/m/<crate>/<module>               one module's items
//   #/<project>/s/<symbol id>                    symbol page
//   #/<project>/t/<symbol id>/<dir>/<depth>/<x>  trace view (x = externals 0|1)
//   #/<project>/q/<query>                        search results

export type Route =
  | { t: "home"; project: string }
  | { t: "module"; project: string; crate: string; module: string }
  | { t: "symbol"; project: string; id: number }
  | {
      t: "trace";
      project: string;
      id: number;
      dir: "out" | "in";
      depth: number;
      externals: boolean;
    }
  | { t: "search"; project: string; q: string };

export function formatRoute(route: Route): string {
  const p = encodeURIComponent(route.project);
  switch (route.t) {
    case "home":
      return `#/${p}`;
    case "module":
      return `#/${p}/m/${encodeURIComponent(route.crate)}/${encodeURIComponent(route.module)}`;
    case "symbol":
      return `#/${p}/s/${route.id}`;
    case "trace":
      return `#/${p}/t/${route.id}/${route.dir}/${route.depth}/${route.externals ? 1 : 0}`;
    case "search":
      return `#/${p}/q/${encodeURIComponent(route.q)}`;
  }
}

/** Parse a location.hash; null when it doesn't name a view (e.g. first load). */
export function parseRoute(hash: string): Route | null {
  const parts = hash.replace(/^#\/?/, "").split("/").map(decodeURIComponent);
  const [project, kind, ...rest] = parts;
  if (!project) return null;
  switch (kind) {
    case undefined:
    case "":
      return { t: "home", project };
    case "m": {
      const [crate, module] = rest;
      if (crate === undefined) return null;
      return { t: "module", project, crate, module: module ?? "" };
    }
    case "s": {
      const id = Number(rest[0]);
      return Number.isInteger(id) ? { t: "symbol", project, id } : null;
    }
    case "t": {
      const id = Number(rest[0]);
      const dir = rest[1] === "in" ? "in" : "out";
      const depth = Math.min(6, Math.max(1, Number(rest[2]) || 3));
      const externals = rest[3] === "1";
      return Number.isInteger(id) ? { t: "trace", project, id, dir, depth, externals } : null;
    }
    case "q": {
      const q = rest.join("/");
      return q ? { t: "search", project, q } : { t: "home", project };
    }
    default:
      return null;
  }
}

export function navigate(route: Route): void {
  location.hash = formatRoute(route);
}
