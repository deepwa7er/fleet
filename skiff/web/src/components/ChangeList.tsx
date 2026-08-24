import type { ChangeRef } from "../types"

export function ChangeList({
  changes,
  onOpen,
}: {
  changes: ChangeRef[]
  onOpen: (repo: string, card: number) => void
}) {
  if (changes.length === 0) return <p className="text-muted">No changes yet.</p>
  return (
    <section className="flex flex-col gap-3" aria-label="Changes">
      <p className="instrumentation">Changes · {changes.length}</p>
      {changes.map((change) => (
        <button
          className="group flex items-baseline justify-between gap-6 py-2 text-left"
          key={`${change.repo}/${change.card}`}
          onClick={() => onOpen(change.repo, change.card)}
        >
          <span>
            <span className="font-heading text-xl group-hover:text-accent">
              {change.title ?? `Change #${change.card}`}
            </span>
            <span className="ml-3 text-sm text-muted">
              {change.repo} #{change.card}
            </span>
          </span>
          <span className="instrumentation shrink-0">
            {change.rounds} round{change.rounds === 1 ? "" : "s"} · {change.state.replace("_", " ")}
          </span>
        </button>
      ))}
    </section>
  )
}
