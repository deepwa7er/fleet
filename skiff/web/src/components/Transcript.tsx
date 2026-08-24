import { useState } from "react"

import type { LiveState, Message, Part, Role } from "../types"
import { Blocks } from "./Blocks"

/**
 * One session's conversation.
 *
 * DW-001 rule 1: the gap between messages is the divider — there is no border
 * anywhere. Rule 5: the role and the tool lines are instrumentation, the
 * message body is prose.
 */
export function Transcript({
  messages,
  live,
  harness,
}: {
  messages: Message[]
  live: LiveState
  harness: string
}) {
  const empty =
    messages.length === 0 && live.pending === null && live.pendingPrompt === null
  if (empty) {
    return <p className="text-muted">Nothing in this session yet.</p>
  }
  return (
    <div className="flex flex-col gap-8">
      {messages.map((message) => (
        <MessageView key={message.id} message={message} harness={harness} />
      ))}

      {/* A prompt that has been sent but has not reached the transcript. The
          server owns this, so every pane on the session shows it. */}
      {live.pendingPrompt && (
        <section className="flex flex-col gap-2 opacity-60">
          <p className="instrumentation">you · sending</p>
          <p className="whitespace-pre-wrap">{live.pendingPrompt.text}</p>
        </section>
      )}

      {/* The in-flight reply. Its key is the run id, which is stable from the
          moment the reply opens until it settles — so nothing here remounts
          when the message finishes, and the reasoning disclosure a reader
          opened mid-stream stays open. Card #110 was the absence of exactly
          this property. */}
      {live.pending && (
        <MessageView message={live.pending} harness={harness} streaming />
      )}

      {live.working && live.pending === null && (
        <p className="instrumentation">working…</p>
      )}
    </div>
  )
}

function MessageView({
  message,
  harness,
  streaming = false,
}: {
  message: Message
  harness: string
  streaming?: boolean
}) {
  return (
    <section className="flex flex-col gap-2">
      <p className="instrumentation">
        {label(message.role, harness, message.agent)}
        {streaming && " · streaming"}
      </p>
      {message.parts.map((part, i) => (
        // The index is stable within a message: parts are appended in order
        // and never reordered, and the message id is stable for the message's
        // whole life — including while it streams, which is what lets a
        // reasoning disclosure survive a reply settling.
        <PartView
          key={i}
          part={part}
          messageId={message.id}
          partIndex={i}
          streaming={streaming}
        />
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
  streaming,
}: {
  part: Part
  messageId: string
  partIndex: number
  streaming: boolean
}) {
  switch (part.kind) {
    case "text":
      return (
        <div className="flex flex-col gap-3">
          <Blocks blocks={part.blocks} />
        </div>
      )
    case "reasoning":
      return (
        <Reasoning
          part={part}
          id={`${messageId}:${partIndex}`}
          initiallyOpen={streaming}
        />
      )
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
function Reasoning({
  part,
  id,
  initiallyOpen,
}: {
  part: Extract<Part, { kind: "reasoning" }>
  id: string
  initiallyOpen: boolean
}) {
  // Open while it streams, so the thinking is readable as it arrives — and it
  // stays however the reader left it when the reply settles, because the
  // component does not remount.
  const [open, setOpen] = useState(initiallyOpen)
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
