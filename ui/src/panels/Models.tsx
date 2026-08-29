import { Fragment, useState } from "react";
import {
  CLASSES,
  classMembers,
  emptyPricing,
  modelRows,
  previewId,
  priceText,
  virtualId,
  type AppConfig,
  type ModelClass,
  type ModelEntry,
  type Pricing,
  type Snapshot,
} from "../api";
import {
  Badge,
  Banner,
  Button,
  Card,
  CompactNumber,
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
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
}) {
  const { config } = snapshot;
  const rows = modelRows(snapshot);
  const [draft, setDraft] = useState({
    provider_id: config.providers[0]?.id ?? "",
    upstream_model: "",
    class: "" as ModelClass | "",
  });
  const [expanded, setExpanded] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const unclassified = rows.filter((r) => r.model.class === null);

  const update = (id: string, patch: Partial<ModelEntry>) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const model = next.models.find(
        (m) => previewId(m.provider_id, m.upstream_model) === id,
      );
      if (!model) return null;
      Object.assign(model, patch);
      return next;
    });
  };

  const remove = (id: string) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const index = next.models.findIndex(
        (m) => previewId(m.provider_id, m.upstream_model) === id,
      );
      if (index < 0) return null;
      next.models.splice(index, 1);
      return next;
    });
  };

  const add = () => {
    const upstream = draft.upstream_model.trim();
    // Narrowed outside the save updater: TS cannot narrow state fields inside
    // a closure that may run later.
    const modelClass = draft.class;
    const providerId = draft.provider_id;
    if (!providerId || !upstream) {
      setNotice("Pick a provider and fill in the model name.");
      return;
    }
    if (!modelClass) {
      setNotice(
        "Choose a class. Zroutery never guesses which tier a model belongs to.",
      );
      return;
    }
    // Identity is the provider plus the upstream name, so the same model coming
    // from a second provider is a separate entry with its own id.
    const clash = config.models.some(
      (m) =>
        m.provider_id === draft.provider_id && m.upstream_model === upstream,
    );
    if (clash) {
      setNotice(`That provider already offers “${upstream}”.`);
      return;
    }
    void save((cfg) => {
      const next = structuredClone(cfg);
      next.models.push({
        provider_id: providerId,
        upstream_model: upstream,
        class: modelClass,
        priority: 0,
        weight: 1,
        enabled: true,
        supports_tools: true,
        supports_vision: false,
        supports_thinking: false,
        display_name: null,
        aliases: [],
        max_output_tokens: null,
        // Prices are typed in on the row, like the class: never guessed.
        pricing: null,
      });
      return next;
    });
    setDraft({ ...draft, upstream_model: "", class: "" });
    setNotice(null);
  };

  const preview =
    draft.provider_id && draft.upstream_model.trim()
      ? previewId(draft.provider_id, draft.upstream_model)
      : null;

  return (
    <>
      {notice && (
        <Banner
          tone="warn"
          actions={
            <Button kind="ghost" onClick={() => setNotice(null)}>
              OK
            </Button>
          }
        >
          {notice}
        </Banner>
      )}

      {unclassified.length > 0 && (
        <Banner tone="warn">
          {unclassified.length} model{unclassified.length === 1 ? "" : "s"} have
          no class yet: <code>{unclassified.map((r) => r.id).join(", ")}</code>.
          They stay callable by their exact id, but they are excluded from{" "}
          <code>*-class</code> routing until you pick a tier.
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
                    Empty — requests to <code>{virtualId(cls)}</code> will fail
                    with 503.
                  </p>
                ) : (
                  <ol className="member-list">
                    {members.map((r, i) => (
                      <li key={r.id}>
                        <span className="muted">
                          {i === 0 ? "primary" : `fallback ${i}`}
                        </span>{" "}
                        {r.id}
                        {r.model.pricing && (
                          <span className="muted">
                            {" "}
                            · {priceText(r.model.pricing)}
                          </span>
                        )}
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
              onChange={(e) =>
                setDraft({ ...draft, provider_id: e.currentTarget.value })
              }
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
              onChange={(e) =>
                setDraft({ ...draft, upstream_model: e.currentTarget.value })
              }
              onKeyDown={(e) => e.key === "Enter" && add()}
            />
          </Field>
          <Field label="Class" hint="Required">
            <select
              value={draft.class}
              onChange={(e) =>
                setDraft({
                  ...draft,
                  class: e.currentTarget.value as ModelClass,
                })
              }
            >
              <option value="">— choose —</option>
              {CLASSES.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </Field>
          <Field
            label="Exposed as"
            hint="The provider prefix keeps duplicates apart"
          >
            <input
              readOnly
              value={preview ?? ""}
              placeholder="<provider>-<model>"
            />
          </Field>
          <div className="field-actions">
            <Button
              kind="primary"
              onClick={add}
              disabled={busy || !config.providers.length}
            >
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
                <th title="Per million tokens, input / output">Price</th>
                <th title="Lower wins inside a class">Priority</th>
                <th title="Random tie breaking among equal priorities">
                  Weight
                </th>
                <th>On</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map(({ model: m, id }) => {
                const provider = config.providers.find(
                  (p) => p.id === m.provider_id,
                );
                return (
                  <Fragment key={id}>
                    <tr className={m.class === null ? "row-warn" : ""}>
                      <td>
                        <button
                          className="linky"
                          onClick={() =>
                            setExpanded(expanded === id ? null : id)
                          }
                        >
                          {id}
                        </button>
                        {m.aliases.length > 0 && (
                          <Badge tone="neutral">
                            +{m.aliases.length} alias
                          </Badge>
                        )}
                      </td>
                      <td>
                        {provider?.name ?? <Badge tone="danger">missing</Badge>}
                        {provider && !provider.enabled && (
                          <Badge tone="warn">off</Badge>
                        )}
                      </td>
                      <td className="muted">{m.upstream_model}</td>
                      <td>
                        <select
                          aria-label={`Class for ${id}`}
                          value={m.class ?? ""}
                          onChange={(e) =>
                            update(id, {
                              class: (e.currentTarget.value ||
                                null) as ModelClass | null,
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
                        {/* The editor lives in the row detail, so the cell itself
                            opens it: an empty column is otherwise a dead end. */}
                        <button
                          className="linky"
                          onClick={() =>
                            setExpanded(expanded === id ? null : id)
                          }
                          title={
                            m.pricing
                              ? "Edit the price"
                              : "No price yet, so requests are logged without a cost"
                          }
                        >
                          {m.pricing ? priceText(m.pricing) : "set price"}
                        </button>
                      </td>
                      <td>
                        <CompactNumber
                          ariaLabel={`Priority for ${id}`}
                          value={m.priority}
                          min={0}
                          integer
                          onCommit={(priority) =>
                            update(id, { priority: priority ?? 0 })
                          }
                        />
                      </td>
                      <td>
                        <CompactNumber
                          ariaLabel={`Weight for ${id}`}
                          value={m.weight}
                          min={1}
                          integer
                          onCommit={(weight) =>
                            update(id, { weight: weight ?? 1 })
                          }
                        />
                      </td>
                      <td>
                        <input
                          type="checkbox"
                          aria-label={`Enable ${id}`}
                          checked={m.enabled}
                          onChange={(e) =>
                            update(id, { enabled: e.currentTarget.checked })
                          }
                        />
                      </td>
                      <td>
                        <Button kind="ghost" onClick={() => remove(id)}>
                          Delete
                        </Button>
                      </td>
                    </tr>
                    {expanded === id && (
                      <tr>
                        <td colSpan={9}>
                          <div className="subpanel">
                            <div className="controls">
                              <TextField
                                label="Model name"
                                hint="Sent upstream; renaming it changes the exposed id"
                                value={m.upstream_model}
                                onCommit={(upstream_model) =>
                                  upstream_model.trim() &&
                                  update(id, { upstream_model })
                                }
                              />
                              <TextField
                                label="Display name"
                                hint="Shown in /v1/models"
                                value={m.display_name ?? ""}
                                placeholder={m.upstream_model}
                                onCommit={(v) =>
                                  update(id, { display_name: v || null })
                                }
                              />
                              <TextField
                                label="Aliases"
                                hint="Comma separated short names that also reach this model"
                                value={m.aliases.join(", ")}
                                onCommit={(v) =>
                                  update(id, {
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
                                  update(id, { max_output_tokens })
                                }

                                integer
                              />
                            </div>
                            <h3>Price per million tokens</h3>
                            <p className="field-hint">
                              In the currency the provider bills in. Leave it
                              off and requests are logged without a cost rather
                              than with a guessed one.
                            </p>
                            <PriceFields
                              id={id}
                              pricing={m.pricing}
                              onChange={(pricing) => update(id, { pricing })}
                            />
                            <div className="grid-three">
                              <Toggle
                                label="Tool use"
                                checked={m.supports_tools}
                                onChange={(v) =>
                                  update(id, { supports_tools: v })
                                }
                              />
                              <Toggle
                                label="Vision"
                                checked={m.supports_vision}
                                onChange={(v) =>
                                  update(id, { supports_vision: v })
                                }
                              />
                              <Toggle
                                label="Extended thinking"
                                checked={m.supports_thinking}
                                onChange={(v) =>
                                  update(id, { supports_thinking: v })
                                }
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

/**
 * The four numbers that make up a price. Clearing input and output removes the
 * price entirely, which is how a model goes back to being unpriced.
 */
function PriceFields({
  id,
  pricing,
  onChange,
}: {
  id: string;
  pricing: Pricing | null;
  onChange: (pricing: Pricing | null) => void;
}) {
  const current = pricing ?? emptyPricing();

  const patch = (change: Partial<Pricing>) => {
    const next = { ...current, ...change };
    const priced =
      next.input_per_mtok > 0 ||
      next.output_per_mtok > 0 ||
      next.cache_read_per_mtok !== null ||
      next.cache_write_per_mtok !== null;
    onChange(priced ? next : null);
  };

  return (
    <div className="controls">
      <TextField
        label="Currency"
        hint="USD, CNY, …"
        value={current.currency}
        onCommit={(currency) =>
          patch({ currency: currency.trim().toUpperCase() || "USD" })
        }
      />
      <NumberField
        label="Input"
        hint={`per 1M prompt tokens for ${id}`}
        min={0}
        value={current.input_per_mtok}
        onCommit={(v) => patch({ input_per_mtok: v ?? 0 })}
      />
      <NumberField
        label="Output"
        hint="per 1M completion tokens"
        min={0}
        value={current.output_per_mtok}
        onCommit={(v) => patch({ output_per_mtok: v ?? 0 })}
      />
      <NumberField
        label="Cache read"
        hint="optional, defaults to the input price"
        min={0}
        placeholder="same as input"
        value={current.cache_read_per_mtok}
        onCommit={(v) => patch({ cache_read_per_mtok: v })}
      />
      <NumberField
        label="Cache write"
        hint="optional"
        min={0}
        placeholder="not billed"
        value={current.cache_write_per_mtok}
        onCommit={(v) => patch({ cache_write_per_mtok: v })}
      />
      {pricing && (
        <div className="field-actions">
          <Button kind="ghost" onClick={() => onChange(null)}>
            Clear price
          </Button>
        </div>
      )}
    </div>
  );
}
