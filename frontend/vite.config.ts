import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const apiPort = process.env.VITE_API_PORT || "16000";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    proxy: {
      "/api": {
        target: `http://localhost:${apiPort}`,
        changeOrigin: true,
      },
    },
  },
  build: {
    outDir: "dist",
    rollupOptions: {
      output: {
        entryFileNames: "assets/main.js",
        assetFileNames: (assetInfo) => {
          if (assetInfo.names?.[0]?.endsWith(".css")) {
            return "assets/style.css";
          }
          return "assets/[name].[ext]";
        },
      },
    },
  },
});
