import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// `pnpm exec vite` serves the UI with hot reload and sends the API to a Davy
// on DAVY_URL. Davy accepts a write only with its own address as `Origin`,
// so the proxy sets it.
declare const process: { env: Record<string, string | undefined> };
const davy = process.env.DAVY_URL ?? "http://127.0.0.1:8000";

export default defineConfig({
  base: "/ui/",
  plugins: [react()],
  build: {
    target: "es2022",
    // Every browser that runs this UI supports modulepreload, and the
    // polyfill would be one more script.
    modulePreload: { polyfill: false },
    // The binary serves source without maps, so a map would only add size.
    sourcemap: false,
    reportCompressedSize: false,
  },
  server: {
    proxy: {
      "/ui/api": {
        target: davy,
        headers: { origin: davy },
      },
    },
  },
});
