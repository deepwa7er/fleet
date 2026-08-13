import { Controller } from "@hotwired/stimulus"

// The code block's Copy key. DW-001 §6: the only motion is the label swap —
// a state change ("Copy" -> "Copied" -> back), never an animation, and the
// failure state is an honest readout ("Copy failed") a re-tap can clear.
//
// The clipboard API requires a secure context: the phone reaches the app
// over https (breakwater), so navigator.clipboard is the primary path; the
// tailnet-IP http fallback uses execCommand on a scratch textarea. A block
// re-renders as it streams, so this controller connects fresh per block —
// the label is always "Copy" on connect and the confirm timer never outlives
// the controller.
export default class extends Controller {
  static targets = ["source"]

  connect() {
    this.confirmTimer = null
    this.originalLabel = null
  }

  disconnect() {
    if (this.confirmTimer) clearTimeout(this.confirmTimer)
  }

  copy(event) {
    const key = event.currentTarget
    const text = this.sourceTarget.textContent

    const confirm = () => {
      this.originalLabel ??= key.textContent
      key.textContent = "Copied"
      if (this.confirmTimer) clearTimeout(this.confirmTimer)
      this.confirmTimer = setTimeout(() => {
        key.textContent = this.originalLabel
        this.confirmTimer = null
      }, 1600)
    }
    const fail = () => {
      key.textContent = "Copy failed"
    }

    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).then(confirm, fail)
    } else {
      try {
        this.copyViaExecCommand(text)
        confirm()
      } catch {
        fail()
      }
    }
  }

  copyViaExecCommand(text) {
    const area = document.createElement("textarea")
    area.value = text
    area.setAttribute("readonly", "")
    area.style.position = "fixed"
    area.style.opacity = "0"
    document.body.appendChild(area)
    area.select()
    let ok = false
    try {
      ok = document.execCommand("copy")
    } finally {
      area.remove()
    }
    if (!ok) throw new Error("copy failed")
  }
}
