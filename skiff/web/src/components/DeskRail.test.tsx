import { cleanup, fireEvent, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import type { ChangeRef, SessionSummary } from "../types"
import { DeskRail } from "./DeskRail"

const changes: ChangeRef[] = [
  { repo: "fleet", card: 124, state: "in_review", rounds: 2, title: "Rust Skiff", updatedAt: "now" },
]
const sessions: SessionSummary[] = [
  {
    id: "pi:abc",
    harness: "pi",
    capabilities: { rename: true, orchestrator: true, model: true },
    title: "Rewrite",
    directory: "/work",
    createdMs: 1,
    updatedMs: 2,
    model: "claude",
    orchestratorActive: false,
  },
]

afterEach(cleanup)

describe("DeskRail", () => {
  it("navigates its unified work queue with j/k and opens with Enter", async () => {
    const user = userEvent.setup()
    const openSession = vi.fn()
    render(
      <DeskRail
        changes={changes}
        sessions={sessions}
        sources={[]}
        open
        onClose={vi.fn()}
        onOpenChange={vi.fn()}
        onOpenSession={openSession}
        onNewChat={vi.fn()}
      />,
    )

    const change = screen.getByRole("button", { name: /Rust Skiff/ })
    const session = screen.getByRole("button", { name: /Rewrite/ })
    change.focus()
    await user.keyboard("j")
    expect(document.activeElement).toBe(session)
    await user.keyboard("{Enter}")
    expect(openSession).toHaveBeenCalledWith("pi:abc")
    await user.keyboard("k")
    expect(document.activeElement).toBe(change)
  })

  it("starts a new chat from its button", () => {
    const newChat = vi.fn()
    const close = vi.fn()
    render(
      <DeskRail
        changes={[]}
        sessions={[]}
        sources={[]}
        open
        onClose={close}
        onOpenChange={vi.fn()}
        onOpenSession={vi.fn()}
        onNewChat={newChat}
      />,
    )

    // fireEvent: jsdom has no layout, so the open scrim would swallow a
    // hit-tested userEvent click that a real browser delivers to the rail.
    fireEvent.click(screen.getByRole("button", { name: /New chat/ }))
    expect(newChat).toHaveBeenCalledOnce()
    expect(close).toHaveBeenCalledOnce()
  })
})
