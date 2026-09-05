import { Component, type ErrorInfo, type ReactNode } from "react";
import { errorText } from "./api";
import { detectLang } from "./i18n";

interface State {
  error: Error | null;
}

/**
 * Keeps a render bug from leaving a blank window.
 *
 * The dashboard is the only way to reach the configuration, so a crash here must
 * still offer a way out: the error is shown with a reload button, and the proxy
 * itself keeps running in the menu bar regardless. It sits outside the i18n
 * provider (it must render when anything inside crashes), so it reads the
 * stored language directly.
 */
const strings = {
  zh: {
    title: "界面出现了问题",
    unaffected: "网关不受影响,仍在菜单栏正常运行。",
    tryAgain: "重试",
    reload: "重新加载",
  },
  en: {
    title: "The dashboard hit a bug",
    unaffected: "The proxy is unaffected and keeps running in the menu bar.",
    tryAgain: "Try again",
    reload: "Reload",
  },
};

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
    const s = strings[detectLang()];
    return (
      <main className="crash" role="alert">
        <h1>{s.title}</h1>
        <p className="muted">{s.unaffected}</p>
        <pre className="snippet">{errorText(error)}</pre>
        <div className="row gap">
          <button className="btn btn-primary" onClick={() => this.setState({ error: null })}>
            {s.tryAgain}
          </button>
          <button className="btn" onClick={() => window.location.reload()}>
            {s.reload}
          </button>
        </div>
      </main>
    );
  }
}
