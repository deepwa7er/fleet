import { useEffect, useState } from "react"

import { ChangeList } from "./components/ChangeList"
import { ChangeReview } from "./components/ChangeReview"
import { Session } from "./components/Session"
import { SessionList, SourceErrors } from "./components/SessionList"
import { useConnection, useView } from "./lib/useView"

type Route =
  | { kind: "sessions" }
  | { kind: "session"; id: string }
  | { kind: "changes" }
  | { kind: "change"; repo: string; card: number; round: number | null }

function readRoute(): Route {
  const session = location.pathname.match(/^\/s\/(.+)$/)
  if (session?.[1]) return { kind: "session", id: decodeURIComponent(session[1]) }
  if (location.pathname === "/changes") return { kind: "changes" }
  const change = location.pathname.match(/^\/c\/([^/]+)\/(\d+)$/)
  if (change?.[1] && change[2]) {
    const rawRound = new URLSearchParams(location.search).get("round")
    const parsed = rawRound === null ? null : Number(rawRound)
    return {
      kind: "change",
      repo: decodeURIComponent(change[1]),
      card: Number(change[2]),
      round: Number.isInteger(parsed) && (parsed ?? 0) > 0 ? parsed : null,
    }
  }
  return { kind: "sessions" }
}

function href(route: Route): string {
  switch (route.kind) {
    case "sessions": return "/"
    case "session": return `/s/${encodeURIComponent(route.id)}`
    case "changes": return "/changes"
    case "change": {
      const base = `/c/${encodeURIComponent(route.repo)}/${route.card}`
      return route.round === null ? base : `${base}?round=${route.round}`
    }
  }
}

function useRoute(): [Route, (route: Route) => void] {
  const [route, setRoute] = useState(readRoute)
  useEffect(() => {
    const onPop = () => setRoute(readRoute())
    addEventListener("popstate", onPop)
    return () => removeEventListener("popstate", onPop)
  }, [])
  const navigate = (next: Route) => {
    history.pushState(null, "", href(next))
    setRoute(next)
  }
  return [route, navigate]
}

export function App() {
  const connection = useConnection()
  const [route, navigate] = useRoute()
  return (
    <main className={route.kind === "change" ? "mx-auto flex max-w-[96rem] flex-col gap-8 p-8" : "mx-auto flex max-w-3xl flex-col gap-8 p-8"}>
      <header className="flex items-baseline justify-between gap-4">
        <button className="font-heading text-3xl tracking-tight" onClick={() => navigate({ kind: "sessions" })}>skiff</button>
        <nav className="flex items-baseline gap-6">
          <button className="text-sm text-accent" onClick={() => navigate({ kind: "sessions" })}>Sessions</button>
          <button className="text-sm text-accent" onClick={() => navigate({ kind: "changes" })}>Changes</button>
          <span className="instrumentation">{connection}</span>
        </nav>
      </header>

      {route.kind === "sessions" ? (
        <Sessions onOpen={(id) => navigate({ kind: "session", id })} />
      ) : route.kind === "session" ? (
        <Session id={route.id} onBack={() => navigate({ kind: "sessions" })} />
      ) : route.kind === "changes" ? (
        <Changes onOpen={(repo, card) => navigate({ kind: "change", repo, card, round: null })} />
      ) : (
        <ChangeReview
          {...route}
          onBack={() => navigate({ kind: "changes" })}
          onRound={(round) => navigate({ ...route, round })}
        />
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

function Changes({ onOpen }: { onOpen: (repo: string, card: number) => void }) {
  const changes = useView({ kind: "changes" } as const)
  if (changes.status === "loading") return <p className="text-muted">Loading…</p>
  if (changes.status === "error") return <p className="text-danger">{changes.error}</p>
  return <ChangeList changes={changes.data.changes} onOpen={onOpen} />
}
