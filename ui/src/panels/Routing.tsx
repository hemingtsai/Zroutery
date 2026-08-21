import { useState } from "react";
import {
  api,
  CLASSES,
  type AppConfig,
  type ModelClass,
  type RoutingStrategy,
  type Snapshot,
} from "../api";
import { Banner, Button, Card, Field, Toggle } from "../components";

const STRATEGIES: { id: RoutingStrategy; label: string; hint: string }[] = [
  { id: "priority", label: "Priority", hint: "Lowest priority number first; weight breaks ties" },
  { id: "weighted_random", label: "Weighted random", hint: "Spread load by weight" },
  { id: "round_robin", label: "Round robin", hint: "Rotate through the class, ignoring priority" },
  { id: "lowest_latency", label: "Lowest latency", hint: "Prefer whatever has been fastest" },
];

export default function Routing({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (config: AppConfig) => Promise<void>;
  run: (task: () => Promise<Snapshot>) => Promise<void>;
  busy: boolean;
}) {
  const { config } = snapshot;
  const [aliasDraft, setAliasDraft] = useState({ from: "", to: "sonnet" as ModelClass });
  const [portDraft, setPortDraft] = useState(String(config.server.port));
  const [hostDraft, setHostDraft] = useState(config.server.host);

  const patchRouting = (patch: Partial<AppConfig["routing"]>) => {
    const next = structuredClone(config);
    Object.assign(next.routing, patch);
    void save(next);
  };

  const patchServer = (patch: Partial<AppConfig["server"]>) => {
    const next = structuredClone(config);
    Object.assign(next.server, patch);
    void save(next);
  };

  const addAlias = () => {
    const from = aliasDraft.from.trim();
    if (!from) return;
    const next = structuredClone(config);
    next.routing.client_aliases[from] = aliasDraft.to;
    setAliasDraft({ from: "", to: aliasDraft.to });
    void save(next);
  };

  const removeAlias = (from: string) => {
    const next = structuredClone(config);
    delete next.routing.client_aliases[from];
    void save(next);
  };

  return (
    <>
      <Card title="Class routing">
        <div className="controls">
          <Field
            label="Strategy"
            hint={STRATEGIES.find((s) => s.id === config.routing.strategy)?.hint}
          >
            <select
              value={config.routing.strategy}
              onChange={(e) => patchRouting({ strategy: e.currentTarget.value as RoutingStrategy })}
            >
              {STRATEGIES.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.label}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Max attempts" hint="Upstream tries per client request">
            <input
              type="number"
              min={1}
              max={10}
              value={config.routing.max_attempts}
              onChange={(e) => patchRouting({ max_attempts: Number(e.currentTarget.value) || 1 })}
            />
          </Field>
          <Field label="Break after failures" hint="Consecutive errors before cooldown">
            <input
              type="number"
              min={1}
              value={config.routing.break_after_failures}
              onChange={(e) =>
                patchRouting({ break_after_failures: Number(e.currentTarget.value) || 1 })
              }
            />
          </Field>
          <Field label="Cooldown (s)">
            <input
              type="number"
              min={1}
              value={config.routing.cooldown_secs}
              onChange={(e) => patchRouting({ cooldown_secs: Number(e.currentTarget.value) || 60 })}
            />
          </Field>
        </div>
        <div className="grid-two">
          <Toggle
            label="Fail over inside a class"
            hint="Try the next model when one errors"
            checked={config.routing.failover}
            onChange={(failover) => patchRouting({ failover })}
          />
          <Toggle
            label="Understand Claude model names"
            hint="claude-*-sonnet-* → sonnet-class"
            checked={config.routing.match_claude_names}
            onChange={(match_claude_names) => patchRouting({ match_claude_names })}
          />
        </div>
        <Field
          label="Fallback for unknown model ids"
          hint="Leave as 404 unless a client insists on names you cannot predict"
        >
          <select
            value={config.routing.unknown_model_fallback ?? ""}
            onChange={(e) =>
              patchRouting({
                unknown_model_fallback: (e.currentTarget.value || null) as ModelClass | null,
              })
            }
          >
            <option value="">Return 404</option>
            {CLASSES.map((c) => (
              <option key={c} value={c}>
                {c}-class
              </option>
            ))}
          </select>
        </Field>
      </Card>

      <Card title="Client model aliases">
        <p className="field-hint">
          Map an exact model id a client sends onto one of your classes. These win over the Claude
          name heuristic.
        </p>
        {Object.entries(config.routing.client_aliases).length > 0 && (
          <table className="table">
            <thead>
              <tr>
                <th>Client asks for</th>
                <th>Routed to</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {Object.entries(config.routing.client_aliases).map(([from, to]) => (
                <tr key={from}>
                  <td>
                    <code>{from}</code>
                  </td>
                  <td>{to}-class</td>
                  <td>
                    <Button kind="ghost" onClick={() => removeAlias(from)}>
                      Remove
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <div className="controls">
          <Field label="Model id">
            <input
              value={aliasDraft.from}
              placeholder="claude-opus-4-1-20250805"
              onChange={(e) => setAliasDraft({ ...aliasDraft, from: e.currentTarget.value })}
              onKeyDown={(e) => e.key === "Enter" && addAlias()}
            />
          </Field>
          <Field label="Class">
            <select
              value={aliasDraft.to}
              onChange={(e) =>
                setAliasDraft({ ...aliasDraft, to: e.currentTarget.value as ModelClass })
              }
            >
              {CLASSES.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </Field>
          <div className="field-actions">
            <Button onClick={addAlias} disabled={busy || !aliasDraft.from.trim()}>
              Add alias
            </Button>
          </div>
        </div>
      </Card>

      <Card title="Local server" tone={snapshot.server.exposed ? "warn" : undefined}>
        <div className="controls">
          <Field label="Host" hint="127.0.0.1 keeps the proxy on this machine">
            <input
              value={hostDraft}
              onChange={(e) => setHostDraft(e.currentTarget.value)}
              onBlur={() => hostDraft !== config.server.host && patchServer({ host: hostDraft })}
            />
          </Field>
          <Field label="Port">
            <input
              type="number"
              min={1}
              max={65535}
              value={portDraft}
              onChange={(e) => setPortDraft(e.currentTarget.value)}
              onBlur={() => {
                const port = Number(portDraft);
                if (port >= 1 && port <= 65535 && port !== config.server.port) {
                  patchServer({ port });
                }
              }}
            />
          </Field>
          <Field label="Kept requests" hint="Size of the in-memory activity log">
            <input
              type="number"
              min={10}
              max={5000}
              value={config.server.log_limit}
              onChange={(e) => patchServer({ log_limit: Number(e.currentTarget.value) || 500 })}
            />
          </Field>
        </div>

        <div className="grid-two">
          <Toggle
            label="Require the local token"
            hint="Strongly recommended"
            checked={config.server.require_auth}
            onChange={(require_auth) => patchServer({ require_auth })}
          />
          <Toggle
            label="Start the proxy on launch"
            checked={config.server.autostart}
            onChange={(autostart) => patchServer({ autostart })}
          />
          <Toggle
            label="Allow browser origins (CORS)"
            hint="Only needed for web based clients"
            checked={config.server.allow_cors}
            onChange={(allow_cors) => patchServer({ allow_cors })}
          />
        </div>

        <div className="controls">
          <Field label="Local token" wide hint="Send as x-api-key or Authorization: Bearer">
            <input readOnly value={config.server.auth_token} onFocus={(e) => e.currentTarget.select()} />
          </Field>
          <div className="field-actions">
            <Button kind="ghost" onClick={() => api.copy(config.server.auth_token)}>
              Copy
            </Button>
            <Button
              kind="danger"
              onClick={() => run(api.regenerateToken)}
              title="Existing clients will need the new token"
            >
              Regenerate
            </Button>
          </div>
        </div>

        {config.server.host !== "127.0.0.1" && (
          <Banner tone="danger">
            Anything that can reach <code>{config.server.host}:{config.server.port}</code> can spend
            your API credit. Keep the token requirement on.
          </Banner>
        )}
      </Card>

      <Card title="How to point a client at Zroutery">
        <p className="field-hint">Anthropic style clients, including Claude Code:</p>
        <pre className="snippet">
{`export ANTHROPIC_BASE_URL=${snapshot.server.base_url ?? `http://${config.server.host}:${config.server.port}`}
export ANTHROPIC_AUTH_TOKEN=${config.server.auth_token}
export ANTHROPIC_MODEL=sonnet-class`}
        </pre>
        <p className="field-hint">OpenAI style clients:</p>
        <pre className="snippet">
{`export OPENAI_BASE_URL=${snapshot.server.base_url ?? `http://${config.server.host}:${config.server.port}`}/v1
export OPENAI_API_KEY=${config.server.auth_token}
# then request the model "opus-class", "sonnet-class", "haiku-class"
# or any exact id from the Models tab`}
        </pre>
      </Card>
    </>
  );
}
