import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { compileTheme } from "./src/themes/compiler.ts";
import md3Manifest from "../plugins/theme-md3/zroutery.plugin.json" with { type: "json" };
import fluentManifest from "../plugins/theme-fluent/zroutery.plugin.json" with { type: "json" };
import md3Theme from "../plugins/theme-md3/theme.json" with { type: "json" };
import fluentTheme from "../plugins/theme-fluent/theme.json" with { type: "json" };

// Both official data packs go through the same bounded compiler as third-party
// themes. Only compiled CSS is shipped in the first-frame sheet; the raw recipe
// JSON is not parsed twice at runtime or paid for in the entry JavaScript chunk.
const bundledThemes = {
  name: "zroutery-bundled-themes",
  resolveId(id: string) { return id === "virtual:zroutery-bundled-themes.css" ? "\0zroutery-bundled-themes.css" : undefined; },
  load(id: string) {
    if (id !== "\0zroutery-bundled-themes.css") return undefined;
    return [[md3Manifest, md3Theme], [fluentManifest, fluentTheme]].map(([manifest, theme]) => compileTheme(theme, (manifest as typeof md3Manifest).id)).join("\n");
  },
};

// Tauri serves the dev server on a fixed port and needs a stable host.
export default defineConfig({
  plugins: [react(), bundledThemes],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: "safari15",
    // Keep both the WebKit fallback and standard acrylic property. Safari15
    // alone makes the optimizer drop the standard declaration, breaking WebView2.
    cssTarget: ["safari15", "chrome120"],
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
