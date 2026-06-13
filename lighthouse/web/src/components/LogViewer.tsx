import { useEffect, useRef, useState } from "react";

import type { LogEntry } from "../types.ts";
import { fetchLogs, logStreamUrl } from "../api.ts";
import { cn, formatTimestamp } from "../lib/utils.ts";

/** Cap retained lines so a long-running live tail can't grow without bound. */
const MAX_LINES = 5_000;

function priorityColor(priority: number): string {
  if (priority <= 3) return "text-red-400"; // emerg…err
  if (priority === 4) return "text-amber-400"; // warning
  if (priority >= 7) return "text-slate-500"; // debug
  return "text-slate-300"; // notice/info
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
      <div className="flex items-center justify-between border-b border-slate-800 px-4 py-3">
        <h3 className="text-sm font-medium tracking-wide text-slate-400 uppercase">
          Logs
        </h3>
        <label className="flex items-center gap-2 text-sm text-slate-300">
          <input
            type="checkbox"
            checked={live}
            onChange={(event) => setLive(event.target.checked)}
            className="accent-emerald-500"
          />
          Live
          {live && (
            <span className="h-2 w-2 animate-pulse rounded-full bg-emerald-500" />
          )}
        </label>
      </div>
      {error && (
        <div className="bg-red-950/50 px-4 py-2 text-sm text-red-300">
          {error}
        </div>
      )}
      <div className="flex-1 overflow-auto bg-[#06090d] p-4 font-mono text-xs leading-relaxed">
        {logs.length === 0 ? (
          <div className="text-slate-600">No log entries.</div>
        ) : (
          logs.map((entry, index) => (
            <div
              key={`${entry.timestamp_us}-${index}`}
              className="whitespace-pre-wrap break-words"
            >
              <span className="mr-3 select-none text-slate-600">
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
