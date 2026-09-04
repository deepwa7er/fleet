import type { SessionView } from "../types"
import { client } from "../lib/socket"
import { useView } from "../lib/useView"
import { Composer } from "./Composer"
import { SessionControls } from "./SessionControls"
import { Transcript } from "./Transcript"

export function Session({
  id,
  fresh,
  onClose,
  onOpenChange,
}: {
  id: string
  /** The id was just minted here; the session may not exist yet. */
  fresh: boolean
  onClose: () => void
  onOpenChange: (repo: string, card: number) => void
}) {
  const view = useView({ kind: "session", id } as const)

  return (
    <div className="flex flex-col gap-8">
      <button
        type="button"
        onClick={onClose}
        className="instrumentation cursor-pointer self-start text-accent"
      >
        Close session
      </button>
      {view.status === "loading" && <p className="text-muted">Loading…</p>}
      {view.status === "error" && <p className="text-danger">{view.error}</p>}
      {view.status === "ready" && <Loaded id={id} fresh={fresh} view={view.data} onOpenChange={onOpenChange} />}
    </div>
  )
}

function Loaded({
  id,
  fresh,
  view,
  onOpenChange,
}: {
  id: string
  fresh: boolean
  view: SessionView
  onOpenChange: (repo: string, card: number) => void
}) {
  // An absent session is named, not rendered as an empty transcript — the two
  // look identical otherwise, and only one of them means "this is gone". A
  // freshly minted id is the exception: it names a chat about to start.
  if (!view.session) {
    if (!fresh) {
      return <p className="text-muted">That session no longer exists.</p>
    }
    return (
      <>
        <header className="flex flex-col gap-1">
          <h1 className="font-heading text-2xl tracking-tight">New chat</h1>
          <p className="instrumentation flex flex-wrap gap-x-3">
            <span>muse</span>
          </p>
          <p className="text-sm text-muted">Send the first message to start the session.</p>
        </header>
        <Transcript messages={[]} live={view.live} harness="muse" />
        <Composer session={id} working={view.live.working} />
      </>
    )
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
          {view.live.working && <span>working</span>}
        </p>
        {session.directory && (
          <p className="font-mono text-sm text-muted">{session.directory}</p>
        )}
      </header>
      <SessionControls view={view} command={(command) => client.command(command)} />
      {view.change ? (
        <button
          type="button"
          className="physical-key self-start bg-fill px-4 py-2 text-left"
          onClick={() => onOpenChange(view.change!.repo, view.change!.card)}
        >
          <span className="block">Review {view.change.title ?? `change #${view.change.card}`}</span>
          <span className="instrumentation">{view.change.repo} #{view.change.card} · open beside transcript</span>
        </button>
      ) : null}
      <Transcript messages={view.messages} live={view.live} harness={session.harness} />
      {/* All three harnesses expose the same typed send/abort intent here;
          their incompatible process models end at Rust's adapter boundary. */}
      <Composer session={session.id} working={view.live.working} />
    </>
  )
}
