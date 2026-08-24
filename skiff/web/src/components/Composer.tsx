import { useState } from "react"

import { client } from "../lib/socket"

/**
 * The prompt box.
 *
 * DW-001 rule 2: depth marks interactivity — the field and the keys are the
 * only things on the page with an outline or a shadow.
 *
 * The draft, the focus, and the in-flight flag are all per-viewer state, so
 * they are React's. What the *server* owns is whether the prompt has reached
 * the transcript yet — two panes on one session must agree about that, so it
 * arrives as `live.pendingPrompt` rather than being guessed here.
 */
export function Composer({ session, working }: { session: string; working: boolean }) {
  const [text, setText] = useState("")
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function send() {
    const trimmed = text.trim()
    if (trimmed === "" || sending) return
    setSending(true)
    setError(null)
    // Cleared optimistically: the server echoes the prompt back as
    // `pendingPrompt`, so the message is on screen either way, and a field
    // that empties on Enter is what makes the box feel like a chat.
    setText("")
    try {
      await client.command({
        kind: "send",
        session,
        text: trimmed,
        // Identity for the pending prompt the server sends back.
        clientId: crypto.randomUUID(),
      })
    } catch (err) {
      // Put the text back: it is the user's, and losing it to a dropped
      // socket would be unforgivable.
      setText(trimmed)
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setSending(false)
    }
  }

  async function abort() {
    setError(null)
    try {
      await client.command({ kind: "abort", session })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }

  return (
    <div className="flex flex-col gap-2">
      {error && <p className="text-danger text-sm">{error}</p>}
      <label htmlFor={`prompt-${session}`} className="instrumentation">
        Prompt
      </label>
      <div className="flex items-end gap-2">
        <textarea
          id={`prompt-${session}`}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            // Enter sends; Shift-Enter is a newline. The desktop convention,
            // and the primary client is a desktop browser.
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault()
              void send()
            }
          }}
          rows={2}
          placeholder="Say something…"
          className="input-surface flex-1 resize-y px-3 py-2"
        />
        {working ? (
          <button
            type="button"
            onClick={() => void abort()}
            className="physical-key bg-fill px-3 py-2 text-danger"
          >
            Stop
          </button>
        ) : (
          <button
            type="button"
            onClick={() => void send()}
            disabled={text.trim() === "" || sending}
            className="physical-key bg-accent px-3 py-2 text-accent-contrast"
          >
            Send
          </button>
        )}
      </div>
    </div>
  )
}
