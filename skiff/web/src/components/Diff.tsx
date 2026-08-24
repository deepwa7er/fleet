import { useState } from "react"
import clsx from "clsx"

import type { Annotation, AnnotationSide, Diff as StructuredDiff, DiffLine } from "../types"

type Anchor = { path: string; line: number; side: AnnotationSide }

export function Diff({
  diff,
  annotations,
  canAnnotate,
  onAnnotate,
}: {
  diff: StructuredDiff
  annotations: Annotation[]
  canAnnotate: boolean
  onAnnotate: (anchor: Anchor, text: string) => Promise<void>
}) {
  if (diff.files.length === 0) return <p className="text-muted">No textual changes.</p>
  return (
    <div className="flex flex-col gap-10">
      {diff.files.map((file, fileIndex) => {
        const path = file.newPath ?? file.oldPath ?? "binary"
        return (
          <section key={`${path}-${fileIndex}`}>
            <h3 className="sticky top-0 z-10 bg-paper py-2 font-mono text-sm font-semibold">{path}</h3>
            {file.binary ? (
              <p className="instrumentation py-4">Binary file</p>
            ) : (
              <div className="diff-scroll">
                {file.hunks.map((hunk, hunkIndex) => (
                  <div className="mb-5 min-w-max" key={`${hunk.oldStart}-${hunk.newStart}-${hunkIndex}`}>
                    <p className="diff-hunk">
                      −{hunk.oldStart},{hunk.oldCount} +{hunk.newStart},{hunk.newCount}
                      {hunk.heading ? ` · ${hunk.heading}` : ""}
                    </p>
                    {hunk.lines.map((line, lineIndex) => {
                      const anchor = anchorFor(path, line)
                      const notes = anchor
                        ? annotations.filter(
                            (note) =>
                              note.path === anchor.path && note.line === anchor.line && note.side === anchor.side,
                          )
                        : []
                      return (
                        <DiffRow
                          key={`${line.oldLine}-${line.newLine}-${lineIndex}`}
                          line={line}
                          anchor={anchor}
                          notes={notes}
                          canAnnotate={canAnnotate}
                          onAnnotate={onAnnotate}
                        />
                      )
                    })}
                  </div>
                ))}
              </div>
            )}
          </section>
        )
      })}
    </div>
  )
}

function DiffRow({
  line,
  anchor,
  notes,
  canAnnotate,
  onAnnotate,
}: {
  line: DiffLine
  anchor: Anchor | null
  notes: Annotation[]
  canAnnotate: boolean
  onAnnotate: (anchor: Anchor, text: string) => Promise<void>
}) {
  const [editing, setEditing] = useState(false)
  const [text, setText] = useState("")
  const [error, setError] = useState<string | null>(null)
  const submit = async () => {
    if (!anchor || text.trim() === "") return
    setError(null)
    try {
      await onAnnotate(anchor, text.trim())
      setText("")
      setEditing(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    }
  }
  return (
    <div>
      <div
        className={clsx("diff-line group", {
          "diff-addition": line.kind === "addition",
          "diff-deletion": line.kind === "deletion",
        })}
      >
        <span className="diff-number">{line.oldLine ?? ""}</span>
        <span className="diff-number">{line.newLine ?? ""}</span>
        <span className="diff-sign">{line.kind === "addition" ? "+" : line.kind === "deletion" ? "−" : " "}</span>
        <code className="pr-5">{line.text || " "}</code>
        {canAnnotate && anchor ? (
          <button
            className="ml-auto px-3 text-accent opacity-0 group-hover:opacity-100 focus:opacity-100"
            aria-label={`Annotate ${anchor.path} line ${anchor.line}`}
            onClick={() => setEditing((open) => !open)}
          >
            + note
          </button>
        ) : null}
      </div>
      {notes.map((note) => (
        <p className="diff-note" key={note.id}>
          {note.text}
        </p>
      ))}
      {editing && anchor ? (
        <div className="my-3 ml-24 flex max-w-2xl gap-3">
          <input
            className="input-surface min-w-0 flex-1 px-3 py-2"
            autoFocus
            value={text}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) void submit()
            }}
          />
          <button className="physical-key bg-accent px-4 text-accent-contrast" onClick={() => void submit()}>
            Add
          </button>
          {error ? <span className="text-danger">{error}</span> : null}
        </div>
      ) : null}
    </div>
  )
}

function anchorFor(path: string, line: DiffLine): Anchor | null {
  if (line.kind === "deletion" && line.oldLine !== null) {
    return { path, line: line.oldLine, side: "old" }
  }
  if (line.newLine !== null) return { path, line: line.newLine, side: "new" }
  return null
}
