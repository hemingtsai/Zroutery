import { Fragment, useState } from "react";
import {
  CLASSES,
  classMembers,
  virtualId,
  type AppConfig,
  type ModelClass,
  type ModelEntry,
  type Snapshot,
} from "../api";
import { Badge, Banner, Button, Card, Empty, Field, Toggle } from "../components";

const CLASS_HINT: Record<ModelClass, string> = {
  opus: "Your strongest, most expensive model",
  sonnet: "The everyday workhorse",
  haiku: "Cheap and fast",
};

export default function Models({
  snapshot,
  save,
  busy,
}: {
  snapshot: Snapshot;
  save: (config: AppConfig) => Promise<void>;
  busy: boolean;
}) {
  const { config } = snapshot;
  const [draft, setDraft] = useState({
    provider_id: config.providers[0]?.id ?? "",
    upstream_model: "",
    id: "",
    class: "" as ModelClass | "",
  });
  const [expanded, setExpanded] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const unclassified = config.models.filter((m) => m.class === null);

  const update = (id: string, patch: Partial<ModelEntry>) => {
    const next = structuredClone(config);
    const model = next.models.find((m) => m.id === id);
    if (!model) return;
    Object.assign(model, patch);
    void save(next);
  };

  const remove = (id: string) => {
    const next = structuredClone(config);
    next.models = next.models.filter((m) => m.id !== id);
    void save(next);
  };

  const add = () => {
    const upstream = draft.upstream_model.trim();
    const exposed = (draft.id || upstream).trim();
    if (!draft.provider_id || !upstream || !exposed) {
      setNotice("Pick a provider and fill in the model id.");
      return;
    }
    if (!draft.class) {
      setNotice("Choose a class. Zroutery never guesses which tier a model belongs to.");
      return;
    }
    if (config.models.some((m) => m.id === exposed)) {
      setNotice(`“${exposed}” is already exposed.`);
      return;
    }
    const next = structuredClone(config);
    next.models.push({
      id: exposed,
      provider_id: draft.provider_id,
      upstream_model: upstream,
      class: draft.class,
      priority: 0,
      weight: 1,
      enabled: true,
      supports_tools: true,
      supports_vision: false,
      supports_thinking: false,
      display_name: null,
      aliases: [],
      max_output_tokens: null,
    });
    setDraft({ ...draft, upstream_model: "", id: "", class: "" });
    setNotice(null);
    void save(next);
  };

  return (
    <>
      {notice && (
        <Banner tone="warn" actions={<Button kind="ghost" onClick={() => setNotice(null)}>OK</Button>}>
          {notice}
        </Banner>
      )}

      {unclassified.length > 0 && (
        <Banner tone="warn">
          {unclassified.length} model{unclassified.length === 1 ? "" : "s"} have no class yet:{" "}
          <code>{unclassified.map((m) => m.id).join(", ")}</code>. They stay callable by their exact
          id, but they are excluded from <code>*-class</code> routing until you pick a tier.
        </Banner>
      )}

      <Card title="Exposed classes">
        <div className="grid-three">
          {CLASSES.map((cls) => {
            const members = classMembers(config, cls);
            return (
              <div key={cls} className="class-card">
                <div className="row gap">
                  <Badge tone={cls}>{virtualId(cls)}</Badge>
                  <span className="muted">{CLASS_HINT[cls]}</span>
                </div>
                {members.length === 0 ? (
                  <p className="empty small">
                    Empty — requests to <code>{virtualId(cls)}</code> will fail with 503.
                  </p>
                ) : (
                  <ol className="member-list">
                    {members.map((m, i) => (
                      <li key={m.id}>
                        <span className="muted">{i === 0 ? "primary" : `fallback ${i}`}</span>{" "}
                        {m.id}
                      </li>
                    ))}
                  </ol>
                )}
              </div>
            );
          })}
        </div>
      </Card>

      <Card title="Add a model">
        <div className="controls">
          <Field label="Provider">
            <select
              value={draft.provider_id}
              onChange={(e) => setDraft({ ...draft, provider_id: e.currentTarget.value })}
            >
              {config.providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Upstream model id" hint="Exactly what the provider calls it">
            <input
              value={draft.upstream_model}
              placeholder="deepseek-v4-pro"
              onChange={(e) => setDraft({ ...draft, upstream_model: e.currentTarget.value })}
            />
          </Field>
          <Field label="Exposed as" hint="Defaults to the upstream id">
            <input
              value={draft.id}
              placeholder="(same)"
              onChange={(e) => setDraft({ ...draft, id: e.currentTarget.value })}
            />
          </Field>
          <Field label="Class" hint="Required">
            <select
              value={draft.class}
              onChange={(e) => setDraft({ ...draft, class: e.currentTarget.value as ModelClass })}
            >
              <option value="">— choose —</option>
              {CLASSES.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </Field>
          <div className="field-actions">
            <Button kind="primary" onClick={add} disabled={busy || !config.providers.length}>
              Add model
            </Button>
          </div>
        </div>
        {!config.providers.length && <Empty>Add a provider first.</Empty>}
      </Card>

      <Card title="Model registry">
        {config.models.length === 0 ? (
          <Empty>Nothing exposed yet.</Empty>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Exposed id</th>
                <th>Provider</th>
                <th>Upstream</th>
                <th>Class</th>
                <th title="Lower wins inside a class">Priority</th>
                <th title="Random tie breaking among equal priorities">Weight</th>
                <th>On</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {config.models.map((m) => {
                const provider = config.providers.find((p) => p.id === m.provider_id);
                return (
                  <Fragment key={m.id}>
                    <tr className={m.class === null ? "row-warn" : ""}>
                      <td>
                        <button className="linky" onClick={() => setExpanded(expanded === m.id ? null : m.id)}>
                          {m.id}
                        </button>
                      </td>
                      <td>
                        {provider?.name ?? <Badge tone="danger">missing</Badge>}
                        {provider && !provider.enabled && <Badge tone="warn">off</Badge>}
                      </td>
                      <td className="muted">{m.upstream_model}</td>
                      <td>
                        <select
                          aria-label={`Class for ${m.id}`}
                          value={m.class ?? ""}
                          onChange={(e) =>
                            update(m.id, {
                              class: (e.currentTarget.value || null) as ModelClass | null,
                            })
                          }
                        >
                          <option value="">— unset —</option>
                          {CLASSES.map((c) => (
                            <option key={c} value={c}>
                              {c}
                            </option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <input
                          className="tiny"
                          type="number"
                          aria-label={`Priority for ${m.id}`}
                          value={m.priority}
                          onChange={(e) => update(m.id, { priority: Number(e.currentTarget.value) || 0 })}
                        />
                      </td>
                      <td>
                        <input
                          className="tiny"
                          type="number"
                          min={1}
                          aria-label={`Weight for ${m.id}`}
                          value={m.weight}
                          onChange={(e) => update(m.id, { weight: Number(e.currentTarget.value) || 1 })}
                        />
                      </td>
                      <td>
                        <input
                          type="checkbox"
                          aria-label={`Enable ${m.id}`}
                          checked={m.enabled}
                          onChange={(e) => update(m.id, { enabled: e.currentTarget.checked })}
                        />
                      </td>
                      <td>
                        <Button kind="ghost" onClick={() => remove(m.id)}>
                          Delete
                        </Button>
                      </td>
                    </tr>
                    {expanded === m.id && (
                      <tr>
                        <td colSpan={8}>
                          <div className="subpanel">
                            <div className="controls">
                              <Field label="Display name">
                                <input
                                  value={m.display_name ?? ""}
                                  placeholder={m.id}
                                  onChange={(e) =>
                                    update(m.id, { display_name: e.currentTarget.value || null })
                                  }
                                />
                              </Field>
                              <Field label="Aliases" hint="Comma separated, resolve to this exact model">
                                <input
                                  value={m.aliases.join(", ")}
                                  onChange={(e) =>
                                    update(m.id, {
                                      aliases: e.currentTarget.value
                                        .split(",")
                                        .map((a) => a.trim())
                                        .filter(Boolean),
                                    })
                                  }
                                />
                              </Field>
                              <Field label="Max output tokens" hint="Caps what clients may ask for">
                                <input
                                  type="number"
                                  min={1}
                                  value={m.max_output_tokens ?? ""}
                                  placeholder="unlimited"
                                  onChange={(e) =>
                                    update(m.id, {
                                      max_output_tokens: Number(e.currentTarget.value) || null,
                                    })
                                  }
                                />
                              </Field>
                            </div>
                            <div className="grid-three">
                              <Toggle
                                label="Tool use"
                                checked={m.supports_tools}
                                onChange={(v) => update(m.id, { supports_tools: v })}
                              />
                              <Toggle
                                label="Vision"
                                checked={m.supports_vision}
                                onChange={(v) => update(m.id, { supports_vision: v })}
                              />
                              <Toggle
                                label="Extended thinking"
                                checked={m.supports_thinking}
                                onChange={(v) => update(m.id, { supports_thinking: v })}
                              />
                            </div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        )}
      </Card>
    </>
  );
}
