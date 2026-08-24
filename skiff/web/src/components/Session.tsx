import type { SessionView } from "../types"
import { useView } from "../lib/useView"
import { Transcript } from "./Transcript"

export function Session({ id, onBack }: { id: string; onBack: () => void }) {
  const view = useView({ kind: "session", id } as const)

  return (
    <div className="flex flex-col gap-8">
      <button
        type="button"
        onClick={onBack}
        className="instrumentation cursor-pointer self-start text-accent"
      >
        ← sessions
      </button>
      {view.status === "loading" && <p className="text-muted">Loading…</p>}
      {view.status === "error" && <p className="text-danger">{view.error}</p>}
      {view.status === "ready" && <Loaded view={view.data} />}
    </div>
  )
}

function Loaded({ view }: { view: SessionView }) {
  // An absent session is named, not rendered as an empty transcript — the two
  // look identical otherwise, and only one of them means "this is gone".
  if (!view.session) {
    return <p className="text-muted">That session no longer exists.</p>
  }
  const { session } = view
  return (
    <>
      <header className="flex flex-col gap-1">
        <h1 className="font-heading text-2xl tracking-tight">
          {session.title ?? <span className="text-muted">untitled</span>}
        </h1>
        <p className="instrumentation flex flex-wrap gap-x-3">
          <span>{session.harness}</span>
          {session.model && <span>{session.model}</span>}
          {session.orchestratorActive && <span>orchestrator</span>}
        </p>
        {session.directory && (
          <p className="font-mono text-sm text-muted">{session.directory}</p>
        )}
      </header>
      <Transcript messages={view.messages} harness={session.harness} />
    </>
  )
}
