import { useEffect, useMemo, useState } from "react";

import type { HealthBucket, HealthHistory as History } from "../types.ts";
import { fetchHealthHistory } from "../api.ts";
import { cn, formatBytes } from "../lib/utils.ts";

interface Props {
  unit: string;
}

const WINDOWS = [
  { label: "24h", secs: 86_400 },
  { label: "7d", secs: 604_800 },
] as const;

/** Tailwind background for a timeline cell. `unreachable` (systemd active but not
 *  reachable through breakwater) is a warn state; `down` (not active) is failed;
 *  `gap` (no samples) is idle. */
const CELL_BG: Record<HealthBucket["status"], string> = {
  up: "bg-active",
  unreachable: "bg-warn",
  down: "bg-failed",
  gap: "bg-idle/40",
};

function pctLabel(pct: number): string {
  // Avoid a misleading "100%" when a single sample failed.
  return `${pct.toFixed(pct >= 99.95 || pct === 0 ? 0 : 1)}%`;
}

function clockTime(epochSeconds: number): string {
  return new Date(epochSeconds * 1000).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
  });
}

function cellTitle(b: HealthBucket): string {
  const when = new Date(b.at * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
  const label =
    b.status === "gap" ? "no data" : b.status === "up" ? "reachable" : b.status;
  const mem = b.memory_bytes != null ? ` · ${formatBytes(b.memory_bytes)}` : "";
  return `${when} — ${label}${mem}`;
}

/** A minimal SVG sparkline of the buckets' peak memory. */
function MemorySparkline({ buckets }: { buckets: HealthBucket[] }) {
  const points = useMemo(() => {
    const vals = buckets.map((b) => b.memory_bytes);
    const present = vals.filter((v): v is number => v != null);
    if (present.length < 2) return null;
    const max = Math.max(...present);
    const min = Math.min(...present);
    const span = max - min || 1;
    const n = buckets.length;
    // Skip gaps: a null bucket breaks the line into separate segments.
    const segments: string[] = [];
    let current: string[] = [];
    buckets.forEach((b, i) => {
      if (b.memory_bytes == null) {
        if (current.length) segments.push(current.join(" "));
        current = [];
        return;
      }
      const x = (i / (n - 1)) * 100;
      const y = 100 - ((b.memory_bytes - min) / span) * 100;
      current.push(`${x.toFixed(2)},${y.toFixed(2)}`);
    });
    if (current.length) segments.push(current.join(" "));
    return segments;
  }, [buckets]);

  if (!points) return null;
  return (
    <svg
      viewBox="0 0 100 100"
      preserveAspectRatio="none"
      className="h-12 w-full"
      aria-hidden
    >
      {points.map((seg, i) => (
        <polyline
          key={i}
          points={seg}
          fill="none"
          stroke="currentColor"
          strokeWidth={1.2}
          vectorEffect="non-scaling-stroke"
          className="text-accent"
        />
      ))}
    </svg>
  );
}

export function HealthHistory({ unit }: Props) {
  const [windowSecs, setWindowSecs] = useState<number>(WINDOWS[0].secs);
  const [history, setHistory] = useState<History | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    fetchHealthHistory(unit, windowSecs).then(
      (h) => {
        if (!cancelled) {
          setHistory(h);
          setLoading(false);
        }
      },
      (err: unknown) => {
        if (!cancelled) {
          // A 503 means history collection is off or its db couldn't open.
          setError(
            String(err).includes("503")
              ? "History collection is not enabled on this server."
              : String(err),
          );
          setLoading(false);
        }
      },
    );
    return () => {
      cancelled = true;
    };
  }, [unit, windowSecs]);

  const summary = history?.summary;

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center justify-between border-b border-rule-strong px-4 py-3">
        <h3 className="text-xs font-bold tracking-[0.15em] text-ink-muted uppercase">
          Health
        </h3>
        <div className="flex gap-1">
          {WINDOWS.map((w) => (
            <button
              key={w.secs}
              type="button"
              onClick={() => setWindowSecs(w.secs)}
              className={cn(
                "border px-2 py-0.5 text-[11px] uppercase tracking-wide",
                windowSecs === w.secs
                  ? "border-accent text-accent"
                  : "border-rule-strong text-ink-faint hover:text-ink-muted",
              )}
            >
              {w.label}
            </button>
          ))}
        </div>
      </div>

      {error && (
        <div className="border-l-2 border-failed bg-failed/10 px-4 py-2 text-xs text-failed">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-auto p-4 text-xs">
        {loading ? (
          <div className="text-ink-faint uppercase tracking-wide">Loading…</div>
        ) : !history || !summary ? null : summary.sample_count === 0 ? (
          <div className="text-ink-faint uppercase tracking-wide">
            No samples yet — the collector records on an interval; check back
            shortly.
          </div>
        ) : (
          <div className="flex flex-col gap-5">
            {/* Rollup figures */}
            <div className="flex flex-wrap gap-x-8 gap-y-3">
              <Stat
                label="systemd uptime"
                value={pctLabel(summary.systemd_uptime_pct)}
              />
              {summary.probed && summary.probe_uptime_pct != null && (
                <Stat
                  label="reachable (via breakwater)"
                  value={pctLabel(summary.probe_uptime_pct)}
                />
              )}
              {summary.memory_current != null && (
                <Stat
                  label="memory"
                  value={formatBytes(summary.memory_current)}
                  sub={
                    summary.memory_peak != null
                      ? `peak ${formatBytes(summary.memory_peak)}`
                      : undefined
                  }
                />
              )}
            </div>

            {/* Current reachability — the headline out-of-loopback signal. */}
            {summary.current && (
              <div
                className={cn(
                  "flex items-center gap-2 border-l-2 px-3 py-1.5",
                  summary.current.ok
                    ? "border-active text-active"
                    : "border-warn text-warn",
                )}
              >
                <span className="uppercase tracking-wide">
                  through breakwater:
                </span>
                {summary.current.ok ? (
                  <span>
                    reachable
                    {summary.current.status != null &&
                      ` · ${summary.current.status}`}
                    {summary.current.ms != null && ` · ${summary.current.ms}ms`}
                  </span>
                ) : (
                  <span>unreachable since {clockTime(summary.current.since)}</span>
                )}
              </div>
            )}

            {/* Uptime timeline */}
            <div className="flex flex-col gap-1.5">
              <div className="flex h-8 gap-px overflow-hidden">
                {history.buckets.map((b, i) => (
                  <div
                    key={i}
                    title={cellTitle(b)}
                    className={cn("h-full flex-1", CELL_BG[b.status])}
                  />
                ))}
              </div>
              <div className="flex justify-between text-[10px] uppercase tracking-wide text-ink-faint">
                <span>{windowSecs >= 604_800 ? "7 days ago" : "24h ago"}</span>
                <span>now</span>
              </div>
            </div>

            {/* Memory sparkline */}
            {summary.memory_peak != null && (
              <div className="flex flex-col gap-1">
                <div className="text-[10px] uppercase tracking-wide text-ink-faint">
                  memory
                </div>
                <MemorySparkline buckets={history.buckets} />
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  sub,
}: {
  label: string;
  value: string;
  sub?: string;
}) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[10px] uppercase tracking-wide text-ink-faint">
        {label}
      </span>
      <span className="text-lg font-bold text-ink">{value}</span>
      {sub && <span className="text-[10px] text-ink-faint">{sub}</span>}
    </div>
  );
}
