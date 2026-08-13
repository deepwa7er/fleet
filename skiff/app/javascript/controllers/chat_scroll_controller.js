import { Controller } from "@hotwired/stimulus"

// How close to the document bottom still counts as "at the bottom" — a little
// slack so a reader near the edge is treated as pinned, not detached.
const PIN_SLACK = 24

// DW-001 §6: opening a chat places the reader at the newest message — the
// page reads bottom-up, the composer anchoring its foot — and a reader pinned
// to the bottom follows new messages as they stream in. Pinned-ness is a
// property of the reader's position, never a guess about the session: any
// scroll up detaches, any return to the bottom re-attaches, so a follow can
// never yank someone reading history. Motion discipline (rule 6): placement
// and follows are instant jumps, never animations.
//
// New content arrives in three ways — the server-rendered first paint, the
// stream's append/replace events, and wholesale transcript replacements — so
// a MutationObserver on the transcript wrapper catches every path uniformly,
// instead of each content source having to announce itself.
//
// The placement itself has to outlast Turbo Drive: every rendered visit ends
// with Turbo settling the scroll position (performScroll runs after the body
// swap, i.e. after this controller's connect scroll), so a navigation into
// this page — sending a message, following a session link — would otherwise
// land at the top. turbo:load fires once the visit and its scroll settling
// are over, so that is the moment to reassert the placement; see
// reassertPinnedPlacement.
export default class extends Controller {
  connect() {
    this.pinned = true
    this.scrollToBottom()

    this.handleScroll = this.updatePinned.bind(this)
    window.addEventListener("scroll", this.handleScroll, { passive: true })

    this.handleTurboLoad = this.reassertPinnedPlacement.bind(this)
    document.addEventListener("turbo:load", this.handleTurboLoad)

    this.observer = new MutationObserver(() => {
      if (this.pinned) this.scrollToBottom()
    })
    // childList on the wrapper catches a repaint replacing #transcript
    // itself; subtree catches the stream's appends deep inside it.
    this.observer.observe(this.element, { childList: true, subtree: true })
  }

  disconnect() {
    window.removeEventListener("scroll", this.handleScroll)
    document.removeEventListener("turbo:load", this.handleTurboLoad)
    this.observer.disconnect()
  }

  updatePinned() {
    this.pinned = this.atBottom()
  }

  // Turbo's scroll settling after a rendered visit is motion, not a reader
  // gesture: performScroll (top, or a restored position) moves the viewport
  // after the body swap, and the scroll events it triggers may reach
  // updatePinned before or after this reassert depending on task ordering.
  // A freshly rendered page is pinned by definition — this controller only
  // exists on this page — so the reassert restores the invariant outright
  // instead of trusting events Turbo itself triggered. It runs at
  // turbo:load, once the visit's scroll settling is over, and thus lands
  // the reader at the newest message on every entry: first paint, a session
  // link, and the send-message round trip alike.
  reassertPinnedPlacement() {
    this.pinned = true
    this.scrollToBottom()
  }

  atBottom() {
    return window.innerHeight + window.scrollY >= document.documentElement.scrollHeight - PIN_SLACK
  }

  scrollToBottom() {
    window.scrollTo(0, document.documentElement.scrollHeight)
  }
}
