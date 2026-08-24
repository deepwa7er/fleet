import { StrictMode } from "react"
import { createRoot } from "react-dom/client"

import { App } from "./App"
import "./index.css"

const root = document.getElementById("root")
if (!root) throw new Error("index.html is missing #root")

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
)

// The worker caches only content-addressed client assets and the honest
// offline page. WebSocket traffic, change state, and transcripts never enter
// Cache Storage; reconnect snapshots remain the sole convergence mechanism.
if ("serviceWorker" in navigator) {
  addEventListener("load", () => {
    void navigator.serviceWorker.register("/service-worker.js", { scope: "/" })
  })
}
