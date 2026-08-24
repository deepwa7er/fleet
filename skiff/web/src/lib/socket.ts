import type { ClientFrame, Command, ServerFrame, ViewData, ViewSpec } from "../types"

/**
 * The client's whole data layer (DW-004 §10).
 *
 * Every read is a subscription over one socket; there is no request/response
 * path, so cold load and live update are the same mechanism and cannot
 * disagree. That is the property this file exists to preserve — resist adding
 * a `fetch` beside it.
 *
 * On reconnect every live subscription is re-registered under a **new** wire
 * id and takes a fresh snapshot. Frames addressed to a retired id are dropped
 * without any sequence reasoning, and there is no replay buffer: the snapshot
 * is the convergence guarantee.
 */

/** The data a given view spec yields, narrowed by its `kind`. */
export type DataFor<S extends ViewSpec> = Extract<ViewData, { kind: S["kind"] }>

export type ViewState<S extends ViewSpec> =
  | { status: "loading" }
  | { status: "ready"; data: DataFor<S> }
  | { status: "error"; error: string }

/** A session view's data, with the live half kept current by `live` frames. */
type SessionData = Extract<ViewData, { kind: "session" }>

export type ConnectionState = "connecting" | "open" | "offline"

/** Backoff between reconnect attempts: quick at first, then out of the way. */
const RETRY_BASE_MS = 250
const RETRY_MAX_MS = 5_000

type Slot = {
  spec: ViewSpec
  notify: (state: ViewState<ViewSpec>) => void
  /** The wire id this slot currently holds, or null while disconnected. */
  sub: number | null
  /**
   * The last data delivered, so a `live` frame — which carries only the live
   * half — has a transcript to merge into.
   */
  last: ViewData | null
}

function socketUrl(): string {
  const protocol = location.protocol === "https:" ? "wss:" : "ws:"
  return `${protocol}//${location.host}/ws`
}

export class Client {
  #socket: WebSocket | null = null
  #slots = new Map<number, Slot>()
  #bySub = new Map<number, number>()
  #nextSlot = 1
  #nextSub = 1
  #nextReq = 1
  #commands = new Map<number, { resolve: () => void; reject: (e: Error) => void }>()
  #retries = 0
  #retryTimer: ReturnType<typeof setTimeout> | null = null
  #connection: ConnectionState = "connecting"
  #connectionListeners = new Set<(state: ConnectionState) => void>()

  constructor() {
    this.#connect()
  }

  /** Watch the connection itself, for the status readout. */
  onConnection(listener: (state: ConnectionState) => void): () => void {
    this.#connectionListeners.add(listener)
    listener(this.#connection)
    return () => {
      this.#connectionListeners.delete(listener)
    }
  }

  /**
   * Open a subscription for as long as the returned function is uncalled.
   * `notify` fires immediately with `loading`, then on every snapshot.
   */
  subscribe<S extends ViewSpec>(spec: S, notify: (state: ViewState<S>) => void): () => void {
    const slotId = this.#nextSlot++
    const slot: Slot = { spec, notify: notify as Slot["notify"], sub: null, last: null }
    this.#slots.set(slotId, slot)
    notify({ status: "loading" })
    this.#register(slotId, slot)

    return () => {
      this.#slots.delete(slotId)
      if (slot.sub === null) return
      this.#bySub.delete(slot.sub)
      this.#send({ t: "unsubscribe", sub: slot.sub })
      slot.sub = null
    }
  }

  /**
   * Run a command, resolving when the server acknowledges it.
   *
   * Rejects if the socket is down rather than queueing: a prompt that lands
   * minutes later, after the user gave up and retyped it, is worse than one
   * that visibly failed.
   */
  command(cmd: Command): Promise<void> {
    return new Promise((resolve, reject) => {
      if (this.#socket?.readyState !== WebSocket.OPEN) {
        reject(new Error("not connected"))
        return
      }
      const req = this.#nextReq++
      this.#commands.set(req, { resolve, reject })
      this.#send({ t: "command", req, cmd })
    })
  }

  #setConnection(state: ConnectionState) {
    if (this.#connection === state) return
    this.#connection = state
    for (const listener of this.#connectionListeners) listener(state)
  }

  #connect() {
    this.#setConnection(this.#retries === 0 ? "connecting" : "offline")
    const socket = new WebSocket(socketUrl())
    this.#socket = socket

    socket.onopen = () => {
      this.#retries = 0
      this.#setConnection("open")
      // Re-register every live slot under a fresh id; anything still in
      // flight for the old ids is now unaddressable and will be dropped.
      this.#bySub.clear()
      for (const [slotId, slot] of this.#slots) {
        slot.sub = null
        // The reconnect takes a fresh snapshot, so anything merged into the
        // old one is worthless.
        slot.last = null
        slot.notify({ status: "loading" })
        this.#register(slotId, slot)
      }
    }

    socket.onmessage = (event) => {
      if (typeof event.data !== "string") return
      let frame: ServerFrame
      try {
        frame = JSON.parse(event.data) as ServerFrame
      } catch {
        return
      }
      this.#receive(frame)
    }

    socket.onclose = () => {
      if (this.#socket !== socket) return
      this.#socket = null
      for (const slot of this.#slots.values()) slot.sub = null
      this.#bySub.clear()
      // An in-flight command can never be answered now.
      for (const { reject } of this.#commands.values()) reject(new Error("disconnected"))
      this.#commands.clear()
      this.#setConnection("offline")
      this.#scheduleRetry()
    }

    // A failed connection also closes, so onclose owns the retry.
    socket.onerror = () => socket.close()
  }

  #scheduleRetry() {
    if (this.#retryTimer !== null) return
    const delay = Math.min(RETRY_BASE_MS * 2 ** this.#retries, RETRY_MAX_MS)
    this.#retries++
    this.#retryTimer = setTimeout(() => {
      this.#retryTimer = null
      this.#connect()
    }, delay)
  }

  #register(slotId: number, slot: Slot) {
    if (this.#socket?.readyState !== WebSocket.OPEN) return
    const sub = this.#nextSub++
    slot.sub = sub
    this.#bySub.set(sub, slotId)
    this.#send({ t: "subscribe", sub, view: slot.spec })
  }

  #receive(frame: ServerFrame) {
    switch (frame.t) {
      case "hello":
        return
      case "snapshot": {
        const slot = this.#slotFor(frame.sub)
        if (!slot) return
        slot.last = frame.data
        slot.notify({ status: "ready", data: frame.data as DataFor<ViewSpec> })
        return
      }
      case "live": {
        // The cheap, frequent frame: it replaces only the live half, leaving
        // the transcript exactly as it was. A `live` frame that arrives before
        // any snapshot has nothing to merge into and is dropped — the snapshot
        // that follows carries the same state.
        const slot = this.#slotFor(frame.sub)
        if (!slot?.last || slot.last.kind !== "session") return
        const next: SessionData = { ...slot.last, live: frame.live }
        slot.last = next
        slot.notify({ status: "ready", data: next as DataFor<ViewSpec> })
        return
      }
      case "ack": {
        this.#commands.get(frame.req)?.resolve()
        this.#commands.delete(frame.req)
        return
      }
      case "error": {
        if (frame.req !== null) {
          this.#commands.get(frame.req)?.reject(new Error(frame.error))
          this.#commands.delete(frame.req)
          return
        }
        // A `sub`-less, `req`-less error is a protocol-level complaint about a
        // frame this client sent; it belongs in the console, not in a pane.
        if (frame.sub === null) {
          console.error("skiff: protocol error", frame.error)
          return
        }
        this.#slotFor(frame.sub)?.notify({ status: "error", error: frame.error })
        return
      }
    }
  }

  /** The slot a wire id belongs to, or undefined if that id is retired. */
  #slotFor(sub: number): Slot | undefined {
    const slotId = this.#bySub.get(sub)
    return slotId === undefined ? undefined : this.#slots.get(slotId)
  }

  #send(frame: ClientFrame) {
    if (this.#socket?.readyState !== WebSocket.OPEN) return
    this.#socket.send(JSON.stringify(frame))
  }
}

export const client = new Client()
