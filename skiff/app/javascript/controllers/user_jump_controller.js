import { Controller } from "@hotwired/stimulus"

// The jump widget: two small keys that hop the reader between the messages
// the user sent in this session — the blocks the partial marked data-mine.
// It is chat-scroll's deliberate counterpart: that controller owns the
// pinned follow, this one owns traversal, and the two never fight — a hop
// away from the bottom detaches the follow (the scroll event flips pinned,
// like any reader gesture), and a hop that lands at the bottom re-attaches
// it.
//
// The message list is read fresh from the DOM on every interaction and
// never cached: the stream appends parts, replaces settled blocks, and
// repaints a diverged transcript wholesale, so any cached list would go
// stale. The MutationObserver on the wrapper catches every content path
// (the same observation chat-scroll makes), and a scroll listener keeps the
// disabled states honest as the reader scrolls by hand.
//
// Motion discipline (DW-001 §6): hops are instant jumps, never animations.
export default class extends Controller {
  static targets = ["widget", "previous", "next"]

  connect() {
    this.handleScroll = this.refresh.bind(this)
    window.addEventListener("scroll", this.handleScroll, { passive: true })

    this.observer = new MutationObserver(() => this.refresh())
    // childList on the wrapper catches a repaint replacing #transcript
    // itself; subtree catches the stream's appends deep inside it.
    this.observer.observe(this.element, { childList: true, subtree: true })

    this.refresh()
  }

  disconnect() {
    window.removeEventListener("scroll", this.handleScroll)
    this.observer.disconnect()
  }

  // The message the reader is on: the last user message whose top is at or
  // above the bottom of the viewport — the newest one the reader can see.
  // The anchor is the viewport bottom, not the top: at the page bottom the
  // reader is looking at the last user message, so it must count as current
  // (anchoring at the top would leave it uncounted and Up would skip it);
  // at the page top the first visible message is current, so Up is dead and
  // Down goes to the next one. -1 only above every user message.
  currentIndex(messages) {
    let index = -1
    const viewportBottom = window.innerHeight
    for (const message of messages) {
      if (message.getBoundingClientRect().top > viewportBottom) break
      index += 1
    }
    return index
  }

  // The widget is pointless with zero or one user message — it stays hidden
  // until there are two to jump between. Up is dead at the first message;
  // Down is never dead: past the last user message it falls back to the
  // page bottom (End), so it always has somewhere to go — only at the
  // absolute end of the page is that a no-op. A disabled Down at rest was
  // the reported deadness (it demanded a scroll before it could be used).
  refresh() {
    const messages = this.userMessages()
    this.widgetTarget.hidden = messages.length < 2
    if (messages.length < 2) return
    this.previousTarget.disabled = this.currentIndex(messages) <= 0
    this.nextTarget.disabled = false
  }

  previous() {
    this.jump(-1)
  }

  next() {
    this.jump(1)
  }

  // Direction is relative to the reader's position at press time; the list
  // is re-read so a just-arrived message is always in reach. Up goes to the
  // previous user message; Down to the next, or to the page bottom when the
  // reader is at or past the last one.
  jump(direction) {
    const messages = this.userMessages()
    const target = messages[this.currentIndex(messages) + direction]
    if (target) this.scrollToTop(target)
    else if (direction > 0) this.scrollToBottom()
  }

  // Land the message's top at the viewport top — or as low as the page
  // allows. The newest messages sit near the page bottom, where the browser
  // clamps any scroll past the end: an unclamped target would make the jump
  // a silent no-op (the reported dead Down at the page bottom).
  scrollToTop(message) {
    const maxScroll = document.documentElement.scrollHeight - window.innerHeight
    const top = message.getBoundingClientRect().top + window.scrollY
    window.scrollTo(0, Math.min(Math.max(top, 0), maxScroll))
  }

  scrollToBottom() {
    window.scrollTo(0, document.documentElement.scrollHeight)
  }

  // Document order is chronological, so the list is already sorted.
  userMessages() {
    return Array.from(this.element.querySelectorAll(".message[data-mine]"))
  }
}
