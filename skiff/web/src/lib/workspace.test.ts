import { describe, expect, it } from "vitest"

import { readWorkspace, workspaceHref, type Workspace } from "./workspace"

describe("workspace URLs", () => {
  const cases: Workspace[] = [
    { session: null, change: null },
    { session: "pi:abc/def", change: null },
    { session: null, change: { repo: "deepwa7er/fleet", card: 124, round: null } },
    { session: "pi:abc", change: { repo: "fleet", card: 124, round: 3 } },
  ]

  it.each(cases)("round-trips $session / $change", (workspace) => {
    const url = new URL(workspaceHref(workspace), "https://skiff.test")
    expect(readWorkspace(url.pathname, url.search)).toEqual(workspace)
  })

  it("rejects invalid rounds", () => {
    expect(readWorkspace("/c/fleet/124", "?round=zero").change?.round).toBeNull()
  })

  it("treats malformed percent escapes as an empty workspace", () => {
    expect(readWorkspace("/s/pi:%zz")).toEqual({ session: null, change: null })
    expect(readWorkspace("/c/%zz/124")).toEqual({ session: null, change: null })
  })

  it("rejects card numbers that are not positive safe integers", () => {
    expect(readWorkspace("/c/fleet/0")).toEqual({ session: null, change: null })
    expect(readWorkspace("/c/fleet/9007199254740992")).toEqual({ session: null, change: null })
  })
})
