import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import { detectLang, prepareLang } from "./i18n";
import "@fontsource-variable/noto-serif-sc";
import "@fontsource/ibm-plex-mono/400.css";
import "./styles.css";

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

