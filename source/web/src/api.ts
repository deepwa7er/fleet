// Typed wrappers over source's JSON API. In production the same axum binary
// serves this bundle, so all calls are same-origin under /api.

export interface Repo {
  name: string;
}

export interface Tree {
  repo: string;
  files: string[];
}

export interface FileContents {
  repo: string;
  path: string;
  language: string;
  bytes: number;
  binary: boolean;
  too_large: boolean;
  content: string | null;
}

export interface LineMatch {
  line_number: number;
  text: string;
  ranges: [number, number][];
}

export interface FileMatches {
  path: string;
  matches: LineMatch[];
}

export interface RepoMatches {
  repo: string;
  files: FileMatches[];
}

export interface SearchResults {
  query: string;
  truncated: boolean;
  total: number;
  repos: RepoMatches[];
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

export function fetchRepos(): Promise<Repo[]> {
  return getJSON("/api/repos");
}

export function fetchTree(repo: string): Promise<Tree> {
  return getJSON(`/api/tree?repo=${encodeURIComponent(repo)}`);
}

export function fetchFile(repo: string, path: string): Promise<FileContents> {
  return getJSON(
    `/api/file?repo=${encodeURIComponent(repo)}&path=${encodeURIComponent(path)}`,
  );
}

export function search(
  query: string,
  repo: string | null,
  regex: boolean,
): Promise<SearchResults> {
  const params = new URLSearchParams({ q: query });
  if (repo) params.set("repo", repo);
  if (regex) params.set("regex", "1");
  return getJSON(`/api/search?${params.toString()}`);
}

export interface SaveResult {
  committed: boolean;
  pushed: boolean;
  commit: string | null;
  warning: string | null;
  file: FileContents;
}

export async function saveFile(
  repo: string,
  path: string,
  content: string,
  message: string,
): Promise<SaveResult> {
  const res = await fetch("/api/file", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ repo, path, content, message }),
  });
  if (!res.ok) {
    let msg = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) msg = body.error;
    } catch {
      // keep status line
    }
    throw new Error(msg);
  }
  return (await res.json()) as SaveResult;
}
