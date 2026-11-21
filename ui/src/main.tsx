import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { detectLang, prepareLang } from "./i18n";
import { applySkin, loadSkin } from "./skin";
import { applyTheme, loadThemePref } from "./theme";

const initialSkin = loadSkin();
applySkin(initialSkin);
applyTheme(loadThemePref());
// The UI text is Latin script; the mono face carries no CJK glyphs either way.
// Shipping only the Latin subsets keeps the Cyrillic, Greek and Vietnamese
// @font-face rules — about two fifths of the vendor CSS — out of the bundle.
import "@fontsource/ibm-plex-mono/latin-400.css";
import "@fontsource/ibm-plex-mono/latin-ext-400.css";
import "./styles.css";
import "virtual:zroutery-bundled-themes.css";

// Traditional Chinese lives in its own chunk; resolving it before the first
// paint is what keeps a zh-TW reader from seeing English for a frame.
void prepareLang(detectLang()).then(() => {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </StrictMode>,
  );
});

