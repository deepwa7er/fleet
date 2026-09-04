import { useMemo, useRef, useState } from "react"
import clsx from "clsx"

import type { ChangeRef, SessionSummary, SourceHealth } from "../types"
import { SourceErrors } from "./SessionList"

type Item =
  | { kind: "change"; key: string; change: ChangeRef }
  | { kind: "session"; key: string; session: SessionSummary }

export function DeskRail({
  changes,
  sessions,
  sources,
  open,
  onClose,
  onOpenChange,
  onOpenSession,
  onNewChat,
}: {
  changes: ChangeRef[]
  sessions: SessionSummary[]
  sources: SourceHealth[]
  open: boolean
  onClose: () => void
  onOpenChange: (repo: string, card: number) => void
  onOpenSession: (id: string) => void
  onNewChat: () => void
}) {
  const activeChanges = useMemo(
    () => changes.filter((change) => change.state !== "shipped"),
    [changes],
  )
  const items = useMemo<Item[]>(
    () => [
      ...activeChanges.map((change) => ({
        kind: "change" as const,
        key: `c:${change.repo}:${change.card}`,
        change,
      })),
      ...sessions.map((session) => ({ kind: "session" as const, key: `s:${session.id}`, session })),
    ],
    [activeChanges, sessions],
  )
  const [selected, setSelected] = useState(0)
  const buttons = useRef(new Map<number, HTMLButtonElement>())

  const move = (next: number) => {
    if (items.length === 0) return
    const index = (next + items.length) % items.length
    setSelected(index)
    buttons.current.get(index)?.focus()
  }
  const activate = (item: Item) => {
    if (item.kind === "change") onOpenChange(item.change.repo, item.change.card)
    else onOpenSession(item.session.id)
    onClose()
  }
  const startChat = () => {
    onNewChat()
    onClose()
  }

  return (
    <>
      {open ? <button className="desk-scrim" aria-label="Close desk" onClick={onClose} /> : null}
      <aside
        className={clsx("desk-rail", open && "desk-rail-open")}
        aria-label="Desk"
        onKeyDown={(event) => {
          if (event.key === "j" || event.key === "ArrowDown") {
            event.preventDefault()
            move(selected + 1)
          } else if (event.key === "k" || event.key === "ArrowUp") {
            event.preventDefault()
            move(selected - 1)
          } else if (event.key === "Enter" && items[selected]) {
            activate(items[selected])
          }
        }}
      >
        <header className="mb-8 flex items-baseline justify-between">
          <p className="font-heading text-2xl">skiff</p>
          <button className="text-accent lg:hidden" onClick={onClose}>Close</button>
        </header>
        <button className="rail-row text-accent" onClick={startChat}>
          <span>New chat</span>
          <span className="instrumentation">muse</span>
        </button>
        <SourceErrors sources={sources} />
        <section className="mt-7">
          <p className="instrumentation mb-3">Needs you · {activeChanges.length}</p>
          <div className="flex flex-col gap-1">
            {activeChanges.map((change, index) => (
              <button
                key={`${change.repo}/${change.card}`}
                ref={(element) => {
                  if (element) buttons.current.set(index, element)
                  else buttons.current.delete(index)
                }}
                className={clsx("rail-row", selected === index && "rail-row-selected")}
                onFocus={() => setSelected(index)}
                onClick={() => activate(items[index]!)}
              >
                <span className="truncate">{change.title ?? `Change #${change.card}`}</span>
                <span className="instrumentation">#{change.card} · {change.state.replace("_", " ")}</span>
              </button>
            ))}
            {activeChanges.length === 0 ? <p className="text-sm text-muted">Nothing waiting.</p> : null}
          </div>
        </section>
        <section className="mt-9">
          <p className="instrumentation mb-3">Sessions · {sessions.length}</p>
          <div className="flex flex-col gap-1">
            {sessions.map((session, offset) => {
              const index = activeChanges.length + offset
              return (
                <button
                  key={session.id}
                  ref={(element) => {
                    if (element) buttons.current.set(index, element)
                    else buttons.current.delete(index)
                  }}
                  className={clsx("rail-row", selected === index && "rail-row-selected")}
                  onFocus={() => setSelected(index)}
                  onClick={() => activate(items[index]!)}
                >
                  <span className="truncate">{session.title ?? "untitled"}</span>
                  <span className="instrumentation">{session.harness}{session.model ? ` · ${session.model}` : ""}</span>
                </button>
              )
            })}
          </div>
        </section>
        <p className="instrumentation mt-auto pt-10">j/k navigate · ⌘K palette</p>
      </aside>
    </>
  )
}
