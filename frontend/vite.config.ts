import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const apiPort = process.env.VITE_API_PORT || "16000";

// A standalone `bun run build` writes frontend/dist. Cargo instead sets this to
// its private OUT_DIR, because generated files are outputs rather than inputs to
// build.rs. Either way the one bundle is compiled into the binary (src/web.rs),
// which serves it over HTTP from an origin root.
const outDir = process.env.SAVE_AUDIO_STREAM_FRONTEND_OUT_DIR ?? "dist";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    outDir,
    // Vite does not empty an output outside the project root by default. Cargo's
    // directory must not retain obsolete content-hashed assets between builds.
    emptyOutDir: true,
  },
  server: {
    proxy: {
      "/api": {
        target: `http://localhost:${apiPort}`,
        changeOrigin: true,
      },
    },
  },
});
