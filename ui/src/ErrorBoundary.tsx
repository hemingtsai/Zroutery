import { Component, type ErrorInfo, type ReactNode } from "react";
import { errorText } from "./api";
import { detectLang, dicts } from "./i18n";

interface State {
  error: Error | null;
}

/**
 * Keeps a render bug from leaving a blank window.
 *
 * The dashboard is the only way to reach the configuration, so a crash here must
 * still offer a way out: the error is shown with a reload button, and the proxy
 * itself keeps running in the menu bar regardless. It sits outside the i18n
 * provider — it must render when anything inside crashes — so it reads the
 * dictionary directly rather than through the hook.
 *
 * The dictionary is the same one the rest of the interface uses, which is the
 * point: this used to keep a private copy of four strings, and a copy of a
 * string is a second thing to keep in step with the first.
 */
export default class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("dashboard crashed", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const s = dicts[detectLang()];
    return (
      <main className="crash" role="alert">
        <h1>{s["error.title"]}</h1>
        <p className="muted">{s["error.unaffected"]}</p>
        <pre className="snippet">{errorText(error)}</pre>
        <div className="row gap">
          <button className="btn btn-primary" onClick={() => this.setState({ error: null })}>
            {s["common.try_again"]}
          </button>
          <button className="btn" onClick={() => window.location.reload()}>
            {s["common.reload"]}
          </button>
        </div>
      </main>
    );
  }
}