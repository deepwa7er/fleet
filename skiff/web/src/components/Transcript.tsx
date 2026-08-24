import { useState } from "react"

import type { Message, Part, Role } from "../types"
import { Blocks } from "./Blocks"

/**
 * One session's conversation.
 *
 * DW-001 rule 1: the gap between messages is the divider — there is no border
 * anywhere. Rule 5: the role and the tool lines are instrumentation, the
 * message body is prose.
 */
export function Transcript({ messages, harness }: { messages: Message[]; harness: string }) {
  if (messages.length === 0) {
    return <p className="text-muted">Nothing in this session yet.</p>
  }
  return (
    <div className="flex flex-col gap-8">
      {messages.map((message) => (
        <MessageView key={message.id} message={message} harness={harness} />
      ))}
    </div>
  )
}

function MessageView({ message, harness }: { message: Message; harness: string }) {
  return (
    <section className="flex flex-col gap-2">
      <p className="instrumentation">{label(message.role, harness, message.agent)}</p>
      {message.parts.map((part, i) => (
        // The index is stable within a message: parts are appended in order
        // and never reordered, and the message id is stable for the message's
        // whole life — including while it streams, which is what lets a
        // reasoning disclosure survive a reply settling.
        <PartView key={i} part={part} messageId={message.id} partIndex={i} />
      ))}
    </section>
  )
}

function label(role: Role, harness: string, agent: string | null): string {
  if (role === "user") return "you"
  if (role === "tool") return "tool"
  return [harness, agent].filter(Boolean).join(" · ")
}

function PartView({
  part,
  messageId,
  partIndex,
}: {
  part: Part
  messageId: string
  partIndex: number
}) {
  switch (part.kind) {
    case "text":
      return (
        <div className="flex flex-col gap-3">
          <Blocks blocks={part.blocks} />
        </div>
      )
    case "reasoning":
      return <Reasoning part={part} id={`${messageId}:${partIndex}`} />
    case "tool":
      return <Tool part={part} />
    case "file":
      return <p className="instrumentation">file · {part.filename}</p>
  }
}

/**
 * Thinking, collapsed by default.
 *
 * The open/closed state is per-viewer and per-part, so it is React's to own —
 * the server has no opinion about what this reader has expanded. It survives
 * a reply settling because the key is the message id, which does not change
 * when a live message finishes (DW-004 §7, and card #110, which was this bug).
 */
function Reasoning({ part, id }: { part: Extract<Part, { kind: "reasoning" }>; id: string }) {
  const [open, setOpen] = useState(false)
  return (
    <details
      key={id}
      open={open}
      onToggle={(e) => setOpen((e.currentTarget as HTMLDetailsElement).open)}
    >
      <summary className="instrumentation cursor-pointer">reasoning</summary>
      <div className="mt-2 flex flex-col gap-3 text-muted">
        <Blocks blocks={part.blocks} />
      </div>
    </details>
  )
}

/**
 * DW-001 §2 rule 3: `--danger` marks a fact, not a mood. A tool line turns bad
 * only when the call did not complete; running and completed stay muted.
 */
function Tool({ part }: { part: Extract<Part, { kind: "tool" }> }) {
  const [open, setOpen] = useState(false)
  const failed = part.status === "error"
  const line = [part.name, part.status].filter(Boolean).join(" · ")
  const output = part.output?.trim()

  return (
    <div className="flex flex-col gap-1">
      {output ? (
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className={`instrumentation cursor-pointer text-left ${failed ? "text-danger" : ""}`}
        >
          {line} {open ? "▾" : "▸"}
        </button>
      ) : (
        <p className={`instrumentation ${failed ? "text-danger" : ""}`}>{line}</p>
      )}
      {open && output && (
        <pre className="overflow-x-auto bg-fill px-3 py-2 font-mono text-sm">{output}</pre>
      )}
    </div>
  )
}
