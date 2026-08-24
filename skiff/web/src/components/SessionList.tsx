import type { SessionSummary, SourceHealth } from "../types"

/**
 * The session list. DW-001 rule 1: whitespace separates rows — the gap is the
 * divider, and there is no border anywhere.
 */
export function SessionList({ sessions }: { sessions: SessionSummary[] }) {
  if (sessions.length === 0) {
    return <p className="text-muted">No sessions yet.</p>
  }
  return (
    <ul className="flex flex-col gap-6">
      {sessions.map((session) => (
        <li key={session.id}>
          <Session session={session} />
        </li>
      ))}
    </ul>
  )
}

function Session({ session }: { session: SessionSummary }) {
  return (
    <article className="flex flex-col gap-1">
      <h2 className="font-heading text-xl tracking-tight">
        {session.title ?? <span className="text-muted">untitled</span>}
      </h2>
      <p className="instrumentation flex flex-wrap gap-x-3">
        <span>{session.harness}</span>
        {session.model && <span>{session.model}</span>}
        {session.orchestratorActive && <span>orchestrator</span>}
        <Relative ms={session.updatedMs} />
      </p>
      {session.directory && (
        <p className="font-mono text-sm text-muted">{session.directory}</p>
      )}
    </article>
  )
}

/**
 * Elapsed time, in the coarsest unit that still says something. Rendered at
 * the resolution the reader can act on: "3m" is useful, "3m 12s" is noise.
 */
function Relative({ ms }: { ms: number | null }) {
  if (ms === null) return <span>never</span>
  const seconds = Math.max(0, Math.round((Date.now() - ms) / 1000))
  if (seconds < 60) return <span>just now</span>
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return <span>{minutes}m ago</span>
  const hours = Math.round(minutes / 60)
  if (hours < 24) return <span>{hours}h ago</span>
  return <span>{Math.round(hours / 24)}d ago</span>
}

/**
 * DW-004 §4: a source that cannot be read is named, never swallowed. "No muse
 * sessions" and "muse is unreachable" look identical in a list, and the
 * difference is the whole reason health is surfaced at all.
 */
export function SourceErrors({ sources }: { sources: SourceHealth[] }) {
  const failing = sources.filter((source) => source.error !== null)
  if (failing.length === 0) return null
  return (
    <ul className="flex flex-col gap-1">
      {failing.map((source) => (
        <li key={source.source} className="text-danger text-sm">
          <span className="instrumentation text-danger">{source.source}</span>{" "}
          {source.error}
        </li>
      ))}
    </ul>
  )
}
