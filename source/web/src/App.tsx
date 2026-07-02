import { useCallback, useEffect, useRef, useState } from "react";
import {
  fetchFile,
  fetchRepos,
  fetchTree,
  search as searchApi,
  type FileContents,
  type Repo,
  type SearchResults,
  type Tree,
} from "./api";
import { RepoTree } from "./components/RepoTree";
import { FileView } from "./components/FileView";
import { SearchPanel } from "./components/SearchPanel";

// What the main pane is showing.
type Pane = "file" | "search" | "empty";

// A file to open, optionally scrolled to a line (from a search hit).
interface OpenTarget {
  repo: string;
  path: string;
  line?: number;
}

export function App() {
  const [repos, setRepos] = useState<Repo[]>([]);
  const [activeRepo, setActiveRepo] = useState<string | null>(null);
  const [tree, setTree] = useState<Tree | null>(null);
  const [treeError, setTreeError] = useState<string | null>(null);

  const [open, setOpen] = useState<OpenTarget | null>(null);
  const [file, setFile] = useState<FileContents | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);

  const [pane, setPane] = useState<Pane>("empty");

  // Search state.
  const [query, setQuery] = useState("");
  const [scopeToRepo, setScopeToRepo] = useState(false);
  const [regex, setRegex] = useState(false);
  const [results, setResults] = useState<SearchResults | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);

  // Monotonic request guards: ignore responses from superseded requests.
  const treeReq = useRef(0);
  const fileReq = useRef(0);
  const searchReq = useRef(0);

  useEffect(() => {
    fetchRepos()
      .then(setRepos)
      .catch((e) => setTreeError(String(e)));
  }, []);

  const selectRepo = useCallback((name: string) => {
    setActiveRepo(name);
    setTree(null);
    setTreeError(null);
    const req = ++treeReq.current;
    fetchTree(name)
      .then((t) => {
        if (treeReq.current === req) setTree(t);
      })
      .catch((e) => {
        if (treeReq.current === req) setTreeError(String(e));
      });
  }, []);

  const openFile = useCallback(
    (target: OpenTarget) => {
      setOpen(target);
      setPane("file");
      setFile(null);
      setFileError(null);
      // Make sure the sidebar tree follows the file's repo.
      if (target.repo !== activeRepo) selectRepo(target.repo);
      const req = ++fileReq.current;
      fetchFile(target.repo, target.path)
        .then((f) => {
          if (fileReq.current === req) setFile(f);
        })
        .catch((e) => {
          if (fileReq.current === req) setFileError(String(e));
        });
    },
    [activeRepo, selectRepo],
  );

  const runSearch = useCallback(() => {
    const q = query.trim();
    if (!q) return;
    setPane("search");
    setSearching(true);
    setSearchError(null);
    setResults(null);
    const req = ++searchReq.current;
    searchApi(q, scopeToRepo ? activeRepo : null, regex)
      .then((r) => {
        if (searchReq.current === req) setResults(r);
      })
      .catch((e) => {
        if (searchReq.current === req) setSearchError(String(e));
      })
      .finally(() => {
        if (searchReq.current === req) setSearching(false);
      });
  }, [query, scopeToRepo, activeRepo, regex]);

  return (
    <div className="app">
      <header>
        <div className="brand">
          <span className="wordmark">SOURCE</span>
          <span className="sub">fleet code · {repos.length} repos</span>
        </div>
        <form
          className="searchbar"
          onSubmit={(e) => {
            e.preventDefault();
            runSearch();
          }}
        >
          <input
            type="search"
            placeholder="Search the fleet…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            spellCheck={false}
            autoComplete="off"
          />
          <label className="opt" title="Treat the query as a regular expression">
            <input
              type="checkbox"
              checked={regex}
              onChange={(e) => setRegex(e.target.checked)}
            />
            regex
          </label>
          <label
            className="opt"
            title={
              activeRepo
                ? `Limit search to ${activeRepo}`
                : "Select a repo to enable scoping"
            }
          >
            <input
              type="checkbox"
              checked={scopeToRepo}
              disabled={!activeRepo}
              onChange={(e) => setScopeToRepo(e.target.checked)}
            />
            {activeRepo ? `in ${activeRepo}` : "in repo"}
          </label>
          <button type="submit">search</button>
        </form>
      </header>

      <div className="body">
        <aside className="sidebar">
          <RepoTree
            repos={repos}
            activeRepo={activeRepo}
            tree={tree}
            treeError={treeError}
            openPath={open?.repo === activeRepo ? open.path : null}
            onSelectRepo={selectRepo}
            onOpenFile={(repo, path) => openFile({ repo, path })}
          />
        </aside>

        <main className="main">
          {pane === "empty" && (
            <div className="placeholder">
              <p>Select a repo to browse, or search the whole fleet.</p>
            </div>
          )}
          {pane === "file" && (
            <FileView
              target={open}
              file={file}
              error={fileError}
              onSaved={setFile}
            />
          )}
          {pane === "search" && (
            <SearchPanel
              results={results}
              searching={searching}
              error={searchError}
              onOpenMatch={(repo, path, line) => openFile({ repo, path, line })}
            />
          )}
        </main>
      </div>
    </div>
  );
}
