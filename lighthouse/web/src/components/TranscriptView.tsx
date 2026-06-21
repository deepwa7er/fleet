import { useEffect, useRef, useState } from "react";

import { fetchDeployLog } from "../api.ts";

interface Props {
  unit: string;
  /** Deploy id whose saved transcript to show. */
  id: string;
  /** Short header label, e.g. "1a2b3c4d · deployed". */
  label: string;
  /** Return to the deploy list. */
  onBack: () => void;
}

/** Read-only viewer for a past deploy's saved transcript (the live equivalent
 *  is DeployConsole). Fetches once and renders the lines in a mono console. */
export function TranscriptView({ unit, id, label, onBack }: Props) {
  // `null` means still loading; `[]` means loaded but empty.
  const [lines, setLines] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLines(null);
    setError(null);
    fetchDeployLog(unit, id).then(
      (loaded) => {
        if (!cancelled) setLines(loaded);
      },
      (err: unknown) => {
        if (!cancelled) setError(String(err));
      },
    );
    return () => {
      cancelled = true;
    };
  }, [unit, id]);

  // Jump to the end once loaded — a failed deploy's reason is at the bottom.
  useEffect(() => {
    if (lines) bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [lines]);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-rule-strong px-4 py-3">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-bold tracking-[0.15em] text-ink-muted uppercase">
            Deploy log
          </h3>
          <span className="font-mono text-xs text-ink-muted">{label}</span>
        </div>
        <button
          type="button"
          onClick={onBack}
          className="border border-rule-strong bg-surface px-3 py-1.5 text-xs uppercase tracking-wide text-ink-muted hover:border-accent"
        >
          Back
        </button>
      </div>
      {error && (
        <div className="border-l-2 border-failed bg-failed/10 px-4 py-2 text-xs text-failed">
          {error}
        </div>
      )}
      <div className="flex-1 overflow-auto bg-bg p-4 font-mono text-xs leading-relaxed">
        {lines === null && !error ? (
          <div className="text-ink-faint uppercase tracking-wide">Loading…</div>
        ) : lines && lines.length === 0 ? (
          <div className="text-ink-faint uppercase tracking-wide">
            Empty transcript.
          </div>
        ) : (
          lines?.map((line, index) => (
            <div
              key={index}
              className="whitespace-pre-wrap break-words text-ink-muted"
            >
              {line}
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
