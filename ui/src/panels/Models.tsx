import { Fragment, useState } from "react";
import {
  CLASSES,
  classMembers,
  modelRows,
  previewId,
  virtualId,
  type AppConfig,
  type ModelClass,
  type ModelEntry,
  type Snapshot,
} from "../api";
import {
  Badge,
  Banner,
  Button,
  Card,
  Empty,
  Field,
  NumberField,
  TextField,
  Toggle,
} from "../components";

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
  const rows = modelRows(snapshot);
  const [draft, setDraft] = useState({
    provider_id: config.providers[0]?.id ?? "",
    upstream_model: "",
    class: "" as ModelClass | "",
  });
  const [expanded, setExpanded] = useState<number | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const unclassified = rows.filter((r) => r.model.class === null);

  const update = (index: number, patch: Partial<ModelEntry>) => {
    const next = structuredClone(config);
    const model = next.models[index];
    if (!model) return;
    Object.assign(model, patch);
    void save(next);
  };

  const remove = (index: number) => {
    const next = structuredClone(config);
    next.models.splice(index, 1);
    void save(next);
  };

  const add = () => {
    const upstream = draft.upstream_model.trim();
    if (!draft.provider_id || !upstream) {
      setNotice("Pick a provider and fill in the model name.");
      return;
    }
    if (!draft.class) {
      setNotice("Choose a class. Zroutery never guesses which tier a model belongs to.");
      return;
    }
    // Identity is the provider plus the upstream name, so the same model coming
    // from a second provider is a separate entry with its own id.
    const clash = config.models.some(
      (m) => m.provider_id === draft.provider_id && m.upstream_model === upstream,
    );
    if (clash) {
      setNotice(`That provider already offers “${upstream}”.`);
      return;
    }
    const next = structuredClone(config);
    next.models.push({
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
    setDraft({ ...draft, upstream_model: "", class: "" });
    setNotice(null);
    void save(next);
  };

  const preview =
    draft.provider_id && draft.upstream_model.trim()
      ? previewId(draft.provider_id, draft.upstream_model)
      : null;

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
          <code>{unclassified.map((r) => r.id).join(", ")}</code>. They stay callable by their exact
          id, but they are excluded from <code>*-class</code> routing until you pick a tier.
        </Banner>
      )}

      <Card title="Exposed classes">
        <div className="grid-three">
          {CLASSES.map((cls) => {
            const members = classMembers(rows, config.providers, cls);
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
                    {members.map((r, i) => (
                      <li key={r.id}>
                        <span className="muted">{i === 0 ? "primary" : `fallback ${i}`}</span> {r.id}
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
          <Field label="Model name" hint="Exactly what the provider calls it">
            <input
              value={draft.upstream_model}
              placeholder="deepseek-chat"
              onChange={(e) => setDraft({ ...draft, upstream_model: e.currentTarget.value })}
              onKeyDown={(e) => e.key === "Enter" && add()}
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
          <Field label="Exposed as" hint="The provider prefix keeps duplicates apart">
            <input readOnly value={preview ?? ""} placeholder="<provider>-<model>" />
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
        {rows.length === 0 ? (
          <Empty>Nothing exposed yet.</Empty>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Exposed id</th>
                <th>Provider</th>
                <th>Model name</th>
                <th>Class</th>
                <th title="Lower wins inside a class">Priority</th>
                <th title="Random tie breaking among equal priorities">Weight</th>
                <th>On</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map(({ model: m, id, index }) => {
                const provider = config.providers.find((p) => p.id === m.provider_id);
                return (
                  <Fragment key={id}>
                    <tr className={m.class === null ? "row-warn" : ""}>
                      <td>
                        <button
                          className="linky"
                          onClick={() => setExpanded(expanded === index ? null : index)}
                        >
                          {id}
                        </button>
                        {m.aliases.length > 0 && (
                          <Badge tone="neutral">+{m.aliases.length} alias</Badge>
                        )}
                      </td>
                      <td>
                        {provider?.name ?? <Badge tone="danger">missing</Badge>}
                        {provider && !provider.enabled && <Badge tone="warn">off</Badge>}
                      </td>
                      <td className="muted">{m.upstream_model}</td>
                      <td>
                        <select
                          aria-label={`Class for ${id}`}
                          value={m.class ?? ""}
                          onChange={(e) =>
                            update(index, {
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
                          aria-label={`Priority for ${id}`}
                          value={m.priority}
                          onChange={(e) =>
                            update(index, { priority: Number(e.currentTarget.value) || 0 })
                          }
                        />
                      </td>
                      <td>
                        <input
                          className="tiny"
                          type="number"
                          min={1}
                          aria-label={`Weight for ${id}`}
                          value={m.weight}
                          onChange={(e) =>
                            update(index, { weight: Number(e.currentTarget.value) || 1 })
                          }
                        />
                      </td>
                      <td>
                        <input
                          type="checkbox"
                          aria-label={`Enable ${id}`}
                          checked={m.enabled}
                          onChange={(e) => update(index, { enabled: e.currentTarget.checked })}
                        />
                      </td>
                      <td>
                        <Button kind="ghost" onClick={() => remove(index)}>
                          Delete
                        </Button>
                      </td>
                    </tr>
                    {expanded === index && (
                      <tr>
                        <td colSpan={8}>
                          <div className="subpanel">
                            <div className="controls">
                              <TextField
                                label="Model name"
                                hint="Sent upstream; renaming it changes the exposed id"
                                value={m.upstream_model}
                                onCommit={(upstream_model) =>
                                  upstream_model.trim() && update(index, { upstream_model })
                                }
                              />
                              <TextField
                                label="Display name"
                                hint="Shown in /v1/models"
                                value={m.display_name ?? ""}
                                placeholder={m.upstream_model}
                                onCommit={(v) => update(index, { display_name: v || null })}
                              />
                              <TextField
                                label="Aliases"
                                hint="Comma separated short names that also reach this model"
                                value={m.aliases.join(", ")}
                                onCommit={(v) =>
                                  update(index, {
                                    aliases: v
                                      .split(",")
                                      .map((a) => a.trim())
                                      .filter(Boolean),
                                  })
                                }
                              />
                              <NumberField
                                label="Max output tokens"
                                hint="Caps what clients may ask for"
                                min={1}
                                placeholder="unlimited"
                                value={m.max_output_tokens}
                                onCommit={(max_output_tokens) =>
                                  update(index, { max_output_tokens })
                                }
                              />
                            </div>
                            <div className="grid-three">
                              <Toggle
                                label="Tool use"
                                checked={m.supports_tools}
                                onChange={(v) => update(index, { supports_tools: v })}
                              />
                              <Toggle
                                label="Vision"
                                checked={m.supports_vision}
                                onChange={(v) => update(index, { supports_vision: v })}
                              />
                              <Toggle
                                label="Extended thinking"
                                checked={m.supports_thinking}
                                onChange={(v) => update(index, { supports_thinking: v })}
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

