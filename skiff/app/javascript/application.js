// Configure your import map in config/importmap.rb. Read more: https://github.com/rails/importmap-rails
import "@hotwired/turbo-rails"
import "controllers"

// PWA: register the service worker after first paint. Its policy is
// asset-only caching (see app/views/pwa/service-worker.js) — transcripts
// and streams always hit the network. Registration requires a secure
// context, so the https path (breakwater) gets the worker and the
// tailnet-IP http fallback simply continues without one; that failure is
// expected there and is only worth a warning, never a crash.
if ("serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/service-worker.js").catch((err) => {
      console.warn("service worker registration failed:", err)
    })
  })
}
