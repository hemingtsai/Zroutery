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
    // One 500 kB chunk is React, the router and the two dictionaries in the
    // same file. Splitting the libraries out keeps the app chunk below the
    // warning threshold and, more usefully, lets a change to the interface
    // revalidate one file instead of every dependency too.
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            {
              name: "react-vendor",
              test: /node_modules[\\/](react|react-dom|scheduler)[\\/]/,
              priority: 20,
            },
            {
              name: "vendor",
              test: /node_modules[\\/]/,
              priority: 10,
            },
          ],
        },
      },
    },
  },
});
