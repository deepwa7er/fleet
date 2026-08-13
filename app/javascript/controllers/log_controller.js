import { Controller } from "@hotwired/stimulus"

// Keeps the harness output pinned to the newest line as it streams.
//
// The log lives inside a Turbo Frame that is replaced wholesale every couple of
// seconds, so the <pre> is a *different element* after each poll and its scroll
// position starts at zero — which is why the output kept snapping back to the
// top. Restoring it therefore needs state that outlives the element, hence the
// module-scoped variables below: the controller instance is torn down and
// rebuilt on every swap, but the module is not.
//
// Scrolling up is treated as intent. Someone reading back through earlier output
// should not be yanked to the bottom two seconds later, so auto-scroll re-engages
// only when they return to the bottom themselves.
let stuckToBottom = true
let lastScrollTop = 0

// How close to the bottom still counts as "at the bottom". Without some slack,
// sub-pixel rounding and the final partial line can leave a container a fraction
// short and silently disable following.
const BOTTOM_SLACK_PX = 24

export default class extends Controller {
  connect() {
    // Deferred a frame: connect() can run before the swapped-in content has been
    // laid out, and scrollHeight read too early is the previous, shorter value —
    // which lands you near the bottom but not at it.
    this.pending = requestAnimationFrame(() => {
      if (stuckToBottom) {
        this.scrollToBottom()
      } else {
        // Put them back where they were, not at the top of the new element.
        this.element.scrollTop = lastScrollTop
      }
    })

    this.handleScroll = () => {
      lastScrollTop = this.element.scrollTop
      stuckToBottom = this.atBottom()
    }
    this.element.addEventListener("scroll", this.handleScroll, { passive: true })
  }

  disconnect() {
    if (this.pending) cancelAnimationFrame(this.pending)
    this.element.removeEventListener("scroll", this.handleScroll)
  }

  scrollToBottom() {
    this.element.scrollTop = this.element.scrollHeight
    lastScrollTop = this.element.scrollTop
  }

  atBottom() {
    const distance =
      this.element.scrollHeight - this.element.scrollTop - this.element.clientHeight
    return distance <= BOTTOM_SLACK_PX
  }
}
