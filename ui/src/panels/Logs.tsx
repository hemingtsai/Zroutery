import { useEffect, useRef, useState } from "react";
import { api, errorText } from "../api";
import { Banner, Button, Card, Empty } from "../components";

/**
 * Live tracing output from the desktop process. Polls the in-memory log buffer
 * while the tab is mounted, and offers a one-click copy of everything visible.
 */
export default function Logs() {
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const viewRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const next = await api.logs();
        if (cancelled) return;
        setLines(next);
        setError(null);
      } catch (e) {
        if (!cancelled) setError(errorText(e));
      }
    };
    const timer = setInterval(poll, 2000);
    void poll();
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);

  // Keep the newest line in view as logs stream in.
  useEffect(() => {
    const el = viewRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  const text = lines.join("\n");

  return (
    <Card
      title="Log output"
      actions={
        <Button
          kind="ghost"
          disabled={lines.length === 0}
          onClick={() => api.copy(text)}
          title="Copy all visible log lines to the clipboard"
        >
          Copy logs
        </Button>
      }
    >
      {error && <Banner tone="warn">Logs unavailable: {error}</Banner>}
      {lines.length === 0 ? (
        <Empty>No log output yet. Start the proxy or make a request to see tracing here.</Empty>
      ) : (
        <pre ref={viewRef} className="log-view">
          {text}
          {"\n"}
        </pre>
      )}
    </Card>
  );
}
