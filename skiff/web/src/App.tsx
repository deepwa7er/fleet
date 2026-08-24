import { useCallback, useEffect, useRef, useState } from "react"

import { ChangeReview } from "./components/ChangeReview"
import { CommandPalette } from "./components/CommandPalette"
import { DeskRail } from "./components/DeskRail"
import { Session } from "./components/Session"
import { useConnection, useView } from "./lib/useView"
import { readWorkspace, workspaceHref, type Workspace } from "./lib/workspace"

type Pane = "session" | "change"

function useWorkspace(): [Workspace, (workspace: Workspace) => void] {
  const [workspace, setWorkspace] = useState(readWorkspace)
  useEffect(() => {
    const onPop = () => setWorkspace(readWorkspace())
    addEventListener("popstate", onPop)
    return () => removeEventListener("popstate", onPop)
  }, [])
  const navigate = useCallback((next: Workspace) => {
    history.pushState(null, "", workspaceHref(next))
    setWorkspace(next)
  }, [])
  return [workspace, navigate]
}

export function App() {
  const connection = useConnection()
  const sessionsView = useView({ kind: "sessions" } as const)
  const changesView = useView({ kind: "changes" } as const)
  const [workspace, navigate] = useWorkspace()
  const [railOpen, setRailOpen] = useState(false)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [activePane, setActivePane] = useState<Pane>(workspace.change ? "change" : "session")
  const sessionPane = useRef<HTMLElement>(null)
  const changePane = useRef<HTMLElement>(null)

  const sessions = sessionsView.status === "ready" ? sessionsView.data.sessions : []
  const sources = sessionsView.status === "ready" ? sessionsView.data.sources : []
  const changes = changesView.status === "ready" ? changesView.data.changes : []

  const focusPane = useCallback((pane: Pane) => {
    const target = pane === "session" ? sessionPane.current : changePane.current
    if (!target) return
    setActivePane(pane)
    target.focus()
  }, [])
  const openSession = useCallback((id: string) => {
    navigate({ ...workspace, session: id })
    setActivePane("session")
  }, [navigate, workspace])
  const openChange = useCallback((repo: string, card: number) => {
    navigate({ ...workspace, change: { repo, card, round: null } })
    setActivePane("change")
  }, [navigate, workspace])

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) return
      if (event.key.toLocaleLowerCase() === "k") {
        event.preventDefault()
        setPaletteOpen((open) => !open)
      } else if (event.key === "1" && workspace.session) {
        event.preventDefault()
        focusPane("session")
      } else if (event.key === "2" && workspace.change) {
        event.preventDefault()
        focusPane("change")
      }
    }
    addEventListener("keydown", onKey)
    return () => removeEventListener("keydown", onKey)
  }, [focusPane, workspace.change, workspace.session])

  useEffect(() => {
    if (activePane === "session" && !workspace.session && workspace.change) setActivePane("change")
    if (activePane === "change" && !workspace.change && workspace.session) setActivePane("session")
  }, [activePane, workspace.change, workspace.session])

  return (
    <div className="desk-shell">
      <DeskRail
        changes={changes}
        sessions={sessions}
        sources={sources}
        open={railOpen}
        onClose={() => setRailOpen(false)}
        onOpenChange={openChange}
        onOpenSession={openSession}
      />
      <main className="desk-main">
        <header className="desk-toolbar">
          <button className="text-accent lg:hidden" onClick={() => setRailOpen(true)}>Desk</button>
          <span className="instrumentation">{connection}</span>
          {workspace.session && workspace.change ? (
            <nav className="flex gap-4 lg:hidden" aria-label="Open panes">
              <button className={activePane === "session" ? "text-accent" : "text-muted"} onClick={() => focusPane("session")}>Session</button>
              <button className={activePane === "change" ? "text-accent" : "text-muted"} onClick={() => focusPane("change")}>Change</button>
            </nav>
          ) : null}
          <button className="instrumentation ml-auto text-accent" onClick={() => setPaletteOpen(true)}>Open · ⌘K</button>
        </header>
        {sessionsView.status === "error" ? <p className="text-danger">{sessionsView.error}</p> : null}
        {changesView.status === "error" ? <p className="text-danger">{changesView.error}</p> : null}
        {!workspace.session && !workspace.change ? (
          <section className="desk-empty">
            <p className="instrumentation">Agent desk</p>
            <h1 className="font-heading text-5xl tracking-tight">Pick up where the work needs you.</h1>
            <p className="max-w-xl text-muted">Open a live session or a change waiting for review from the desk. They can share the workspace without losing either context.</p>
            <button className="physical-key self-start bg-accent px-5 py-3 text-accent-contrast" onClick={() => setPaletteOpen(true)}>Open something</button>
          </section>
        ) : (
          <div className="desk-panes" data-count={workspace.session && workspace.change ? "two" : "one"}>
            {workspace.session ? (
              <section
                ref={sessionPane}
                className="desk-pane"
                data-active={activePane === "session"}
                aria-label="Session pane"
                tabIndex={-1}
                onPointerDown={() => setActivePane("session")}
              >
                <Session
                  key={workspace.session}
                  id={workspace.session}
                  onClose={() => navigate({ ...workspace, session: null })}
                  onOpenChange={openChange}
                />
              </section>
            ) : null}
            {workspace.change ? (
              <section
                ref={changePane}
                className="desk-pane desk-pane-change"
                data-active={activePane === "change"}
                aria-label="Change pane"
                tabIndex={-1}
                onPointerDown={() => setActivePane("change")}
              >
                <ChangeReview
                  key={`${workspace.change.repo}:${workspace.change.card}`}
                  {...workspace.change}
                  onClose={() => navigate({ ...workspace, change: null })}
                  onRound={(round) => navigate({ ...workspace, change: { ...workspace.change!, round } })}
                />
              </section>
            ) : null}
          </div>
        )}
      </main>
      <CommandPalette
        open={paletteOpen}
        changes={changes}
        sessions={sessions}
        onClose={() => setPaletteOpen(false)}
        onOpenChange={openChange}
        onOpenSession={openSession}
      />
    </div>
  )
}
