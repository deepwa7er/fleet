import { cleanup, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import type { SessionView } from "../types"

import { SessionControls } from "./SessionControls"

const command = vi.fn(() => Promise.resolve())

afterEach(() => {
  cleanup()
  command.mockClear()
})

const view: SessionView = {
  session: {
    id: "pi:abc",
    harness: "pi",
    capabilities: { rename: true, orchestrator: true, model: true },
    title: "Old name",
    directory: "/work",
    createdMs: 1,
    updatedMs: 2,
    model: "old-model",
    orchestratorActive: false,
  },
  messages: [],
  live: { working: false, pending: null, pendingPrompt: null },
  models: {
    options: [
      { provider: "one", id: "old-model" },
      { provider: "two", id: "new-model" },
    ],
    error: null,
  },
}

describe("SessionControls", () => {
  it("sends typed commands for every capability", async () => {
    const user = userEvent.setup()
    render(<SessionControls view={view} command={command} />)

    const name = screen.getByLabelText("Session name")
    await user.clear(name)
    await user.type(name, "New name")
    await user.click(screen.getByRole("button", { name: "Rename" }))
    expect(command).toHaveBeenCalledWith({
      kind: "rename",
      session: "pi:abc",
      name: "New name",
    })

    await user.selectOptions(screen.getByLabelText("Model"), JSON.stringify(["two", "new-model"]))
    await user.click(screen.getByRole("button", { name: "Switch" }))
    expect(command).toHaveBeenCalledWith({
      kind: "setModel",
      session: "pi:abc",
      provider: "two",
      modelId: "new-model",
    })

    await user.click(screen.getByRole("button", { name: "Turn orchestrator on" }))
    expect(command).toHaveBeenCalledWith({
      kind: "setOrchestrator",
      session: "pi:abc",
      active: true,
    })
  })

  it("shows a model enumeration failure without hiding the other controls", () => {
    render(
      <SessionControls
        view={{ ...view, models: { options: [], error: "Pi model discovery failed" } }}
        command={command}
      />,
    )
    expect(screen.getByText("Pi model discovery failed")).toBeTruthy()
    expect(screen.getByLabelText("Session name")).toBeTruthy()
    expect((screen.getByLabelText("Model") as HTMLSelectElement).disabled).toBe(true)
  })

  it("renders nothing when a harness advertises no controls", () => {
    const { container } = render(
      <SessionControls
        view={{
          ...view,
          session: {
            ...view.session!,
            harness: "muse",
            capabilities: { rename: false, orchestrator: false, model: false },
          },
          models: { options: [], error: null },
        }}
        command={command}
      />,
    )
    expect(container.innerHTML).toBe("")
  })
})
