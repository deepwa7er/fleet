import { Fragment, useEffect, useState } from "react";

import type { ChangelogCommit, DeployHistoryEntry } from "../types.ts";
import { fetchChangelog, fetchDeployHistory } from "../api.ts";
import { cn, formatRelative } from "../lib/utils.ts";
import { CommitLink } from "./CommitLink.tsx";
import { TranscriptView } from "./TranscriptView.tsx";

interface Props {
  unit: string;
}

/** The lazily-loaded changelog for one deploy row. */
interface ChangelogState {
  loading: boolean;
  error: string | null;
  commits: ChangelogCommit[] | null;
}

/** The sha of the nearest older deploy that actually went live — the code that
 *  was running before `entries[index]`, and so the base of its changelog. Skips
 *  rolled-back attempts (they never changed the running version). `null` when
 *  there's no earlier deployed entry in view (e.g. the first deploy). */
function prevDeployedSha(
  entries: DeployHistoryEntry[],
  index: number,
): string | null {
  for (let j = index + 1; j < entries.length; j++) {
    if (entries[j].result === "deployed") return entries[j].sha;
  }
  return null;
}

export function DeployHistory({ unit }: Props) {
  // `null` means still loading; `[]` means loaded but empty.
  const [entries, setEntries] = useState<DeployHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The deploy whose saved transcript is open, if any.
  const [selected, setSelected] = useState<DeployHistoryEntry | null>(null);
  // The row whose changelog is expanded inline, keyed by row key, and the
  // changelogs fetched so far (kept across collapses so reopening is instant).
  const [expandedKey, setExpandedKey] = useState<string | null>(null);
  const [changelogs, setChangelogs] = useState<Record<string, ChangelogState>>(
    {},
  );

  useEffect(() => {
    let cancelled = false;
    setEntries(null);
    setError(null);
    setSelected(null);
    setExpandedKey(null);
    setChangelogs({});
    fetchDeployHistory(unit).then(
      (loaded) => {
        if (!cancelled) setEntries(loaded);
      },
      (err: unknown) => {
        if (!cancelled) setError(String(err));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [unit]);

  function toggleChangelog(key: string, from: string, to: string) {
    if (expandedKey === key) {
      setExpandedKey(null);
      return;
    }
    setExpandedKey(key);
    if (changelogs[key]) return; // already loaded (or loading)
    setChangelogs((prev) => ({
      ...prev,
      [key]: { loading: true, error: null, commits: null },
    }));
    fetchChangelog(unit, from, to).then(
      (commits) =>
        setChangelogs((prev) => ({
          ...prev,
          [key]: { loading: false, error: null, commits },
        })),
      (err: unknown) =>
        setChangelogs((prev) => ({
          ...prev,
          [key]: { loading: false, error: String(err), commits: null },
        })),
    );
  }

  if (selected?.id) {
    const status =
      selected.result === "rolled_back" ? "rolled back" : "deployed";
    return (
      <TranscriptView
        unit={unit}
        id={selected.id}
        short={selected.short}
        commitUrl={selected.commit_url}
        status={status}
        onBack={() => setSelected(null)}
      />
    );
  }

  return (
    <div className="flex h-full flex-col">
      <div className="border-b border-rule-strong px-4 py-3">
        <h3 className="text-xs font-bold tracking-[0.15em] text-ink-muted uppercase">
          Deploys
        </h3>
      </div>
      {error && (
        <div className="border-l-2 border-failed bg-failed/10 px-4 py-2 text-xs text-failed">
          {error}
        </div>
      )}
      <div className="flex-1 overflow-auto p-4 text-xs leading-relaxed">
        {entries === null ? (
          <div className="text-ink-faint uppercase tracking-wide">Loading…</div>
        ) : entries.length === 0 ? (
          <div className="text-ink-faint uppercase tracking-wide">
            No deploys recorded.
          </div>
        ) : (
          entries.map((entry, index) => {
            // A transcript exists only for v2+ deploys (those with an id).
            const hasLog = entry.id !== null;
            const key = `${entry.sha}-${entry.at}-${index}`;
            // A changelog exists for a deploy that went live and has an earlier
            // live deploy to diff against (the previous running code).
            const from =
              entry.result === "deployed"
                ? prevDeployedSha(entries, index)
                : null;
            const expanded = expandedKey === key;
            const log = changelogs[key];
            return (
              <Fragment key={key}>
                <div
                  onClick={hasLog ? () => setSelected(entry) : undefined}
                  title={hasLog ? "View deploy log" : undefined}
                  className={cn(
                    "flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-rule py-1.5",
                    !expanded && "last:border-0",
                    hasLog && "cursor-pointer hover:bg-surface",
                  )}
                >
                  <CommitLink
                    short={entry.short}
                    url={entry.commit_url}
                    className="text-ink"
                  />
                  {entry.branch && (
                    <span className="text-ink-muted">{entry.branch}</span>
                  )}
                  {entry.dirty && (
                    <span className="uppercase tracking-wide text-warn">
                      dirty
                    </span>
                  )}
                  <span
                    className={cn(
                      "uppercase tracking-wide",
                      entry.result === "rolled_back"
                        ? "text-failed"
                        : "text-active",
                    )}
                  >
                    {entry.result === "rolled_back" ? "rolled back" : "deployed"}
                  </span>
                  {hasLog && (
                    <span className="text-ink-faint uppercase tracking-wide">
                      log
                    </span>
                  )}
                  {from && (
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        toggleChangelog(key, from, entry.sha);
                      }}
                      title="Commits this deploy shipped"
                      className={cn(
                        "uppercase tracking-wide hover:text-ink",
                        expanded ? "text-ink" : "text-ink-faint",
                      )}
                    >
                      {expanded ? "hide ▾" : "changes ▸"}
                    </button>
                  )}
                  <span className="ml-auto shrink-0 text-ink-faint">
                    {formatRelative(entry.at)}
                  </span>
                </div>
                {expanded && (
                  <div className="border-b border-rule py-1.5 pl-4 last:border-0">
                    {!log || log.loading ? (
                      <div className="text-ink-faint uppercase tracking-wide">
                        Loading…
                      </div>
                    ) : log.error ? (
                      <div className="text-failed">
                        Changelog unavailable (is the build host awake?)
                      </div>
                    ) : !log.commits || log.commits.length === 0 ? (
                      <div className="text-ink-faint uppercase tracking-wide">
                        No new commits.
                      </div>
                    ) : (
                      <ul className="flex flex-col gap-y-1">
                        {log.commits.map((commit, ci) => (
                          <li
                            key={`${commit.short}-${ci}`}
                            className="flex flex-wrap items-baseline gap-x-2"
                          >
                            <CommitLink
                              short={commit.short}
                              url={commit.commit_url}
                              className="text-ink-muted"
                            />
                            <span className="text-ink">{commit.subject}</span>
                            <span className="ml-auto shrink-0 text-ink-faint">
                              {formatRelative(commit.at)}
                            </span>
                          </li>
                        ))}
                      </ul>
                    )}
                  </div>
                )}
              </Fragment>
            );
          })
        )}
      </div>
    </div>
  );
}
