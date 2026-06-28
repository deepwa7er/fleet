import type { ReactNode } from "react";
import type { LineMatch, SearchResults } from "../api";

interface Props {
  results: SearchResults | null;
  searching: boolean;
  error: string | null;
  onOpenMatch: (repo: string, path: string, line: number) => void;
}

export function SearchPanel({ results, searching, error, onOpenMatch }: Props) {
  if (error) return <div className="error pad">{error}</div>;
  if (searching) return <div className="muted pad">searching…</div>;
  if (!results) return null;

  if (results.total === 0) {
    return (
      <div className="searchpanel">
        <div className="search-summary">
          no matches for <code>{results.query}</code>
        </div>
      </div>
    );
  }

  return (
    <div className="searchpanel">
      <div className="search-summary">
        {results.total} match{results.total === 1 ? "" : "es"} for{" "}
        <code>{results.query}</code>
        {results.truncated && <span className="trunc"> · truncated</span>}
      </div>
      {results.repos.map((repo) => (
        <section key={repo.repo} className="search-repo">
          <div className="pane-label">{repo.repo}</div>
          {repo.files.map((file) => (
            <div key={file.path} className="search-file">
              <div className="search-path">{file.path}</div>
              <ul className="search-lines">
                {file.matches.map((m, i) => (
                  <li key={`${m.line_number}-${i}`}>
                    <button
                      className="search-hit"
                      onClick={() =>
                        onOpenMatch(repo.repo, file.path, m.line_number)
                      }
                    >
                      <span className="ln">{m.line_number}</span>
                      <code className="lt">{renderLine(m)}</code>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

// Render a match line, wrapping each matched byte-range in <mark>. ripgrep's
// offsets are byte offsets into the (UTF-8) line; for the ASCII-dominant source
// we view they line up with JS string indices, and any multibyte drift only
// shifts the highlight, never the text.
function renderLine(m: LineMatch) {
  if (m.ranges.length === 0) return m.text;
  const parts: ReactNode[] = [];
  let cursor = 0;
  m.ranges.forEach(([start, end], i) => {
    if (start > cursor) parts.push(m.text.slice(cursor, start));
    parts.push(<mark key={i}>{m.text.slice(start, end)}</mark>);
    cursor = end;
  });
  if (cursor < m.text.length) parts.push(m.text.slice(cursor));
  return parts;
}
