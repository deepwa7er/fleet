// skiff's service worker. Policy, recorded so it is not re-litigated:
//
//   digested assets (/assets/*)  -> cache-first; populated on first use
//   navigations                  -> network-first; the offline page is the
//                                    honest fallback when the tailnet is
//                                    unreachable (DW-001 G10: a single honest
//                                    line and a way back)
//   everything else              -> network-only, NEVER cached — transcripts,
//                                    the stream's turbo-streams, and the
//                                    manifest must always reflect the server
//
// The worker only ever serves immutable files from the cache: the design
// rests on the DOM being the live source of truth, and a cached transcript
// would be a lie. Push handling is deliberately absent until push lands.
const CACHE = "skiff-assets-v1"
const OFFLINE_PAGE = "/offline.html"

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.add(OFFLINE_PAGE)))
  self.skipWaiting()
})

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))))
  )
  self.clients.claim()
})

self.addEventListener("fetch", (event) => {
  const { request } = event
  if (request.method !== "GET") return

  const url = new URL(request.url)
  if (url.origin !== self.location.origin) return

  // Digested assets are immutable — a cache hit is never stale.
  if (url.pathname.startsWith("/assets/")) {
    event.respondWith(
      caches.match(request).then((hit) => {
        if (hit) return hit
        return fetch(request).then((response) => {
          if (response.ok) {
            const copy = response.clone()
            caches.open(CACHE).then((cache) => cache.put(request, copy))
          }
          return response
        })
      })
    )
    return
  }

  // Navigations: the network decides, always. A failure serves the offline
  // page rather than the browser's dead-end error.
  if (request.mode === "navigate") {
    event.respondWith(fetch(request).catch(() => caches.match(OFFLINE_PAGE)))
    return
  }

  // Anything else — the stream especially — passes through uncached.
})
