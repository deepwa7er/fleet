import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// During `bun run dev`, proxy API calls to a locally running `atlas serve`.
// In production the same axum binary serves this built bundle, so /api is
// same-origin. For dev, run `atlas` with a loopback config on this port.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:7880",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
