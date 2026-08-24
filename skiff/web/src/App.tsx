import { useEffect, useState } from "react"

import { Session } from "./components/Session"
import { SessionList, SourceErrors } from "./components/SessionList"
import { useConnection, useView } from "./lib/useView"

/**
 * Routing is the client's, and lives in the URL so a session is linkable and
 * the back button works (DW-004 §10: React owns intent and navigation).
 *
 * A hand-rolled two-route reader rather than a router: M6 replaces this with
 * the multi-pane workspace, and a router adopted now would be chosen for the
 * shape this milestone happens to have rather than the one that lasts.
 */
function useRoute(): [string | null, (id: string | null) => void] {
  const [path, setPath] = useState(() => location.pathname)

  useEffect(() => {
    const onPop = () => setPath(location.pathname)
    addEventListener("popstate", onPop)
    return () => removeEventListener("popstate", onPop)
  }, [])

  const navigate = (id: string | null) => {
    const next = id === null ? "/" : `/s/${encodeURIComponent(id)}`
    history.pushState(null, "", next)
    setPath(next)
  }

  const match = path.match(/^\/s\/(.+)$/)
  return [match?.[1] ? decodeURIComponent(match[1]) : null, navigate]
}

export function App() {
  const connection = useConnection()
  const [sessionId, navigate] = useRoute()

  return (
    <main className="mx-auto flex max-w-3xl flex-col gap-8 p-8">
      <header className="flex items-baseline justify-between gap-4">
        <h1 className="font-heading text-3xl tracking-tight">skiff</h1>
        <span className="instrumentation">{connection}</span>
      </header>

      {sessionId === null ? (
        <Sessions onOpen={(id) => navigate(id)} />
      ) : (
        <Session id={sessionId} onBack={() => navigate(null)} />
      )}
    </main>
  )
}

function Sessions({ onOpen }: { onOpen: (id: string) => void }) {
  const sessions = useView({ kind: "sessions" } as const)
  if (sessions.status === "loading") return <p className="text-muted">Loading…</p>
  if (sessions.status === "error") return <p className="text-danger">{sessions.error}</p>
  return (
    <>
      <SourceErrors sources={sessions.data.sources} />
      <SessionList sessions={sessions.data.sessions} onOpen={onOpen} />
    </>
  )
}
