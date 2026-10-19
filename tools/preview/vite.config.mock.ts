import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";

const r = (p: string) => fileURLToPath(new URL(p, import.meta.url));

/**
 * Dev-only config: serves the real app source with @tauri-apps/api stubbed,
 * so the UI can be screenshot inside a normal browser.
 */
export default defineConfig({
  root: r("../../"),
  plugins: [react()],
  resolve: {
    alias: {
      "@tauri-apps/api/core": r("./tauri-core-mock.ts"),
      "@tauri-apps/api/event": r("./tauri-event-mock.ts"),
    },
  },
  server: {
    host: "127.0.0.1",
    port: 5199,
    strictPort: true,
    watch: {
      ignored: [
        "**/tools/preview/.edge-profile/**",
        "**/tools/preview/shots/**",
        "**/src-tauri/**",
        "**/node_modules/**",
        "**/dist/**",
      ],
    },
  },
  build: {
    target: "chrome105",
  },
});
