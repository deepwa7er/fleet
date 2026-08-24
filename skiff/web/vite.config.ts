import { defineConfig } from "vite"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"

// `bun run dev` serves the client and proxies the socket to a locally running
// skiffd, so the client can be iterated on without rebuilding the binary.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/ws": { target: "ws://127.0.0.1:8121", ws: true },
    },
  },
})
