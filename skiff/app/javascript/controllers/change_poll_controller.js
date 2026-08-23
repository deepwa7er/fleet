import { Controller } from "@hotwired/stimulus"

// DW-002 §6: the review page follows the change while something else is
// moving it — an agent producing the next round, or a landing in flight.
// The page mounts this only in those states. It polls the change's status
// readout (state + round count) and, on any movement, reloads the page
// whole via Turbo — the view renders ops, it never diffs (skiff's rule);
// the reload IS the render. A failed poll is silence, not an error state:
// the next tick tries again, and the page it would have replaced is still
// correct about the past.
export default class extends Controller {
  static values = {
    url: String,
    state: String,
    rounds: Number,
    deployPending: Boolean,
    interval: { type: Number, default: 5000 },
  }

  connect() {
    this.timer = setInterval(() => this.check(), this.intervalValue)
  }

  disconnect() {
    clearInterval(this.timer)
  }

  async check() {
    let status
    try {
      const response = await fetch(this.urlValue, { headers: { Accept: "application/json" } })
      if (!response.ok) return
      status = await response.json()
    } catch {
      return
    }
    if (
      status.state !== this.stateValue ||
      status.rounds !== this.roundsValue ||
      status.deployPending !== this.deployPendingValue
    ) {
      Turbo.visit(window.location.href, { action: "replace" })
    }
  }
}
