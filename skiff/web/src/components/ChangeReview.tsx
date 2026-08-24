import { useState } from "react"

import { client } from "../lib/socket"
import { useView } from "../lib/useView"
import type { AnnotationSide } from "../types"
import { Diff } from "./Diff"

export function ChangeReview({
  repo,
  card,
  round,
  onRound,
  onClose,
}: {
  repo: string
  card: number
  round: number | null
  onRound: (round: number | null) => void
  onClose: () => void
}) {
  const view = useView({ kind: "change", repo, card, round } as const)
  const [note, setNote] = useState("")
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  if (view.status === "loading") return <p className="text-muted">Loading review…</p>
  if (view.status === "error") return <p className="text-danger">{view.error}</p>
  const { change, diff, unfinished, willDeploy } = view.data
  const selected = round === null ? null : change.rounds.find((candidate) => candidate.n === round)
  const annotations = selected?.annotations ?? []

  const command = async (run: () => Promise<void>) => {
    setBusy(true)
    setError(null)
    try {
      await run()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  return (
    <article className="flex flex-col gap-8">
      <header className="flex flex-wrap items-start justify-between gap-5">
        <div>
          <button className="text-sm text-accent" onClick={onClose}>Close change</button>
          <p className="instrumentation mt-5">{repo} #{card} · {change.state.replace("_", " ")}</p>
          <h2 className="font-heading text-4xl tracking-tight">{change.title ?? `Change #${card}`}</h2>
        </div>
        <p className="instrumentation">{change.rounds.length} round{change.rounds.length === 1 ? "" : "s"}</p>
      </header>

      <nav className="flex flex-wrap gap-3" aria-label="Diff scope">
        <button className={round === null ? "physical-key bg-ink px-4 py-1 text-paper" : "px-4 py-1 text-accent"} onClick={() => onRound(null)}>
          Cumulative
        </button>
        {change.rounds.map((candidate) => (
          <button
            key={candidate.n}
            className={round === candidate.n ? "physical-key bg-ink px-4 py-1 text-paper" : "px-4 py-1 text-accent"}
            onClick={() => onRound(candidate.n)}
          >
            Round {candidate.n}
          </button>
        ))}
      </nav>

      {selected ? (
        <section className="flex flex-wrap gap-x-8 gap-y-2 text-sm text-muted">
          <span>{selected.author}</span>
          <span>{selected.commit?.description ?? "commit unavailable"}</span>
          {selected.gatesRan.map((gate) => <span key={gate}>Claimed gate: {gate}</span>)}
          {selected.worthKnowing.map((fact) => <span key={fact}>{fact}</span>)}
        </section>
      ) : null}

      <Diff
        diff={diff}
        annotations={annotations}
        canAnnotate={change.state === "in_review" && selected !== null}
        onAnnotate={(anchor: { path: string; line: number; side: AnnotationSide }, text: string) =>
          client.command({
            kind: "annotateChange",
            repo,
            card,
            round: selected!.n,
            ...anchor,
            text,
          })
        }
      />

      {unfinished.length > 0 ? (
        <p className="text-danger">Unfinished landing tail: {unfinished.join(", ")}. Run <code>dw finish {card}</code>.</p>
      ) : null}
      {change.state === "in_review" ? (
        <section className="mt-5 grid gap-5 md:grid-cols-[1fr_auto]">
          <textarea
            className="input-surface min-h-28 px-4 py-3"
            placeholder="What should change, and why?"
            value={note}
            onChange={(event) => setNote(event.target.value)}
          />
          <div className="flex flex-col items-stretch justify-end gap-4">
            <button
              className="physical-key bg-fill px-5 py-3"
              disabled={busy || note.trim() === "" || change.session === null}
              onClick={() => void command(async () => {
                await client.command({ kind: "requestChanges", repo, card, note: note.trim() })
                setNote("")
              })}
            >
              Request changes
            </button>
            <button
              className="physical-key bg-accent px-5 py-3 text-accent-contrast"
              disabled={busy}
              onClick={() => void command(() => client.command({ kind: "approveChange", repo, card }))}
            >
              Approve{willDeploy === null ? "" : ` & deploy ${willDeploy}`}
            </button>
          </div>
        </section>
      ) : null}
      {error ? <p className="text-danger">{error}</p> : null}
    </article>
  )
}
