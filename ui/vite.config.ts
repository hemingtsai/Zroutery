import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { compileTheme } from "./src/themes/compiler.ts";
// The ui tsconfig carries no @types/node, but vite bundles this config with
// esbuild, which resolves node builtins at run time — the suppression and the
// local `process` shape are load-bearing.
// @ts-expect-error — node builtins are unavailable to the ui typecheck
import { existsSync, readFileSync } from "node:fs";
declare const process: { cwd(): string };

// Both official data packs go through the same bounded compiler as third-party
// themes. Only compiled CSS is shipped in the first-frame sheet; the raw recipe
// JSON is not parsed twice at runtime or paid for in the entry JavaScript chunk.
const REPO_ROOT = (() => {
  let dir = process.cwd().replace(/\\/g, "/");
  for (let depth = 0; depth < 6 && !existsSync(`${dir}/plugins/theme-md3/theme.json`); depth += 1) {
    const parent = dir.slice(0, dir.lastIndexOf("/"));
    if (!parent) throw new Error("theme packs not found above the working directory");
    dir = parent;
  }
  return dir;
})();
const PACKS = [
  { id: "org.zroutery.theme.md3", dir: `${REPO_ROOT}/plugins/theme-md3` },
  { id: "org.zroutery.theme.fluent", dir: `${REPO_ROOT}/plugins/theme-fluent` },
];
const bundledThemes = {
  name: "zroutery-bundled-themes",
  resolveId(id: string) { return id === "virtual:zroutery-bundled-themes.css" ? "\0zroutery-bundled-themes.css" : undefined; },
  load(this: { addWatchFile?: (path: string) => void }, id: string) {
    if (id !== "\0zroutery-bundled-themes.css") return undefined;
    // Read at request time and watch the pack files: config-level imports are
    // evaluated once at server start, so pack edits never reached a running
    // dev session and the skins silently went stale.
    return PACKS.map(({ id: packId, dir }) => {
      const manifest = JSON.parse(readFileSync(`${dir}/zroutery.plugin.json`, "utf-8"));
      const theme = JSON.parse(readFileSync(`${dir}/theme.json`, "utf-8"));
      this.addWatchFile?.(`${dir}/zroutery.plugin.json`);
      this.addWatchFile?.(`${dir}/theme.json`);
      return compileTheme(theme, (manifest as { id: string }).id || packId);
    }).join("\n");
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
