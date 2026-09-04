import { cleanup, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import { CommandPalette } from "./CommandPalette"

afterEach(cleanup)

describe("CommandPalette", () => {
  it("filters all work and opens the selected result", async () => {
    const user = userEvent.setup()
    const openChange = vi.fn()
    const close = vi.fn()
    render(
      <CommandPalette
        open
        changes={[{ repo: "fleet", card: 124, state: "in_review", rounds: 2, title: "Rust Skiff", updatedAt: "now" }]}
        sessions={[{
          id: "pi:abc",
          harness: "pi",
          capabilities: { rename: true, orchestrator: true, model: true },
          title: "Different task",
          directory: null,
          createdMs: 1,
          updatedMs: 2,
          model: null,
          orchestratorActive: false,
        }]}
        onClose={close}
        onOpenChange={openChange}
        onOpenSession={vi.fn()}
        onNewChat={vi.fn()}
      />,
    )

    await user.type(screen.getByLabelText("Open"), "skiff{Enter}")
    expect(openChange).toHaveBeenCalledWith("fleet", 124)
    expect(close).toHaveBeenCalledOnce()
  })

  it("offers a new chat as its first row", async () => {
    const user = userEvent.setup()
    const newChat = vi.fn()
    const close = vi.fn()
    render(
      <CommandPalette
        open
        changes={[]}
        sessions={[]}
        onClose={close}
        onOpenChange={vi.fn()}
        onOpenSession={vi.fn()}
        onNewChat={newChat}
      />,
    )

    await user.click(screen.getByRole("button", { name: /New chat/ }))
    expect(newChat).toHaveBeenCalledOnce()
    expect(close).toHaveBeenCalledOnce()
  })
})
