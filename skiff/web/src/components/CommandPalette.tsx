import { useEffect, useMemo, useRef, useState } from "react"

import type { ChangeRef, SessionSummary } from "../types"

type Choice = { key: string; label: string; detail: string; run: () => void }

export function CommandPalette({
  open,
  changes,
  sessions,
  onClose,
  onOpenChange,
  onOpenSession,
  onNewChat,
}: {
  open: boolean
  changes: ChangeRef[]
  sessions: SessionSummary[]
  onClose: () => void
  onOpenChange: (repo: string, card: number) => void
  onOpenSession: (id: string) => void
  onNewChat: () => void
}) {
  const [query, setQuery] = useState("")
  const [selected, setSelected] = useState(0)
  const input = useRef<HTMLInputElement>(null)
  const choices = useMemo<Choice[]>(
    () => [
      { key: "new-chat", label: "New chat", detail: "muse · start a session", run: onNewChat },
      ...changes.map((change) => ({
        key: `c:${change.repo}:${change.card}`,
        label: change.title ?? `Change #${change.card}`,
        detail: `${change.repo} #${change.card} · ${change.state.replace("_", " ")}`,
        run: () => onOpenChange(change.repo, change.card),
      })),
      ...sessions.map((session) => ({
        key: `s:${session.id}`,
        label: session.title ?? "untitled",
        detail: `${session.harness} · ${session.id}`,
        run: () => onOpenSession(session.id),
      })),
    ],
    [changes, sessions, onOpenChange, onOpenSession, onNewChat],
  )
  const filtered = choices.filter((choice) =>
    `${choice.label} ${choice.detail}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
  )

  useEffect(() => {
    if (!open) return
    setQuery("")
    setSelected(0)
    requestAnimationFrame(() => input.current?.focus())
  }, [open])

  if (!open) return null
  const choose = (choice: Choice | undefined) => {
    if (!choice) return
    choice.run()
    onClose()
  }
  return (
    <div className="palette-scrim" role="presentation" onMouseDown={onClose}>
      <section className="palette" role="dialog" aria-modal="true" aria-label="Command palette" onMouseDown={(event) => event.stopPropagation()}>
        <label className="instrumentation" htmlFor="palette-search">Open</label>
        <input
          ref={input}
          id="palette-search"
          className="input-surface mt-2 w-full px-4 py-3 text-lg"
          value={query}
          onChange={(event) => {
            setQuery(event.target.value)
            setSelected(0)
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") onClose()
            else if (event.key === "ArrowDown") {
              event.preventDefault()
              setSelected((selected + 1) % Math.max(filtered.length, 1))
            } else if (event.key === "ArrowUp") {
              event.preventDefault()
              setSelected((selected - 1 + Math.max(filtered.length, 1)) % Math.max(filtered.length, 1))
            } else if (event.key === "Enter") choose(filtered[selected])
          }}
        />
        <div className="mt-3 max-h-[50vh] overflow-y-auto">
          {filtered.map((choice, index) => (
            <button
              key={choice.key}
              className={index === selected ? "palette-row palette-row-selected" : "palette-row"}
              onMouseEnter={() => setSelected(index)}
              onClick={() => choose(choice)}
            >
              <span>{choice.label}</span>
              <span className="instrumentation">{choice.detail}</span>
            </button>
          ))}
          {filtered.length === 0 ? <p className="p-4 text-muted">No match.</p> : null}
        </div>
      </section>
    </div>
  )
}
