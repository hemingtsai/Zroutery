import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the dev server on a fixed port and needs a stable host.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "safari15",
    sourcemap: false,
    emptyOutDir: true,
  },
});
