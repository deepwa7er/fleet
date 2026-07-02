import { useEffect, useRef, useState } from "react";

import type { LogEntry } from "../types.ts";
import { fetchLogs, logStreamUrl } from "../api.ts";
import { cn, formatTimestamp } from "../lib/utils.ts";

/** Cap retained lines so a long-running live tail can't grow without bound. */
const MAX_LINES = 5_000;

function priorityColor(priority: number): string {
  if (priority <= 3) return "text-failed"; // emerg…err
  if (priority === 4) return "text-warn"; // warning
  if (priority >= 7) return "text-ink-faint"; // debug
  return "text-ink-muted"; // notice/info
}

interface Props {
  unit: string;
}

export function LogViewer({ unit }: Props) {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  // Live tailing is on by default; the user can uncheck it to freeze the view.
  const [live, setLive] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  // Load recent logs whenever the selected unit changes.
  useEffect(() => {
    let cancelled = false;
    setLogs([]);
    setError(null);
    fetchLogs(unit).then(
      (entries) => {
        if (!cancelled) setLogs(entries);
      },
      (err: unknown) => {
        if (!cancelled) setError(String(err));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [unit]);

  // Open the SSE stream while "Live" is on; close it on toggle off / unit change.
  useEffect(() => {
    if (!live) return;
    const source = new EventSource(logStreamUrl(unit));
    source.onmessage = (event) => {
      const entry = JSON.parse(event.data) as LogEntry;
      setLogs((prev) => {
        const next = [...prev, entry];
        return next.length > MAX_LINES
          ? next.slice(next.length - MAX_LINES)
          : next;
      });
    };
    source.onerror = () => setError("log stream disconnected");
    return () => source.close();
  }, [live, unit]);

  // Keep the newest line in view.
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [logs]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-rule-strong px-4 py-3">
        <h3 className="text-xs font-bold tracking-[0.15em] text-ink-muted uppercase">
          Logs
        </h3>
        <label className="flex items-center gap-2 text-xs tracking-wide text-ink-muted uppercase">
          <input
            type="checkbox"
            checked={live}
            onChange={(event) => setLive(event.target.checked)}
            className="accent-accent"
          />
          Live
          {live && <span className="h-2 w-2 bg-accent" />}
        </label>
      </div>
      {error && (
        <div className="border-l-2 border-failed bg-failed/10 px-4 py-2 text-xs text-failed">
          {error}
        </div>
      )}
      <div className="flex-1 overflow-auto bg-bg p-4 text-xs leading-relaxed">
        {logs.length === 0 ? (
          <div className="text-ink-faint uppercase tracking-wide">No log entries.</div>
        ) : (
          logs.map((entry, index) => (
            <div
              key={`${entry.timestamp_us}-${index}`}
              className="whitespace-pre-wrap break-words"
            >
              <span className="mr-3 select-none text-ink-faint">
                {formatTimestamp(entry.timestamp_us)}
              </span>
              <span className={cn(priorityColor(entry.priority))}>
                {entry.message}
              </span>
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
