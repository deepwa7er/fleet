export type ChangePane = { repo: string; card: number; round: number | null }

export type Workspace = {
  session: string | null
  change: ChangePane | null
}

export function readWorkspace(pathname = location.pathname, search = location.search): Workspace {
  const both = pathname.match(/^\/s\/([^/]+)\/c\/([^/]+)\/(\d+)$/)
  if (both?.[1] && both[2] && both[3]) {
    const session = decodeSegment(both[1])
    const repo = decodeSegment(both[2])
    const card = readCard(both[3])
    if (session === null || repo === null || card === null) return emptyWorkspace()
    return {
      session,
      change: {
        repo,
        card,
        round: readRound(search),
      },
    }
  }
  const session = pathname.match(/^\/s\/(.+)$/)
  if (session?.[1]) {
    const id = decodeSegment(session[1])
    return id === null ? emptyWorkspace() : { session: id, change: null }
  }
  const change = pathname.match(/^\/c\/([^/]+)\/(\d+)$/)
  if (change?.[1] && change[2]) {
    const repo = decodeSegment(change[1])
    const card = readCard(change[2])
    if (repo === null || card === null) return emptyWorkspace()
    return {
      session: null,
      change: { repo, card, round: readRound(search) },
    }
  }
  return emptyWorkspace()
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

function decodeSegment(value: string): string | null {
  try {
    return decodeURIComponent(value)
  } catch {
    return null
  }
}

function readCard(value: string): number | null {
  const card = Number(value)
  return Number.isSafeInteger(card) && card > 0 ? card : null
}

function emptyWorkspace(): Workspace {
  return { session: null, change: null }
}
