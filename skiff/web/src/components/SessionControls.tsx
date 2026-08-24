import { useState, type FormEvent } from "react"

import type { Command, SessionView } from "../types"

type Props = {
  view: SessionView
  command: (command: Command) => Promise<void>
}

export function SessionControls({ view, command }: Props) {
  const session = view.session
  const [name, setName] = useState(session?.title ?? "")
  const [model, setModel] = useState(() => {
    const current = view.models.options.find((option) => option.id === session?.model)
    return current ? encodeModel(current.provider, current.id) : ""
  })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  if (!session) return null
  const capabilities = session.capabilities
  if (!capabilities.rename && !capabilities.model && !capabilities.orchestrator) return null
  const sessionId = session.id

  async function execute(next: Command) {
    setBusy(true)
    setError(null)
    try {
      await command(next)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  function rename(event: FormEvent) {
    event.preventDefault()
    const trimmed = name.trim()
    if (trimmed) void execute({ kind: "rename", session: sessionId, name: trimmed })
  }

  function switchModel(event: FormEvent) {
    event.preventDefault()
    if (!model) return
    const [provider, modelId] = decodeModel(model)
    void execute({ kind: "setModel", session: sessionId, provider, modelId })
  }

  return (
    <section aria-label="Session controls" className="flex flex-wrap items-end gap-5">
      {capabilities.rename && (
        <form onSubmit={rename} className="flex items-end gap-2">
          <label className="flex flex-col gap-1">
            <span className="instrumentation">Session name</span>
            <input
              value={name}
              onChange={(event) => setName(event.target.value)}
              className="input-surface min-w-48 px-3 py-2"
            />
          </label>
          <button
            type="submit"
            disabled={busy || name.trim() === "" || name.trim() === session.title}
            className="physical-key bg-fill px-3 py-2 text-accent"
          >
            Rename
          </button>
        </form>
      )}

      {capabilities.model && (
        <form onSubmit={switchModel} className="flex items-end gap-2">
          <label className="flex flex-col gap-1">
            <span className="instrumentation">Model</span>
            <select
              value={model}
              onChange={(event) => setModel(event.target.value)}
              disabled={busy || view.models.options.length === 0}
              className="input-surface max-w-72 px-3 py-2"
            >
              <option value="">Choose a model…</option>
              {view.models.options.map((option) => (
                <option key={encodeModel(option.provider, option.id)} value={encodeModel(option.provider, option.id)}>
                  {option.provider} · {option.id}
                </option>
              ))}
            </select>
          </label>
          <button
            type="submit"
            disabled={busy || model === ""}
            className="physical-key bg-fill px-3 py-2 text-accent"
          >
            Switch
          </button>
        </form>
      )}

      {capabilities.orchestrator && (
        <button
          type="button"
          disabled={busy}
          onClick={() =>
            void execute({
              kind: "setOrchestrator",
              session: session.id,
              active: !session.orchestratorActive,
            })
          }
          className="physical-key bg-fill px-3 py-2 text-accent"
        >
          Turn orchestrator {session.orchestratorActive ? "off" : "on"}
        </button>
      )}

      {view.models.error && <p className="basis-full text-sm text-danger">{view.models.error}</p>}
      {error && <p className="basis-full text-sm text-danger">{error}</p>}
    </section>
  )
}

function encodeModel(provider: string, id: string): string {
  return JSON.stringify([provider, id])
}

function decodeModel(value: string): [string, string] {
  const decoded: unknown = JSON.parse(value)
  if (
    !Array.isArray(decoded) ||
    decoded.length !== 2 ||
    typeof decoded[0] !== "string" ||
    typeof decoded[1] !== "string"
  ) {
    throw new Error("invalid model selection")
  }
  return [decoded[0], decoded[1]]
}
