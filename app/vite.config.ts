import { defineConfig } from "vite";

// Tauri expects a fixed dev port and does its own reload orchestration.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
