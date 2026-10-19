import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// tauri dev 同时启动 vite 和 cargo。Vite 默认会 watch 项目根，
// 但 src-tauri/target 里的产物（.dll/.exe）被进程锁定，触发 EBUSY，
// 因此显式忽略它们。build 走 frontendDist，不受影响。
const watchIgnore = /[/\\]src-tauri[/\\]target[/\\]/;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // 忽略 cargo 产物目录，避免 EBUSY
      ignored: [
        watchIgnore,
        "**/node_modules/**",
        "**/dist/**",
      ],
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "chrome105",
  },
});
