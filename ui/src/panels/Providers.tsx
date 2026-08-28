import { useState } from "react";
import {
  api,
  BALANCE_PRESETS,
  defaultProbe,
  errorText,
  money,
  previewId,
  slugify,
  type AppConfig,
  type BalancePreset,
  type BalanceStatus,
  type DiscoveredModel,
  type Provider,
  type ProviderKind,
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

const KINDS: { id: ProviderKind; label: string; hint: string }[] = [
  {
    id: "openai_compatible",
    label: "OpenAI compatible",
    hint: "OpenAI, DeepSeek, Groq, OpenRouter, Sub2API, Ollama, vLLM…",
  },
  { id: "anthropic", label: "Anthropic", hint: "api.anthropic.com, Sub2API and other gateways" },
];

function defaultBaseUrl(kind: ProviderKind): string {
  return kind === "anthropic" ? "https://api.anthropic.com" : "https://api.openai.com/v1";
}

function blankProvider(
  kind: ProviderKind,
  name: string,
  baseUrl: string,
  preset: BalancePreset,
): Provider {
  const id = slugify(name);
  return {
    id,
    name: name.trim(),
    kind,
    base_url: baseUrl.trim() || defaultBaseUrl(kind),
    key_ref: `provider:${id}`,
    extra_headers: {},
    enabled: true,
    timeout_secs: 600,
    connect_timeout_secs: 15,
    anthropic_version: null,
    // The custom preset needs a probe to edit; every other one carries its own.
    balance: { preset, custom: preset === "custom" ? defaultProbe() : null },
    quirks: {
      use_max_completion_tokens: false,
      drop_temperature: false,
      drop_top_p: false,
      drop_stop: false,
      stream_usage: true,
      system_as_developer: false,
      send_reasoning_effort: false,
    },
  };
}

export default function Providers({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (config: AppConfig) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, keys } = snapshot;
  const [newName, setNewName] = useState("");
  const [newKind, setNewKind] = useState<ProviderKind>("openai_compatible");
  const [newBaseUrl, setNewBaseUrl] = useState("");
  const [newPreset, setNewPreset] = useState<BalancePreset>("none");
  const [editing, setEditing] = useState<string | null>(null);
  const [keyDraft, setKeyDraft] = useState<Record<string, string>>({});
  const [discovered, setDiscovered] = useState<Record<string, DiscoveredModel[]>>({});
  const [notice, setNotice] = useState<string | null>(null);

  const update = (id: string, patch: Partial<Provider>) => {
    const next = structuredClone(config);
    const provider = next.providers.find((p) => p.id === id);
    if (!provider) return;
    Object.assign(provider, patch);
    void save(next);
  };

  const addProvider = () => {
    const name = newName.trim();
    if (!name) return;
    const provider = blankProvider(newKind, name, newBaseUrl, newPreset);
    if (config.providers.some((p) => p.id === provider.id)) {
      setNotice(`A provider called “${provider.id}” already exists.`);
      return;
    }
    const next = structuredClone(config);
    next.providers.push(provider);
    setNewName("");
    setNewBaseUrl("");
    setNewPreset("none");
    setEditing(provider.id);
    void save(next);
  };

  const removeProvider = async (id: string) => {
    const models = config.models.filter((m) => m.provider_id === id);
    const next = structuredClone(config);
    next.providers = next.providers.filter((p) => p.id !== id);
    next.models = next.models.filter((m) => m.provider_id !== id);
    await save(next);
    // Also delete any stored API key for this provider from the keychain.
    await run(() => api.clearKey(id));
    if (models.length) {
      setNotice(`Removed ${models.length} model(s) that belonged to that provider.`);
    }
  };

  const saveKey = async (provider: Provider) => {
    const value = (keyDraft[provider.id] ?? "").trim();
    if (!value) return;
    const ok = await run(() => api.setKey(provider.id, value));
    // Keep the draft when the save failed, so a long key does not have to be
    // typed twice just because the keychain hiccupped.
    if (ok) setKeyDraft({ ...keyDraft, [provider.id]: "" });
  };

  const discover = async (provider: Provider) => {
    setNotice(null);
    try {
      const ids = await api.fetchModels(provider);
      setDiscovered({ ...discovered, [provider.id]: ids });
      if (!ids.length) setNotice(`${provider.name} returned an empty model list.`);
    } catch (e) {
      setNotice(errorText(e));
    }
  };

  const addDiscovered = (provider: Provider, model: DiscoveredModel) => {
    const modelName = model.id;
    // Only the same provider offering the same model is a duplicate; the same
    // name coming from another provider gets its own prefixed id.
    if (
      config.models.some((m) => m.provider_id === provider.id && m.upstream_model === modelName)
    ) {
      setNotice(`“${modelName}” is already listed for ${provider.name}.`);
      return;
    }
    const next = structuredClone(config);
    next.models.push({
      provider_id: provider.id,
      upstream_model: modelName,
      // Left unset on purpose: classes are always assigned by hand.
      class: null,
      priority: 0,
      weight: 1,
      enabled: true,
      supports_tools: true,
      supports_vision: false,
      supports_thinking: false,
      display_name: null,
      aliases: [],
      max_output_tokens: null,
      // Prefilled when the catalogue publishes prices; still editable.
      pricing: model.pricing,
    });
    void save(next);
    setNotice(
      model.pricing
        ? `Added “${previewId(provider.id, modelName)}” with the catalogue price. Assign it a class on the Models tab.`
        : `Added “${previewId(provider.id, modelName)}”. Assign it a class on the Models tab.`,
    );
  };

  return (
    <>
      {notice && (
        <Banner tone="info" actions={<Button kind="ghost" onClick={() => setNotice(null)}>OK</Button>}>
          {notice}
        </Banner>
      )}

      <Card
        title="Add a provider"
        actions={
          config.providers.some((p) => p.balance.preset !== "none") ? (
            <Button kind="ghost" onClick={() => run(api.refreshBalances)} disabled={busy}>
              Check all balances
            </Button>
          ) : undefined
        }
      >
        <div className="controls">
          <Field label="Name" hint="Shown in logs and the model list">
            <input
              value={newName}
              placeholder="DeepSeek"
              onChange={(e) => setNewName(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && addProvider()}
            />
          </Field>
          <Field label="API dialect" hint={KINDS.find((k) => k.id === newKind)?.hint}>
            <select value={newKind} onChange={(e) => setNewKind(e.currentTarget.value as ProviderKind)}>
              {KINDS.map((k) => (
                <option key={k.id} value={k.id}>
                  {k.label}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Base URL" wide hint="Leave empty for the vendor default">
            <input
              value={newBaseUrl}
              placeholder={defaultBaseUrl(newKind)}
              onChange={(e) => setNewBaseUrl(e.currentTarget.value)}
              onKeyDown={(e) => e.key === "Enter" && addProvider()}
            />
          </Field>
          <Field
            label="Balance endpoint"
            hint={BALANCE_PRESETS.find((p) => p.id === newPreset)?.hint}
          >
            <select
              value={newPreset}
              onChange={(e) => setNewPreset(e.currentTarget.value as BalancePreset)}
            >
              {BALANCE_PRESETS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </Field>
          <div className="field-actions">
            <Button kind="primary" onClick={addProvider} disabled={busy || !newName.trim()}>
              Add provider
            </Button>
          </div>
        </div>
        {newPreset === "sub2api" && !newBaseUrl.trim() && (
          <p className="field-hint">
            A Sub2API relay is self hosted, so fill in its own base URL: the OpenAI dialect wants
            the <code>/v1</code> root, the Anthropic dialect the host on its own.
          </p>
        )}
      </Card>

      {config.providers.length === 0 && (
        <Empty>No providers yet. Add DeepSeek or OpenAI above, then paste an API key.</Empty>
      )}

      {config.providers.map((provider) => {
        const hasKey = keys[provider.id];
        const models = config.models.filter((m) => m.provider_id === provider.id);
        const open = editing === provider.id;
        return (
          <Card
            key={provider.id}
            tone={!hasKey ? "warn" : undefined}
            title={
              <span className="row gap">
                {provider.name}
                <Badge>{provider.kind === "anthropic" ? "Anthropic API" : "OpenAI API"}</Badge>
                {hasKey ? <Badge tone="ok">key stored</Badge> : <Badge tone="warn">no key</Badge>}
                {!provider.enabled && <Badge tone="danger">disabled</Badge>}
              </span>
            }
            actions={
              <>
                <Button kind="ghost" onClick={() => discover(provider)}>
                  Fetch models
                </Button>
                <Button kind="ghost" onClick={() => setEditing(open ? null : provider.id)}>
                  {open ? "Done" : "Settings"}
                </Button>
                <Button kind="danger" onClick={() => removeProvider(provider.id)}>
                  Remove
                </Button>
              </>
            }
          >
            <div className="controls">
              <TextField
                label="Base URL"
                wide
                hint="Everything before /chat/completions or /v1/messages"
                value={provider.base_url}
                onCommit={(base_url) => update(provider.id, { base_url })}
              />
              <Field label="API key" hint={hasKey ? "Stored in the macOS keychain" : "Required"}>
                <input
                  type="password"
                  autoComplete="off"
                  placeholder={hasKey ? "••••••••••••" : "sk-…"}
                  value={keyDraft[provider.id] ?? ""}
                  onChange={(e) => setKeyDraft({ ...keyDraft, [provider.id]: e.currentTarget.value })}
                  onKeyDown={(e) => e.key === "Enter" && saveKey(provider)}
                />
              </Field>
              <div className="field-actions">
                <Button onClick={() => saveKey(provider)} disabled={!(keyDraft[provider.id] ?? "").trim()}>
                  Save key
                </Button>
                {hasKey && (
                  <Button kind="ghost" onClick={() => run(() => api.clearKey(provider.id))}>
                    Remove key
                  </Button>
                )}
              </div>
            </div>

            <div className="row gap wrap">
              <Toggle
                label="Enabled"
                checked={provider.enabled}
                onChange={(enabled) => update(provider.id, { enabled })}
              />
              <span className="muted">
                {models.length} model{models.length === 1 ? "" : "s"}
              </span>
              <BalanceRow
                provider={provider}
                status={snapshot.balances[provider.id]}
                busy={busy}
                onPreset={(preset) =>
                  update(provider.id, {
                    balance: {
                      preset,
                      custom:
                        preset === "custom"
                          ? (provider.balance.custom ?? defaultProbe())
                          : null,
                    },
                  })
                }
                onCheck={() => run(() => api.refreshBalance(provider.id))}
              />
            </div>

            {provider.balance.preset === "custom" && provider.balance.custom && (
              <div className="controls">
                <TextField
                  label="Balance path"
                  hint="Appended to the base URL, or an absolute URL"
                  value={provider.balance.custom.path}
                  onCommit={(path) =>
                    update(provider.id, {
                      balance: {
                        preset: "custom",
                        custom: { ...provider.balance.custom!, path },
                      },
                    })
                  }
                />
                <TextField
                  label="Remaining pointer"
                  hint="JSON pointer, e.g. /data/balance"
                  value={provider.balance.custom.remaining_pointer ?? ""}
                  onCommit={(v) =>
                    update(provider.id, {
                      balance: {
                        preset: "custom",
                        custom: { ...provider.balance.custom!, remaining_pointer: v || null },
                      },
                    })
                  }
                />
                <TextField
                  label="Currency"
                  hint="Used when the payload carries none"
                  value={provider.balance.custom.currency ?? ""}
                  onCommit={(v) =>
                    update(provider.id, {
                      balance: {
                        preset: "custom",
                        custom: { ...provider.balance.custom!, currency: v || null },
                      },
                    })
                  }
                />
              </div>
            )}

            {open && (
              <div className="subpanel">
                <div className="controls">
                  <NumberField
                    label="Request timeout (s)"
                    min={5}
                    value={provider.timeout_secs}
                    onCommit={(v) => update(provider.id, { timeout_secs: v ?? 600 })}
                  />
                  <NumberField
                    label="Connect timeout (s)"
                    min={1}
                    value={provider.connect_timeout_secs}
                    onCommit={(v) => update(provider.id, { connect_timeout_secs: v ?? 15 })}
                  />
                  {provider.kind === "anthropic" && (
                    <TextField
                      label="anthropic-version"
                      hint="Leave empty for 2023-06-01"
                      value={provider.anthropic_version ?? ""}
                      onCommit={(v) => update(provider.id, { anthropic_version: v || null })}
                    />
                  )}
                </div>

                {provider.kind === "openai_compatible" && (
                  <>
                    <h3>Compatibility</h3>
                    <p className="field-hint">
                      Reasoning models such as the gpt-5 family reject <code>max_tokens</code> and{" "}
                      <code>temperature</code>. Turn these on if the provider complains.
                    </p>
                    <div className="grid-two">
                      <Toggle
                        label="Send max_completion_tokens"
                        checked={provider.quirks.use_max_completion_tokens}
                        onChange={(v) =>
                          update(provider.id, {
                            quirks: { ...provider.quirks, use_max_completion_tokens: v },
                          })
                        }
                      />
                      <Toggle
                        label="Drop temperature"
                        checked={provider.quirks.drop_temperature}
                        onChange={(v) =>
                          update(provider.id, {
                            quirks: { ...provider.quirks, drop_temperature: v },
                          })
                        }
                      />
                      <Toggle
                        label="Drop top_p"
                        checked={provider.quirks.drop_top_p}
                        onChange={(v) =>
                          update(provider.id, { quirks: { ...provider.quirks, drop_top_p: v } })
                        }
                      />
                      <Toggle
                        label="Drop stop sequences"
                        checked={provider.quirks.drop_stop}
                        onChange={(v) =>
                          update(provider.id, { quirks: { ...provider.quirks, drop_stop: v } })
                        }
                      />
                      <Toggle
                        label="Ask for stream usage"
                        checked={provider.quirks.stream_usage}
                        onChange={(v) =>
                          update(provider.id, { quirks: { ...provider.quirks, stream_usage: v } })
                        }
                      />
                      <Toggle
                        label="System prompt as developer role"
                        checked={provider.quirks.system_as_developer}
                        onChange={(v) =>
                          update(provider.id, {
                            quirks: { ...provider.quirks, system_as_developer: v },
                          })
                        }
                      />
                      <Toggle
                        label="Map thinking to reasoning_effort"
                        checked={provider.quirks.send_reasoning_effort}
                        onChange={(v) =>
                          update(provider.id, {
                            quirks: { ...provider.quirks, send_reasoning_effort: v },
                          })
                        }
                      />
                    </div>
                  </>
                )}
              </div>
            )}

            {discovered[provider.id]?.length > 0 && (
              <div className="subpanel">
                <h3>Models reported by {provider.name}</h3>
                <div className="chips">
                  {discovered[provider.id].map((model) => {
                    const name = model.id;
                    const already = config.models.some(
                      (m) => m.provider_id === provider.id && m.upstream_model === name,
                    );
                    return (
                      <button
                        key={name}
                        className={`chip ${already ? "chip-done" : ""}`}
                        disabled={already}
                        onClick={() => addDiscovered(provider, model)}
                        title={
                          already
                            ? "Already added"
                            : model.pricing
                              ? `Add as ${previewId(provider.id, name)}, priced ${model.pricing.input_per_mtok}/${model.pricing.output_per_mtok} ${model.pricing.currency} per Mtok`
                              : `Add as ${previewId(provider.id, name)}`
                        }
                      >
                        {name}
                        {model.pricing ? " ¤" : ""}
                        {already ? " ✓" : " +"}
                      </button>
                    );
                  })}
                </div>
              </div>
            )}
          </Card>
        );
      })}
    </>
  );
}

/**
 * The balance line of a provider card: which endpoint to ask, the last answer,
 * and a button to ask again.
 *
 * Nothing polls this. Balance endpoints cost a request and some vendors rate
 * limit them, so it is refreshed only when asked.
 */
function BalanceRow({
  provider,
  status,
  busy,
  onPreset,
  onCheck,
}: {
  provider: Provider;
  status: BalanceStatus | undefined;
  busy: boolean;
  onPreset: (preset: BalancePreset) => void;
  onCheck: () => void;
}) {
  const supported =
    provider.balance.preset !== "none" &&
    (provider.balance.preset !== "custom" || provider.balance.custom !== null);

  return (
    <span className="row gap wrap">
      <span className="field-label">Balance</span>
      <select
        aria-label={`Balance endpoint for ${provider.name}`}
        title={BALANCE_PRESETS.find((p) => p.id === provider.balance.preset)?.hint}
        value={provider.balance.preset}
        onChange={(e) => onPreset(e.currentTarget.value as BalancePreset)}
      >
        {BALANCE_PRESETS.map((p) => (
          <option key={p.id} value={p.id}>
            {p.label}
          </option>
        ))}
      </select>
      {supported && (
        <Button kind="ghost" onClick={onCheck} disabled={busy}>
          Check
        </Button>
      )}
      {status?.balance && (
        <Badge tone="ok">
          {status.balance.remaining !== null
            ? money(status.balance.currency, status.balance.remaining)
            : `${status.balance.total ?? 0} ${status.balance.currency} granted`}
          {status.balance.remaining !== null && status.balance.total !== null
            ? ` of ${money(status.balance.currency, status.balance.total)}`
            : ""}
        </Badge>
      )}
      {status?.error && (
        <span className="row gap">
          <Badge tone="danger">check failed</Badge>
          <span className="muted truncate" title={status.error}>
            {status.error}
          </span>
        </span>
      )}
      {status && (
        <span className="muted">{new Date(status.checked_at).toLocaleTimeString()}</span>
      )}
      {!supported && provider.balance.preset === "none" && (
        <span className="muted">this provider publishes no balance</span>
      )}
    </span>
  );
}
