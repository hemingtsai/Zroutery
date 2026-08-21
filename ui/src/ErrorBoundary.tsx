import { Component, type ErrorInfo, type ReactNode } from "react";
import { errorText } from "./api";

interface State {
  error: Error | null;
}

/**
 * Keeps a render bug from leaving a blank window.
 *
 * The dashboard is the only way to reach the configuration, so a crash here must
 * still offer a way out: the error is shown with a reload button, and the proxy
 * itself keeps running in the menu bar regardless.
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
    return (
      <main className="crash" role="alert">
        <h1>The dashboard hit a bug</h1>
        <p className="muted">
          The proxy is unaffected and keeps running in the menu bar.
        </p>
        <pre className="snippet">{errorText(error)}</pre>
        <div className="row gap">
          <button className="btn btn-primary" onClick={() => this.setState({ error: null })}>
            Try again
          </button>
          <button className="btn" onClick={() => window.location.reload()}>
            Reload
          </button>
        </div>
      </main>
    );
  }
}
