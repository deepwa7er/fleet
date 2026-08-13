// The entry point for the ADMIN, served only on the tailnet hostname.
//
// Everything the public site loads, plus the editor. Lexxy is 933 KB and powers
// no published-page behaviour whatsoever — it defines the editor element and
// nothing that touches rendered `.lexxy-content` — so it is loaded here rather
// than in application.js, and pinned `preload: false` so it is fetched only
// when this file actually imports it.
import "application"

import "lexxy"
import "@rails/actiontext"

// Lexxy builds its toolbar synchronously on connect but mounts the content area
// a frame later, so `:defined` is not enough to know it is ready — without this
// you get a flash of a toolbar with no editor under it on every navigation.
// The `ready` attribute is what application.css keys its visibility off.
addEventListener("lexxy:initialize", (event) => {
  event.target.toggleAttribute("ready", true)
})
