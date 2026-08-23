import { Controller } from "@hotwired/stimulus"

// Card #110: a reasoning disclosure must not snap shut when its reply
// settles. The stream re-renders whole messages — turbo-stream replaces on
// every flush and at settlement (sessions#stream) — destroying and
// rebuilding each <details> node, so open state cannot live in the DOM. It
// is remembered here, keyed per message+part, and re-applied to every fresh
// node.
//
// The key is the message's positional index plus the part's positional
// index (data-reasoning-state-key, rendered server-side). The part's own id
// cannot be the key: the live overlay's entry id is "<pending>", and
// settlement swaps in the authoritative entry with its real id, so part ids
// change exactly at the transition this disclosure must survive. The
// positional key is identical before and after settlement — the overlay is
// replaced in place at the same message index, and both the overlay
// assembly and the settled parse map content in index order — so it
// survives every replace path: flush, settlement, tool-result fold,
// snapshot, reconnect.
//
// Rules:
//   - First sight of a key adopts the server's render: streaming renders
//     open (so the thinking is readable live), settled renders closed. That
//     is what keeps history that loaded already-settled starting closed.
//   - Every later render re-applies the remembered state, so settlement
//     does not close a disclosure that streamed open.
//   - A reader's native toggle updates the remembered state — a manual
//     close during streaming survives the next flush, a manual open on
//     history survives a fold.
//   - The browser fires a toggle event when a <details> is inserted open.
//     Those are harmless by construction: their dispatch is queued as a
//     task, so this observer's reconcile microtask — which has already
//     re-applied the remembered state — runs first, and the diff guard
//     below skips a toggle whose result equals the remembered state anyway.
//     Only a genuine reader flip, which always differs from what was
//     remembered, reaches the map.
//
// A single reconcile pass on every mutation (add or remove) covers all
// content sources uniformly, and doubles as the pruning pass: a remembered
// state whose disclosure is gone (a message removed by an abort, or indices
// shifted by a snapshot after one) is dropped rather than risk applying to
// a different message that later lands at its index.
export default class extends Controller {
  connect() {
    this.states = new Map() // key -> open (boolean)
    this.observer = new MutationObserver(() => this.reconcile())
    this.observer.observe(this.element, { childList: true, subtree: true })
    this.handleToggle = this.rememberToggle.bind(this)
    this.element.addEventListener("toggle", this.handleToggle, true)
    this.reconcile()
  }

  disconnect() {
    this.observer.disconnect()
    this.element.removeEventListener("toggle", this.handleToggle, true)
  }

  reconcile() {
    const live = new Set()
    this.element.querySelectorAll("details.reasoning").forEach((details) => {
      const key = details.dataset.reasoningStateKey
      if (key === undefined) return
      live.add(key)
      if (this.states.has(key)) this.applyState(details, this.states.get(key))
      else this.states.set(key, details.open)
    })
    for (const key of this.states.keys()) {
      if (!live.has(key)) this.states.delete(key)
    }
  }

  rememberToggle(event) {
    const details = event.target.closest("details.reasoning")
    if (!details) return
    const key = details.dataset.reasoningStateKey
    if (key === undefined) return
    const open = details.open
    if (this.states.get(key) !== open) this.states.set(key, open)
  }

  applyState(details, open) {
    if (details.open !== open) details.open = open
  }
}
