import { useEffect, useState } from "react"

import type { ViewSpec } from "../types"
import { client, type ConnectionState, type ViewState } from "./socket"

/**
 * Subscribe to a live query for as long as the component is mounted.
 *
 * The spec is compared by value, not identity, so callers may pass an object
 * literal — which is the natural way to write `{ kind: "session", id }` and
 * would otherwise resubscribe on every render.
 */
export function useView<S extends ViewSpec>(spec: S): ViewState<S> {
  const [state, setState] = useState<ViewState<S>>({ status: "loading" })
  const key = JSON.stringify(spec)

  useEffect(() => client.subscribe(JSON.parse(key) as S, setState), [key])

  return state
}

/** The socket's own state, for the connection readout. */
export function useConnection(): ConnectionState {
  const [state, setState] = useState<ConnectionState>("connecting")
  useEffect(() => client.onConnection(setState), [])
  return state
}
