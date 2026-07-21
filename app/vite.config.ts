import { defineConfig } from 'vite';

// Tauri expects a fixed dev port and does its own reload orchestration.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Never watch Rust sources or the Cargo target dir: `target/` sits inside
    // this Vite root (workspace layout) and its build artifacts (the app DLL)
    // get locked mid-compile, which crashes chokidar with EBUSY.
    watch: { ignored: ['**/src-tauri/**', '**/target/**'] },
  },
});
