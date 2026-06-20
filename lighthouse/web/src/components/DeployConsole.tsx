import { useEffect, useRef, useState } from "react";

import { deployStreamUrl } from "../api.ts";
import { cn } from "../lib/utils.ts";

/** One event from the deploy stream: a transcript line, or the final outcome. */
type DeployEvent =
  | { type: "line"; text: string }
  | { type: "done"; ok: boolean; error?: string };

interface Outcome {
  ok: boolean;
  error?: string;
}

interface Props {
  unit: string;
  jobId: string;
  /** Dismiss the console and return to the log view. */
  onClose: () => void;
  /** Called once the deploy finishes, so the dashboard can refresh status. */
  onDone: () => void;
}

export function DeployConsole({ unit, jobId, onClose, onDone }: Props) {
  const [lines, setLines] = useState<string[]>([]);
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  // Hold the latest onDone without making it an effect dependency, so a parent
  // re-render can't tear down and re-open the stream mid-deploy.
  const onDoneRef = useRef(onDone);
  onDoneRef.current = onDone;

  useEffect(() => {
    setLines([]);
    setOutcome(null);
    const source = new EventSource(deployStreamUrl(unit, jobId));

    source.onmessage = (event) => {
      const msg = JSON.parse(event.data) as DeployEvent;
      if (msg.type === "line") {
        setLines((prev) => [...prev, msg.text]);
      } else {
        setOutcome({ ok: msg.ok, error: msg.error });
        // The server closes the stream after this; close first so the browser
        // doesn't auto-reconnect.
        source.close();
        onDoneRef.current();
      }
    };
    source.onerror = () => {
      // A normal end-of-stream also surfaces here; only treat it as an error if
      // we never saw the terminal event.
      setOutcome((prev) => prev ?? { ok: false, error: "deploy stream disconnected" });
      source.close();
    };

    return () => source.close();
  }, [unit, jobId]);

  // Keep the newest line in view.
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "auto" });
  }, [lines, outcome]);

  const running = outcome === null;

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-rule-strong px-4 py-3">
        <div className="flex items-center gap-2">
          <h3 className="text-xs font-bold tracking-[0.15em] text-ink-muted uppercase">
            Deploy
          </h3>
          <span
            className={cn(
              "text-xs uppercase tracking-wide",
              running && "text-accent",
              outcome?.ok && "text-active",
              outcome && !outcome.ok && "text-failed",
            )}
          >
            {running ? "running…" : outcome.ok ? "succeeded" : "failed"}
          </span>
          {running && <span className="h-2 w-2 bg-accent" />}
        </div>
        <button
          type="button"
          onClick={onClose}
          disabled={running}
          className={cn(
            "border border-rule-strong bg-surface px-3 py-1.5 text-xs uppercase tracking-wide text-ink-muted hover:border-accent",
            running && "cursor-not-allowed opacity-40",
          )}
        >
          Close
        </button>
      </div>
      {outcome && !outcome.ok && outcome.error && (
        <div className="border-l-2 border-failed bg-failed/10 px-4 py-2 text-xs text-failed">
          {outcome.error}
        </div>
      )}
      <div className="flex-1 overflow-auto bg-bg p-4 font-mono text-xs leading-relaxed">
        {lines.length === 0 && running ? (
          <div className="text-ink-faint uppercase tracking-wide">Starting…</div>
        ) : (
          lines.map((line, index) => (
            <div key={index} className="whitespace-pre-wrap break-words text-ink-muted">
              {line}
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
