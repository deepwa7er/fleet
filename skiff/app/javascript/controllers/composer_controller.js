import { Controller } from "@hotwired/stimulus"

const MAX_ROWS = 6

// DW-001 §6: the composer's only motion is the field growing with its
// content; the press stays the primary .button. Enter sends, Shift+Enter (and
// Ctrl/Meta/Alt variants) insert a newline. The submit key disables itself
// for the duration of the Turbo request and re-enables on turbo:submit-end,
// so a double-tap cannot post twice.
export default class extends Controller {
  static targets = ["input", "submit"]

  connect() {
    this.handleSubmitEnd = this.reenable.bind(this)
    this.element.addEventListener("turbo:submit-end", this.handleSubmitEnd)

    // Capture the one-line metrics at a fresh (empty) textarea so autoGrow can
    // cap the box at MAX_ROWS of content plus the recess's own padding.
    const style = getComputedStyle(this.inputTarget)
    this.lineHeight = parseFloat(style.lineHeight)
    this.verticalPadding = this.inputTarget.clientHeight - this.lineHeight

    this.autoGrow()
  }

  disconnect() {
    this.element.removeEventListener("turbo:submit-end", this.handleSubmitEnd)
  }

  submitOnEnter(event) {
    if (event.key !== "Enter" || event.isComposing) return
    if (event.shiftKey || event.ctrlKey || event.metaKey || event.altKey) return
    event.preventDefault()
    this.element.requestSubmit()
  }

  autoGrow() {
    const input = this.inputTarget
    input.style.height = "auto"
    const maxHeight = Math.round(this.lineHeight * MAX_ROWS + this.verticalPadding)
    input.style.height = `${Math.min(input.scrollHeight, maxHeight)}px`
  }

  disableDuringSubmit() {
    this.submitTarget.disabled = true
    this.element.setAttribute("aria-busy", "true")
  }

  reenable() {
    this.submitTarget.disabled = false
    this.element.removeAttribute("aria-busy")
  }
}
