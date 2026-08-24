import { SessionList, SourceErrors } from "./components/SessionList"
import { useConnection, useView } from "./lib/useView"

export function App() {
  const connection = useConnection()
  const sessions = useView({ kind: "sessions" } as const)

  return (
    <main className="mx-auto flex max-w-3xl flex-col gap-8 p-8">
      <header className="flex items-baseline justify-between gap-4">
        <h1 className="font-heading text-3xl tracking-tight">skiff</h1>
        <span className="instrumentation">{connection}</span>
      </header>

      {sessions.status === "loading" && <p className="text-muted">Loading…</p>}
      {sessions.status === "error" && <p className="text-danger">{sessions.error}</p>}
      {sessions.status === "ready" && (
        <>
          <SourceErrors sources={sessions.data.sources} />
          <SessionList sessions={sessions.data.sessions} />
        </>
      )}
    </main>
  )
}
