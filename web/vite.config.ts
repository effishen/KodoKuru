import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// `npm run dev` proxies the API to a running KodoKuru (default https://127.0.0.1:7443).
const target = process.env.KODOKURU_URL ?? "https://127.0.0.1:7443";

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: { "/api": { target, secure: false, ws: true, changeOrigin: false } },
  },
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 1500 },
});
