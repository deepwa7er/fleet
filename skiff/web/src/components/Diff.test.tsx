import { cleanup, render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { afterEach, describe, expect, it, vi } from "vitest"

import type { Annotation, Diff as StructuredDiff } from "../types"
import { Diff } from "./Diff"

afterEach(cleanup)

const diff: StructuredDiff = {
  files: [
    {
      oldPath: "a.txt",
      newPath: "a.txt",
      binary: false,
      hunks: [
        {
          oldStart: 1,
          oldCount: 1,
          newStart: 1,
          newCount: 2,
          heading: null,
          lines: [
            { kind: "context", oldLine: 1, newLine: 1, text: "base", noNewline: false },
            { kind: "addition", oldLine: null, newLine: 2, text: "feature", noNewline: false },
          ],
        },
      ],
    },
  ],
}

const annotation: Annotation = {
  id: "note-1",
  path: "a.txt",
  line: 2,
  side: "new",
  text: "This explains the added line.",
  createdAt: "now",
}

describe("Diff", () => {
  it("renders an existing annotation at its exact line anchor", () => {
    render(<Diff diff={diff} annotations={[annotation]} canAnnotate={false} onAnnotate={vi.fn()} />)
    expect(screen.getByText("This explains the added line.")).toBeTruthy()
    expect(screen.queryByRole("button", { name: /annotate/i })).toBeNull()
  })

  it("authors a typed new-side annotation from an addition", async () => {
    const user = userEvent.setup()
    const onAnnotate = vi.fn(() => Promise.resolve())
    render(<Diff diff={diff} annotations={[]} canAnnotate onAnnotate={onAnnotate} />)
    await user.click(screen.getByRole("button", { name: "Annotate a.txt line 2" }))
    await user.type(screen.getByRole("textbox"), "Reason for this line")
    await user.click(screen.getByRole("button", { name: "Add" }))
    expect(onAnnotate).toHaveBeenCalledWith(
      { path: "a.txt", line: 2, side: "new" },
      "Reason for this line",
    )
  })
})
