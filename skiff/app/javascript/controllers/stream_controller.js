import { Controller } from "@hotwired/stimulus"

// DW-001 §6 (revised): the transcript streams over SSE, it does not poll.
// The bridge pushes snapshot/append/replace/remove/working/orchestrator
// events; Rails (sessions#stream) translates each into a turbo-stream, and
// this controller renders them as they land. EventSource owns reconnection —
// every reconnect delivers a fresh snapshot that replaces the transcript —
// so the DOM converges on the server's state no matter what was missed.
// There is no position bookkeeping, no backoff, no visibility logic: the
// stream is open for the life of the page.
export default class extends Controller {
  connect() {
    this.source = new EventSource(this.element.dataset.streamUrl)
    this.source.addEventListener("message", (event) => {
      if (event.data) Turbo.renderStreamMessage(event.data)
    })
  }

  disconnect() {
    this.source?.close()
    this.source = null
  }
}
