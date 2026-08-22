import { useState } from "react";
import {
  api,
  CLASSES,
  costText,
  errorText,
  type AppConfig,
  type ClassElection,
  type Election,
  type ModelClass,
  type RoutingStrategy,
  type Snapshot,
} from "../api";
import {
  Badge,
  Banner,
  Button,
  Card,
  Empty,
  Field,
  ms,
  NumberField,
  TextField,
  Toggle,
} from "../components";

const STRATEGIES: { id: RoutingStrategy; label: string; hint: string }[] = [
  {
    id: "balanced",
    label: "Balanced (elected)",
    hint: "An election measures latency and price, then pins the order",
  },
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
  const { config, server } = snapshot;
  const [aliasDraft, setAliasDraft] = useState({ from: "", to: "sonnet" as ModelClass });
  const [originDraft, setOriginDraft] = useState("");
  const [revealed, setRevealed] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const patchRouting = (patch: Partial<AppConfig["routing"]>) => {
    const next = structuredClone(config);
    Object.assign(next.routing, patch);
    void save(next);
  };

  const patchScoring = (patch: Partial<AppConfig["routing"]["scoring"]>) => {
    const next = structuredClone(config);
    Object.assign(next.routing.scoring, patch);
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

  const addOrigin = () => {
    const origin = originDraft.trim().replace(/\/$/, "");
    if (!origin) return;
    if (!/^https?:\/\//.test(origin)) {
      setNotice("An origin looks like https://example.com, with no path.");
      return;
    }
    if (config.server.cors_origins.includes(origin)) return;
    setOriginDraft("");
    setNotice(null);
    patchServer({ cors_origins: [...config.server.cors_origins, origin] });
  };

  const reveal = async () => {
    try {
      setRevealed(await api.revealToken());
    } catch (e) {
      setNotice(errorText(e));
    }
  };

  const baseUrl = server.base_url ?? `http://${config.server.host}:${config.server.port}`;

  return (
    <>
      {notice && (
        <Banner tone="warn" actions={<Button kind="ghost" onClick={() => setNotice(null)}>OK</Button>}>
          {notice}
        </Banner>
      )}

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
          <NumberField
            label="Max attempts"
            hint="Upstream tries per client request"
            min={1}
            max={10}
            value={config.routing.max_attempts}
            onCommit={(v) => patchRouting({ max_attempts: v ?? 1 })}
          />
          <NumberField
            label="Break after failures"
            hint="Consecutive errors before cooldown"
            min={1}
            value={config.routing.break_after_failures}
            onCommit={(v) => patchRouting({ break_after_failures: v ?? 1 })}
          />
          <NumberField
            label="Cooldown (s)"
            min={1}
            value={config.routing.cooldown_secs}
            onCommit={(v) => patchRouting({ cooldown_secs: v ?? 60 })}
          />
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

      {config.routing.strategy === "balanced" && (
        <Card
          title="Election"
          actions={
            <Button kind="primary" onClick={() => run(api.runElection)} disabled={busy}>
              Re-run now
            </Button>
          }
        >
          <p className="field-hint">
            An election sends one tiny completion to every model in every class, then pins the
            order by latency and price together. It runs when you ask and, with the box below
            ticked, once at startup — never on a timer, because each round costs one request per
            model.
          </p>
          <div className="controls">
            <NumberField
              label="Price weight"
              hint="Relative, so 3 and 1 means the same as 0.75 and 0.25"
              min={0}
              value={config.routing.scoring.price_weight}
              onCommit={(v) => patchScoring({ price_weight: v ?? 0 })}
            />
            <NumberField
              label="Latency weight"
              min={0}
              value={config.routing.scoring.latency_weight}
              onCommit={(v) => patchScoring({ latency_weight: v ?? 0 })}
            />
            <NumberField
              label="Reference input tokens"
              hint="Prices are per Mtok, so they need a request to be compared on"
              min={0}
              value={config.routing.scoring.reference_input_tokens}
              onCommit={(v) => patchScoring({ reference_input_tokens: v ?? 0 })}
            />
            <NumberField
              label="Reference output tokens"
              min={0}
              value={config.routing.scoring.reference_output_tokens}
              onCommit={(v) => patchScoring({ reference_output_tokens: v ?? 0 })}
            />
          </div>
          <Toggle
            label="Hold one at startup"
            hint="so the pinned order reflects today"
            checked={config.routing.elect_on_start}
            onChange={(elect_on_start) => patchRouting({ elect_on_start })}
          />
          <ElectionResult election={snapshot.election} />
        </Card>
      )}


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

      <Card title="Local server" tone={server.exposed ? "warn" : undefined}>
        <div className="controls">
          <TextField
            label="Host"
            hint="127.0.0.1 keeps the proxy on this machine"
            value={config.server.host}
            onCommit={(host) => patchServer({ host })}
          />
          <NumberField
            label="Port"
            min={1}
            max={65535}
            value={config.server.port}
            onCommit={(port) => port && patchServer({ port })}
          />
          <NumberField
            label="Max request body (MiB)"
            hint="Inline images make prompts big; this caps them"
            min={1}
            max={512}
            value={config.server.max_body_mib}
            onCommit={(v) => patchServer({ max_body_mib: v ?? 32 })}
          />
          <NumberField
            label="Kept requests"
            hint="Size of the in-memory activity log"
            min={10}
            max={5000}
            value={config.server.log_limit}
            onCommit={(v) => patchServer({ log_limit: v ?? 500 })}
          />
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
        </div>

        <div className="row gap wrap">
          <span className="field-label">Local token</span>
          <code className="url">{revealed ?? server.token_hint}</code>
          {revealed ? (
            <Button kind="ghost" onClick={() => setRevealed(null)}>
              Hide
            </Button>
          ) : (
            <Button kind="ghost" onClick={reveal} title="Show the token in this window">
              Reveal
            </Button>
          )}
          <Button kind="ghost" onClick={() => api.copyToken()}>
            Copy
          </Button>
          <Button
            kind="danger"
            onClick={() => {
              setRevealed(null);
              void run(api.regenerateToken);
            }}
            title="Existing clients will need the new token"
          >
            Regenerate
          </Button>
        </div>
        <p className="field-hint">
          Send it as <code>x-api-key</code> or <code>Authorization: Bearer</code>. The dashboard only
          holds the last four characters until you press Reveal.
        </p>

        {server.exposed && (
          <Banner tone="danger">
            Anything that can reach <code>{config.server.host}:{config.server.port}</code> can spend
            your API credit. Keep the token requirement on.
          </Banner>
        )}
      </Card>

      <Card
        title="Browser access (CORS)"
        tone={config.server.allow_cors && config.server.cors_origins.length === 0 ? "danger" : undefined}
      >
        <p className="field-hint">
          Only needed for clients that run inside a web page. Without an origin list every site you
          visit can call the proxy from your browser.
        </p>
        <Toggle
          label="Allow browser origins"
          checked={config.server.allow_cors}
          onChange={(allow_cors) => patchServer({ allow_cors })}
        />
        {config.server.allow_cors && (
          <>
            {config.server.cors_origins.length === 0 ? (
              <Banner tone="danger">
                No origins listed, so any origin is accepted. Add the ones you actually use.
              </Banner>
            ) : (
              <div className="chips">
                {config.server.cors_origins.map((origin) => (
                  <span key={origin} className="chip chip-done">
                    {origin}
                    <button
                      className="chip-remove"
                      aria-label={`Remove ${origin}`}
                      onClick={() =>
                        patchServer({
                          cors_origins: config.server.cors_origins.filter((o) => o !== origin),
                        })
                      }
                    >
                      ×
                    </button>
                  </span>
                ))}
              </div>
            )}
            <div className="controls">
              <Field label="Allowed origin" hint="Scheme, host and port, no path">
                <input
                  value={originDraft}
                  placeholder="http://localhost:3000"
                  onChange={(e) => setOriginDraft(e.currentTarget.value)}
                  onKeyDown={(e) => e.key === "Enter" && addOrigin()}
                />
              </Field>
              <div className="field-actions">
                <Button onClick={addOrigin} disabled={busy || !originDraft.trim()}>
                  Add origin
                </Button>
              </div>
            </div>
          </>
        )}
      </Card>

      <Card title="How to point a client at Zroutery">
        <p className="field-hint">
          Anthropic style clients, including Claude Code. Use <em>Copy token</em> above for the value
          of the second line.
        </p>
        <pre className="snippet">
{`export ANTHROPIC_BASE_URL=${baseUrl}
export ANTHROPIC_AUTH_TOKEN=<paste the token>
export ANTHROPIC_MODEL=sonnet-class`}
        </pre>
        <p className="field-hint">OpenAI style clients:</p>
        <pre className="snippet">
{`export OPENAI_BASE_URL=${baseUrl}/v1
export OPENAI_API_KEY=<paste the token>
# then request "opus-class", "sonnet-class", "haiku-class"
# or any exact id from the Models tab`}
        </pre>
        <div className="row gap">
          <Badge tone="neutral">{snapshot.exposed_ids.length} models exposed</Badge>
          <Button kind="ghost" onClick={() => api.copy(baseUrl)}>
            Copy base URL
          </Button>
          <Button kind="ghost" onClick={() => api.copyToken()}>
            Copy token
          </Button>
        </div>
      </Card>
    </>
  );
}


/**
 * What the last election decided, per class, with the numbers behind it.
 *
 * The reason a model sits where it does matters more than the score: a class scored
 * on latency alone because one member has no price is a fixable situation, and
 * saying so is the only way the user finds out.
 */
function ElectionResult({ election }: { election: Election | null }) {
  if (!election) {
    return (
      <Empty>
        No election has been held this run, so routing follows the priorities you set by hand.
      </Empty>
    );
  }

  const classes = CLASSES.map((cls) => election.classes[cls]).filter(
    (c): c is ClassElection => c !== undefined,
  );

  return (
    <>
      <div className="row gap wrap">
        <span className="muted">decided {new Date(election.decided_at).toLocaleString()}</span>
        <Badge tone="neutral">
          priced against {election.scoring.reference_input_tokens} in /{" "}
          {election.scoring.reference_output_tokens} out
        </Badge>
      </div>
      {classes.length === 0 && <Empty>No class had an enabled model to measure.</Empty>}
      {classes.map((outcome) => (
        <div key={outcome.class} className="subpanel">
          <div className="row gap wrap">
            <Badge tone={outcome.class}>{outcome.class}-class</Badge>
            {outcome.priced ? (
              <Badge tone="ok">latency and price</Badge>
            ) : (
              <Badge tone="warn">latency only</Badge>
            )}
            {outcome.note && <span className="muted">{outcome.note}</span>}
          </div>
          <table className="table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Latency</th>
                <th>Reference cost</th>
                <th title="Lower is better; 1.0 is the best in class">Score</th>
                <th>Why</th>
              </tr>
            </thead>
            <tbody>
              {outcome.ranked.map((r, place) => (
                <tr key={r.model_id} className={r.score === null ? "row-warn" : ""}>
                  <td>
                    {place === 0 && r.score !== null && <Badge tone="ok">primary</Badge>}{" "}
                    {r.model_id}
                  </td>
                  <td>{ms(r.latency_ms)}</td>
                  <td className={r.price ? "" : "muted"}>{costText(r.price)}</td>
                  <td>{r.score === null ? "—" : r.score.toFixed(3)}</td>
                  <td className="muted truncate" title={r.note ?? ""}>
                    {r.note}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ))}
    </>
  );
}
