import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import ErrorBoundary from "./ErrorBoundary";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-mono/400.css";
import "./styles.css";

// Brand type: IBM Plex carries Latin, the platform's own font carries CJK.
// Pinning the platform class lets each OS get exactly one CJK fallback in its
// stack, instead of every OS seeing every other OS's font.
document.documentElement.classList.add(
  /Mac|iPhone|iPad/.test(navigator.userAgent) ? "platform-macos" : "platform-windows",
);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>,
);

