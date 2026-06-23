import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// During `bun run dev`, proxy API calls to a locally running `drydock serve`.
// In production the same axum server serves this built bundle, so /api is same-origin.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:7878",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
});
