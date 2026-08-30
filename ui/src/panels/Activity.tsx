import { api, costText, totalsText, type Snapshot } from "../api";
import { Badge, Button, Card, Empty, ms, num } from "../components";

export default function Activity({
  snapshot,
  run,
}: {
  snapshot: Snapshot;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
}) {
  const { summary, health, recent } = snapshot;
  const success = summary.requests - summary.failures;
  const rate = summary.requests ? Math.round((success / summary.requests) * 100) : 100;
  const classifier = summary.per_kind.find((k) => k.kind === "auto_mode");
  const classifierRate =
    classifier && classifier.requests > 0
      ? Math.round(
          ((classifier.requests - classifier.failures) / classifier.requests) * 1000,
        ) / 10
      : null;

  return (
    <>
      <Card
        title="Since launch"
        actions={
          <Button kind="ghost" onClick={() => run(api.clearStats)}>
            Reset counters
          </Button>
        }
      >
        <div className="stats">
          <div>
            <strong>{num(summary.requests)}</strong>
            <span>requests</span>
          </div>
          <div>
            <strong>{rate}%</strong>
            <span>succeeded</span>
          </div>
          <div>
            <strong>{num(summary.input_tokens)}</strong>
            <span>input tokens</span>
          </div>
          <div>
            <strong>{num(summary.output_tokens)}</strong>
            <span>output tokens</span>
          </div>
          <div>
            <strong>{totalsText(summary.cost)}</strong>
            <span>estimated spend</span>
          </div>
        </div>
        <p className="field-hint">
          Spend is computed from the prices you entered and the usage each provider reported.
          Unpriced models contribute nothing, so a total is a floor, not a bill.
        </p>

        {classifier && classifier.requests > 0 && (
          <p className="field-hint">
            <Badge tone={classifierRate !== null && classifierRate >= 90 ? "ok" : "warn"}>
              auto mode {classifierRate}%
            </Badge>{" "}
            {classifier.requests.toLocaleString()} classifier queries ({classifier.failures}{" "}
            failed, avg {ms(classifier.avg_latency_ms)}) — counted separately from main
            traffic, so a slow classifier never reads as a slow model.
          </p>
        )}

        {summary.per_model.length > 0 && (
          <table className="table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Requests</th>
                <th>Failures</th>
                <th>In</th>
                <th>Out</th>
                <th>Reasoning</th>
                <th>Cached</th>
                <th>Spend</th>
                <th>Avg latency</th>
              </tr>
            </thead>
            <tbody>
              {summary.per_model.map((m) => (
                <tr key={m.model_id}>
                  <td>{m.model_id}</td>
                  <td>{num(m.requests)}</td>
                  <td>{m.failures ? <Badge tone="danger">{m.failures}</Badge> : "0"}</td>
                  <td>{num(m.input_tokens)}</td>
                  <td>{num(m.output_tokens)}</td>
                  <td>{num(m.reasoning_tokens)}</td>
                  <td>{num(m.cached_tokens)}</td>
                  <td>{totalsText(m.cost)}</td>
                  <td>{ms(m.avg_latency_ms)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>

      <Card title="Model health">
        {health.length === 0 ? (
          <Empty>No upstream calls yet.</Empty>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Model</th>
                <th>State</th>
                <th>Streak</th>
                <th>OK / failed</th>
                <th>Avg latency</th>
                <th>Last error</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {health.map((h) => (
                <tr key={h.model_id}>
                  <td>{h.model_id}</td>
                  <td>
                    {h.cooldown_remaining_secs > 0 ? (
                      <Badge tone="danger">cooling {h.cooldown_remaining_secs}s</Badge>
                    ) : (
                      <Badge tone="ok">ready</Badge>
                    )}
                  </td>
                  <td>{h.consecutive_failures}</td>
                  <td>
                    {num(h.total_success)} / {num(h.total_failure)}
                  </td>
                  <td>{ms(h.avg_latency_ms)}</td>
                  <td className="muted truncate" title={h.last_error ?? ""}>
                    {h.last_error ?? "—"}
                  </td>
                  <td>
                    <Button kind="ghost" onClick={() => run(() => api.resetHealth(h.model_id))}>
                      Clear
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>

      <Card title="Recent requests">
        {recent.length === 0 ? (
          <Empty>Nothing yet. Point a client at the base URL to see traffic here.</Empty>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Time</th>
                <th>API</th>
                <th>Asked for</th>
                <th>Answered by</th>
                <th>Tokens</th>
                <th>Cost</th>
                <th>TTFT</th>
                <th>Total</th>
                <th>Result</th>
              </tr>
            </thead>
            <tbody>
              {recent.map((r) => (
                <tr key={r.id}>
                  <td className="muted">{new Date(r.at).toLocaleTimeString()}</td>
                  <td>
                    <Badge>{r.ingress === "anthropic" ? "messages" : "chat"}</Badge>
                    {r.stream && <Badge tone="neutral">stream</Badge>}
                    {r.kind === "auto_mode" && <Badge tone="warn">auto mode</Badge>}
                  </td>
                  <td>{r.requested_model}</td>
                  <td>
                    {r.resolved_model ?? "—"}
                    {r.attempts > 1 && <Badge tone="warn">{r.attempts} tries</Badge>}
                  </td>
                  <td className="muted">
                    {r.usage.input_tokens}/{r.usage.output_tokens}
                  </td>
                  <td className={r.cost ? "" : "muted"}>{costText(r.cost)}</td>
                  <td>{ms(r.ttft_ms)}</td>
                  <td>{ms(r.latency_ms)}</td>
                  <td>
                    {r.ok ? (
                      <Badge tone="ok">200</Badge>
                    ) : (
                      <span className="row gap">
                        <Badge tone="danger">{r.status}</Badge>
                        <span className="muted truncate" title={r.error ?? ""}>
                          {r.error}
                        </span>
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>
    </>
  );
}
