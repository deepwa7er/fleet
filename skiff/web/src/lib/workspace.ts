export type ChangePane = { repo: string; card: number; round: number | null }

export type Workspace = {
  session: string | null
  change: ChangePane | null
}

export function readWorkspace(pathname = location.pathname, search = location.search): Workspace {
  const both = pathname.match(/^\/s\/([^/]+)\/c\/([^/]+)\/(\d+)$/)
  if (both?.[1] && both[2] && both[3]) {
    return {
      session: decodeURIComponent(both[1]),
      change: {
        repo: decodeURIComponent(both[2]),
        card: Number(both[3]),
        round: readRound(search),
      },
    }
  }
  const session = pathname.match(/^\/s\/(.+)$/)
  if (session?.[1]) return { session: decodeURIComponent(session[1]), change: null }
  const change = pathname.match(/^\/c\/([^/]+)\/(\d+)$/)
  if (change?.[1] && change[2]) {
    return {
      session: null,
      change: { repo: decodeURIComponent(change[1]), card: Number(change[2]), round: readRound(search) },
    }
  }
  return { session: null, change: null }
}

export function workspaceHref(workspace: Workspace): string {
  const session = workspace.session ? `/s/${encodeURIComponent(workspace.session)}` : ""
  const change = workspace.change
    ? `/c/${encodeURIComponent(workspace.change.repo)}/${workspace.change.card}`
    : ""
  const path = `${session}${change}` || "/"
  return workspace.change?.round ? `${path}?round=${workspace.change.round}` : path
}

function readRound(search: string): number | null {
  const value = new URLSearchParams(search).get("round")
  const round = value === null ? null : Number(value)
  return Number.isInteger(round) && (round ?? 0) > 0 ? round : null
}
