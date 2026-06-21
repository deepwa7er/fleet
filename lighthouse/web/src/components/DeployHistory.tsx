import { useEffect, useState } from "react";

import type { DeployHistoryEntry } from "../types.ts";
import { fetchDeployHistory } from "../api.ts";
import { cn, formatRelative } from "../lib/utils.ts";
import { TranscriptView } from "./TranscriptView.tsx";

interface Props {
  unit: string;
}

export function DeployHistory({ unit }: Props) {
  // `null` means still loading; `[]` means loaded but empty.
  const [entries, setEntries] = useState<DeployHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // The deploy whose saved transcript is open, if any.
  const [selected, setSelected] = useState<DeployHistoryEntry | null>(null);

  useEffect(() => {
    let cancelled = false;
    setEntries(null);
    setError(null);
    setSelected(null);
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

  if (selected?.id) {
    const verdict =
      selected.result === "rolled_back" ? "rolled back" : "deployed";
    return (
      <TranscriptView
        unit={unit}
        id={selected.id}
        label={`${selected.short} · ${verdict}`}
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
            return (
              <div
                key={`${entry.sha}-${entry.at}-${index}`}
                onClick={hasLog ? () => setSelected(entry) : undefined}
                title={hasLog ? "View deploy log" : undefined}
                className={cn(
                  "flex items-center gap-3 border-b border-rule py-1.5 last:border-0",
                  hasLog && "cursor-pointer hover:bg-surface",
                )}
              >
                <span className="font-mono text-ink">{entry.short}</span>
                {entry.branch && (
                  <span className="text-ink-muted">{entry.branch}</span>
                )}
                {entry.dirty && (
                  <span className="uppercase tracking-wide text-warn">dirty</span>
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
                <span className="ml-auto shrink-0 text-ink-faint">
                  {formatRelative(entry.at)}
                </span>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
