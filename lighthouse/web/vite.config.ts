import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// In development the React dev server proxies API + log-stream requests to the
// Rust backend running on 127.0.0.1:8080. In production the Rust server serves
// the built assets directly, so requests are same-origin and no proxy applies.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      "/api": {
        target: "http://127.0.0.1:8080",
        changeOrigin: true,
      },
    },
  },
});
