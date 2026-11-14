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
import "@fontsource/ibm-plex-mono/400.css";
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

