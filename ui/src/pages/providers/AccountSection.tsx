/**
 * One provider's account declaration and inference-key pool: the identity
 * line, the declared credential, the per-account keys with their drag
 * reordering, and the binding review. The deepest single unit on the page.
 */
import { useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import {
  api,
  errorText,
  type Account,
  type BindingPreview,
  type AppConfig,
  type Maintenance,
  type Provider,
  type Snapshot,
} from "../../api";
import { describeBackendError, useI18n } from "../../i18n";
import {
  Badge,
  Button,
  CompactNumber,
  ConfirmDialog,
  Empty,
  Field,
  NumberField,
  Section,
  Select,
  TextField,
  Toggle,
  useToast,
  type ConfirmRequest,
} from "../../components";
import { AccountBindingReview } from "../../AccountBindingReview";

/** A fresh declaration: nothing observed, nothing scheduled, provider's key. */
function blankMaintenance(): Maintenance {
  return {
    checkin_enabled: false,
    checkin_interval_secs: null,
    checkin_path: null,
    checkin_trigger_selector: null,
    login_path: null,
    panel_url: null,
    new_api_user: null,
    browser_executable: "",
  };
}

/**
 * Declare, credential and maintain one provider's accounts.
 *
 * What this section edits is the *declaration*: the configuration that says an
 * account exists, where its credential lives and what maintenance was asked
 * for. It never shows an observation — quota, balances and the last check-in
 * are runtime readings, and they render in the maintenance section below,
 * filled by the reconciler.
 *
 * Two credentials are kept apart on purpose. The panel token (`key_ref`) probes
 * balances and signs in; it is never sent as chat traffic. `inference_keys` is
 * the pool the routing path may actually send upstream, and a declared account
 * with an empty pool does not fall back to the panel token or the provider key.
 *
 * Keys are written through the backend command, never through the config save:
 * the dashboard only ever learns whether a secret exists, the same discipline
 * the provider key field follows.
 */
export function AccountSection({
  provider,
  snapshot,
  busy,
  save,
  run,
  onUpdate,
}: {
  provider: Provider;
  snapshot: Snapshot;
  busy: boolean;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  onUpdate: (mutate: (provider: Provider) => void) => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  const accounts = provider.accounts ?? [];
  const [newId, setNewId] = useState("");
  const [newKey, setNewKey] = useState("");
  const [keyFor, setKeyFor] = useState<string | null>(null);
  const [keyDraft, setKeyDraft] = useState("");
  const [userHint, setUserHint] = useState("");
  const [bindingPreview, setBindingPreview] = useState<BindingPreview | null>(null);
  const [newInference, setNewInference] = useState<
    Record<string, { id: string; secret: string; note: string }>
  >({});
  const [inferenceKeyFor, setInferenceKeyFor] = useState<string | null>(null);
  const [inferenceDraft, setInferenceDraft] = useState("");
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [dragFrom, setDragFrom] = useState<{ accountId: string; keyId: string } | null>(null);
  const [dropAt, setDropAt] = useState<{ accountId: string; index: number } | null>(null);
  const [reordering, setReordering] = useState<Record<string, boolean>>({});
  const reorderPending = useRef(new Set<string>());
  const inferenceWriteChains = useRef(new Map<string, Promise<boolean>>());
  const inferenceRows = useRef(new Map<string, HTMLDivElement>());

  /** A reference of its own — the account does not resolve to the provider key. */
  const hasOwnRef = (a: Account) =>
    a.key_ref !== "" && a.key_ref !== provider.key_ref;
  const keyStored = (a: Account) =>
    hasOwnRef(a) && (snapshot.keys[a.key_ref] ?? false);

  /** Edit one declaration in place; the id is the key within this provider. */
  const edit = (account_id: string, mutate: (a: Account) => void) =>
    onUpdate((p) => {
      const declaration = p.accounts?.find((a) => a.account_id === account_id);
      if (declaration) mutate(declaration);
    });

  const addAccount = async () => {
    const id = newId.trim();
    const key = newKey.trim();
    if (!id) return;
    if (accounts.some((a) => a.account_id === id)) {
      notify("error", t("accounts.duplicate", { id }));
      return;
    }
    const ok = await save((cfg) => {
      const next = structuredClone(cfg);
      const target = next.providers.find((p) => p.id === provider.id);
      if (!target) return null;
      target.accounts = target.accounts ?? [];
      // Re-checked here rather than trusted from the render: a save that
      // queues behind another one rebases onto the newer document.
      if (target.accounts.some((a) => a.account_id === id)) return null;
      target.accounts.push({
        account_id: id,
        key_ref: "",
        probe_kind: "none",
        enabled: true,
        inference_keys: [],
        maintenance: blankMaintenance(),
      });
      return next;
    });
    if (!ok) return;
    setNewId("");
    setNewKey("");
    if (key) {
      // The declaration exists only after the save landed, so the key is
      // stored as a second step against the saved configuration.
      const stored = await run(() => api.setAccountKey(provider.id, id, key));
      if (!stored) notify("error", t("accounts.key_store_failed"));
    }
  };

  const storeKey = async (account_id: string) => {
    const key = keyDraft.trim();
    if (!key) return;
    try {
      setBindingPreview(await api.stageAccountKey(provider.id, account_id, key, userHint.trim() || undefined));
      setKeyDraft("");
    } catch (error) { notify("error", describeBackendError(t, errorText(error))); }
  };

  const inferenceDraftFor = (account_id: string) =>
    newInference[account_id] ?? { id: "", secret: "", note: "" };

  const editNewInference = (
    account_id: string,
    patch: Partial<{ id: string; secret: string; note: string }>,
  ) =>
    setNewInference((current) => ({
      ...current,
      [account_id]: {
        ...(current[account_id] ?? { id: "", secret: "", note: "" }),
        ...patch,
      },
    }));

  /**
   * The command edits saved configuration, so it runs without a config save:
   * an enable/priority change must not rewrite the declaration the user is
   * still editing.
   */
  const writeInference = async (
    account_id: string,
    key_id: string,
    options: { api_key?: string; priority?: number; enabled?: boolean; note?: string },
  ) => {
    const previous = inferenceWriteChains.current.get(account_id) ?? Promise.resolve(true);
    const pending = previous
      .then((previousOk) =>
        previousOk
          ? run(() => api.setAccountInferenceKey(provider.id, account_id, key_id, options))
          : false,
      );
    inferenceWriteChains.current.set(account_id, pending);
    const ok = await pending;
    if (inferenceWriteChains.current.get(account_id) === pending) {
      inferenceWriteChains.current.delete(account_id);
    }
    if (!ok) notify("error", t("accounts.inference_store_failed"));
    return ok;
  };

  const reorderInference = async (
    account_id: string,
    order: (typeof accounts)[number]["inference_keys"],
  ) => {
    if (reorderPending.current.has(account_id)) return;
    reorderPending.current.add(account_id);
    setReordering((current) => ({ ...current, [account_id]: true }));
    try {
      // The command changes persisted config. Await every write so each one
      // rebases on the snapshot committed by the previous key's update.
      for (const [index, key] of order.entries()) {
        const priority = index * 10;
        if (!(await writeInference(account_id, key.id, { priority }))) return;
      }
    } finally {
      reorderPending.current.delete(account_id);
      setReordering((current) => ({ ...current, [account_id]: false }));
    }
  };

  const moveInference = (
    account_id: string,
    order: typeof accounts[number]["inference_keys"],
    from: number,
    delta: -1 | 1,
  ) => {
    const to = from + delta;
    if (to < 0 || to >= order.length || reorderPending.current.has(account_id)) return;
    const next = [...order];
    [next[from], next[to]] = [next[to], next[from]];
    void reorderInference(account_id, next);
  };

  const startInferenceDrag = (
    account_id: string,
    ordered: (typeof accounts)[number]["inference_keys"],
    from: number,
  ) => (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (busy || reorderPending.current.has(account_id) || event.button !== 0) return;
    const startY = event.clientY;
    let started = false;
    const hitIndex = (y: number) => {
      for (let index = 0; index < ordered.length; index += 1) {
        const row = inferenceRows.current.get(`${account_id}:${ordered[index].id}`);
        if (!row) continue;
        const rect = row.getBoundingClientRect();
        if (y < rect.top + rect.height / 2) return index;
      }
      return ordered.length;
    };
    const finish = (event: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      if (!started) return;
      const to = hitIndex(event.clientY);
      setDragFrom(null);
      setDropAt(null);
      if (to === from || to === from + 1) return;
      const next = [...ordered];
      const [key] = next.splice(from, 1);
      next.splice(to > from ? to - 1 : to, 0, key);
      void reorderInference(account_id, next);
    };
    const cancel = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", cancel);
      setDragFrom(null);
      setDropAt(null);
    };
    const move = (pointer: PointerEvent) => {
      if (!started) {
        if (Math.abs(pointer.clientY - startY) < 4) return;
        started = true;
        setDragFrom({ accountId: account_id, keyId: ordered[from].id });
      }
      pointer.preventDefault();
      setDropAt({ accountId: account_id, index: hitIndex(pointer.clientY) });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", cancel);
  };

  const addInference = async (account_id: string) => {
    const draft = inferenceDraftFor(account_id);
    const id = draft.id.trim();
    if (!id) return;
    const account = accounts.find((a) => a.account_id === account_id);
    if (account?.inference_keys.some((key) => key.id === id)) {
      notify("error", t("accounts.inference_duplicate", { id }));
      return;
    }
    const ok = await writeInference(account_id, id, {
      api_key: draft.secret.trim(),
      note: draft.note.trim(),
    });
    if (ok) editNewInference(account_id, { id: "", secret: "", note: "" });
  };

  const replaceInference = async (account_id: string, key_id: string) => {
    const secret = inferenceDraft.trim();
    if (!secret) return;
    const ok = await writeInference(account_id, key_id, { api_key: secret });
    if (ok) {
      setInferenceDraft("");
      setInferenceKeyFor(null);
    }
  };

  return (
    <Section title={t("accounts.title")} hint={t("accounts.title_hint")}>
      {bindingPreview && <AccountBindingReview preview={bindingPreview} busy={busy}
        publish={(confirmed) => void run(() => api.publishAccountKey(bindingPreview.publication_id, confirmed)).then((ok) => {
          if (ok) { setBindingPreview(null); setKeyFor(null); }
        })}
        discard={() => void api.discardAccountKey(bindingPreview.publication_id).then(() => setBindingPreview(null)).catch((error) => notify("error", errorText(error)))} />}
      {accounts.length === 0 ? (
        <Empty>{t("accounts.empty")}</Empty>
      ) : (
        <div className="list">
          {accounts.map((a) => (
            <div className="account-decl" key={a.account_id}>
              <div className="row-main">
                <span className="row-title mono">{a.account_id}</span>
                {a.binding && <Badge tone={a.binding.verification === "verified" ? "ok" : "warn"}>
                  {t(a.binding.verification === "verified" ? "binding.verified" : "binding.pending")}
                  {a.binding.identity && ` · ${a.binding.identity.username ?? a.binding.identity.user_id} · ${a.binding.identity.group ?? t("common.unknown")}`}
                </Badge>}
                {hasOwnRef(a) ? (
                  keyStored(a) ? (
                    <Badge tone="ok">{t("accounts.own_key")}</Badge>
                  ) : (
                    <Badge tone="warn">{t("accounts.no_key")}</Badge>
                  )
                ) : (
                  <Badge>{t("accounts.follows_provider")}</Badge>
                )}
                <div className="field-actions">
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() => {
                      setKeyFor(keyFor === a.account_id ? null : a.account_id);
                      setKeyDraft("");
                    }}
                  >
                    {t("accounts.set_key")}
                  </Button>
                  {hasOwnRef(a) && (
                    <Button
                      kind="ghost"
                      disabled={busy}
                      onClick={() =>
                        setConfirm({
                          title: t("accounts.clear_key_title", { id: a.account_id }),
                          body: t("accounts.clear_key_body"),
                          confirmLabel: t("accounts.clear_key"),
                          danger: true,
                          onConfirm: () =>
                            void run(() =>
                              api.clearAccountKey(provider.id, a.account_id),
                            ),
                        })
                      }
                    >
                      {t("accounts.clear_key")}
                    </Button>
                  )}
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() =>
                      setConfirm({
                        title: t("accounts.remove_title", { id: a.account_id }),
                        body: t("accounts.remove_body"),
                        confirmLabel: t("common.delete"),
                        danger: true,
                        onConfirm: () =>
                          void run(() => api.removeAccount(provider.id, a.account_id)),
                      })
                    }
                  >
                    {t("common.delete")}
                  </Button>
                </div>
              </div>
              {keyFor === a.account_id && (
                <div className="controls">
                  <TextField
                    label={t("accounts.key_label")}
                    hint={t("accounts.key_hint")}
                    value={keyDraft}
                    password
                    onCommit={(v) => {
                      setKeyDraft(v);
                    }}
                  />
                  <TextField label={t("binding.user_hint")} value={userHint} onCommit={setUserHint} />
                  <div className="field-actions">
                    <Button
                      kind="primary"
                      disabled={busy || !keyDraft.trim()}
                      onClick={() => void storeKey(a.account_id)}
                    >
                      {t("accounts.set_key")}
                    </Button>
                  </div>
                </div>
              )}
              <Field label={t("binding.probe_kind")}><Select ariaLabel={t("binding.probe_kind")} value={a.probe_kind ?? "new_api"}
                options={[{ value: "none", label: t("binding.probe_none") }, { value: "new_api", label: t("binding.probe_newapi") }]}
                onChange={(kind) => edit(a.account_id, (account) => { account.probe_kind = kind as "none" | "new_api"; })} /></Field>
              {/*
                * Chat credentials, separate from the panel token above. The
                * list is edited by command rather than by the config save, so
                * a toggle here cannot race the declaration this drawer holds.
                */}
              <div className="inference-pool">
                <div className="pool-head">
                  <span className="pool-title">{t("accounts.inference_title")}</span>
                  <span className="field-hint">{t("accounts.inference_hint")}</span>
                </div>
                {a.inference_keys.length === 0 ? (
                  <Empty>{t("accounts.inference_empty")}</Empty>
                ) : (
                  <div className="key-list">
                    {[...a.inference_keys]
                      .sort(
                        (left, right) =>
                          left.priority - right.priority || left.id.localeCompare(right.id),
                      )
                      .map((key, index, ordered) => {
                      const slot = `${a.account_id}:${key.id}`;
                      const stored = snapshot.keys[key.key_ref] ?? false;
                      const dragging = dragFrom?.accountId === a.account_id && dragFrom.keyId === key.id;
                      const dragIndex = dragFrom?.accountId === a.account_id
                        ? ordered.findIndex((item) => item.id === dragFrom.keyId)
                        : -1;
                      const dropBefore = dropAt?.accountId === a.account_id && dropAt.index === index && dropAt.index !== dragIndex && dropAt.index !== dragIndex + 1;
                      const dropAfter = dropAt?.accountId === a.account_id && dropAt.index === ordered.length && index === ordered.length - 1 && dragIndex !== ordered.length - 1;
                      return (
                        <div
                          className={`key-row ${dragging ? "key-row-dragging" : ""} ${dropBefore ? "key-row-drop-before" : ""} ${dropAfter ? "key-row-drop-after" : ""}`}
                          data-inference-key-id={key.id}
                          key={key.id}
                          ref={(row) => {
                            const rowId = `${a.account_id}:${key.id}`;
                            if (row) inferenceRows.current.set(rowId, row);
                            else inferenceRows.current.delete(rowId);
                          }}
                        >
                          <button
                            type="button"
                            className="inference-drag-handle"
                            aria-label={t("routing.drag_hint")}
                            title={t("routing.drag_hint")}
                            disabled={busy || reordering[a.account_id] || ordered.length < 2}
                            onPointerDown={startInferenceDrag(a.account_id, ordered, index)}
                          >
                            ⋮⋮
                          </button>
                          <div className="key-head">
                            <span className="key-id mono">{key.id}</span>
                            <input
                              className="text-input key-note"
                              style={{ maxWidth: 220 }}
                              defaultValue={key.note}
                              placeholder={t("accounts.inference_note_placeholder")}
                              aria-label={t("accounts.inference_note")}
                              onBlur={(event) => {
                                const note = event.currentTarget.value.trim();
                                if (note !== key.note) {
                                  void writeInference(a.account_id, key.id, { note });
                                }
                              }}
                            />
                            {stored ? (
                              <Badge tone="ok">{t("accounts.inference_stored")}</Badge>
                            ) : (
                              <Badge tone="warn">{t("accounts.inference_missing")}</Badge>
                            )}
                          </div>
                          <div className="key-controls">
                            <Toggle
                              label={t("common.enabled")}
                              checked={key.enabled}
                              onChange={(enabled) =>
                                void writeInference(a.account_id, key.id, { enabled })
                              }
                            />
                            <label
                              className="key-priority"
                              title={t("accounts.inference_priority_hint")}
                            >
                              <span>{t("accounts.inference_priority_label")}</span>
                              <CompactNumber
                                integer
                                min={0}
                                value={key.priority}
                                ariaLabel={t("accounts.inference_priority_label")}
                                onCommit={(v) =>
                                  void writeInference(a.account_id, key.id, {
                                    priority: v ?? 0,
                                  })
                                }
                              />
                            </label>
                          </div>
                          <div className="key-actions">
                              <button
                                type="button"
                                className="btn btn-ghost"
                                aria-label={t("routing.move_up", { model: key.id })}
                                disabled={busy || reordering[a.account_id] || index === 0}
                                onClick={() => moveInference(a.account_id, ordered, index, -1)}
                                onKeyDown={(event) => {
                                  if (event.key === "Enter" || event.key === " ") {
                                    event.preventDefault();
                                    moveInference(a.account_id, ordered, index, -1);
                                  }
                                }}
                              >
                                ↑
                              </button>
                              <button
                                type="button"
                                className="btn btn-ghost"
                                aria-label={t("routing.move_down", { model: key.id })}
                                disabled={busy || reordering[a.account_id] || index === ordered.length - 1}
                                onClick={() => moveInference(a.account_id, ordered, index, 1)}
                                onKeyDown={(event) => {
                                  if (event.key === "Enter" || event.key === " ") {
                                    event.preventDefault();
                                    moveInference(a.account_id, ordered, index, 1);
                                  }
                                }}
                              >
                                ↓
                              </button>
                            <Button
                              kind="ghost"
                              disabled={busy}
                              onClick={() => {
                                setInferenceKeyFor(
                                  inferenceKeyFor === slot ? null : slot,
                                );
                                setInferenceDraft("");
                              }}
                            >
                              {t("accounts.inference_replace")}
                            </Button>
                            <Button
                              kind="ghost"
                              disabled={busy}
                              onClick={() =>
                                setConfirm({
                                  title: t("accounts.inference_remove_title", {
                                    id: key.id,
                                  }),
                                  body: t("accounts.inference_remove_body"),
                                  confirmLabel: t("common.delete"),
                                  danger: true,
                                  onConfirm: () =>
                                    void run(() =>
                                      api.removeAccountInferenceKey(
                                        provider.id,
                                        a.account_id,
                                        key.id,
                                      ),
                                    ),
                                })
                              }
                            >
                              {t("common.delete")}
                            </Button>
                          </div>
                          {inferenceKeyFor === slot && (
                            <div className="key-replace">
                              <TextField
                                label={t("accounts.inference_key_label")}
                                hint={t("accounts.inference_key_hint")}
                                value={inferenceDraft}
                                password
                                onCommit={(v) => setInferenceDraft(v)}
                              />
                              <div className="field-actions">
                                <Button
                                  kind="primary"
                                  disabled={busy || !inferenceDraft.trim()}
                                  onClick={() =>
                                    void replaceInference(a.account_id, key.id)
                                  }
                                >
                                  {t("common.save")}
                                </Button>
                              </div>
                            </div>
                          )}
                        </div>
                      );
                      })}
                  </div>
                )}
                <div className="controls">
                  <TextField
                    label={t("accounts.inference_new_id")}
                    hint={t("accounts.inference_new_id_hint")}
                    value={inferenceDraftFor(a.account_id).id}
                    onCommit={(v) => editNewInference(a.account_id, { id: v })}
                  />
                  <TextField
                    label={t("accounts.inference_new_secret")}
                    hint={t("accounts.inference_key_hint")}
                    value={inferenceDraftFor(a.account_id).secret}
                    password
                    onCommit={(v) => editNewInference(a.account_id, { secret: v })}
                  />
                  <TextField
                    label={t("accounts.inference_note")}
                    value={inferenceDraftFor(a.account_id).note}
                    onCommit={(v) => editNewInference(a.account_id, { note: v })}
                  />
                  <div className="field-actions">
                    <Button
                      kind="primary"
                      disabled={
                        busy ||
                        !inferenceDraftFor(a.account_id).id.trim() ||
                        !inferenceDraftFor(a.account_id).secret.trim()
                      }
                      onClick={() => void addInference(a.account_id)}
                    >
                      {t("accounts.inference_add")}
                    </Button>
                  </div>
                </div>
              </div>
              <div className="controls">
                <Toggle
                  label={t("common.enabled")}
                  checked={a.enabled}
                  onChange={(enabled) => edit(a.account_id, (d) => { d.enabled = enabled; })}
                />
                <Toggle
                  label={t("accounts.checkin")}
                  checked={a.maintenance.checkin_enabled}
                  onChange={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.checkin_enabled = v;
                      if (v) {
                        // NewAPI's dominant layout. Filled only when the field
                        // is empty so an operator's correction is never
                        // overwritten by turning the toggle off and on again.
                        d.maintenance.checkin_path ??= "/console/personal";
                        d.maintenance.checkin_trigger_selector ??= "button.checkin";
                        d.maintenance.login_path ??= "/sign-in";
                      }
                    })
                  }
                />
                <NumberField
                  label={t("accounts.interval")}
                  hint={t("accounts.interval_hint")}
                  min={1}
                  integer
                  value={a.maintenance.checkin_interval_secs}
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.checkin_interval_secs = v;
                    })
                  }
                />
                <TextField
                  label={t("accounts.panel_url")}
                  hint={t("accounts.panel_url_hint")}
                  value={a.maintenance.panel_url ?? ""}
                  placeholder="https://panel.example"
                  wide
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.panel_url = v.trim() || null;
                    })
                  }
                />
                <TextField
                  label={t("accounts.new_api_user")}
                  hint={t("accounts.new_api_user_hint")}
                  value={a.maintenance.new_api_user ?? ""}
                  placeholder="354394"
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.new_api_user = v.trim() || null;
                    })
                  }
                />
                <TextField
                  label={t("accounts.login_path")}
                  hint={t("accounts.login_path_hint")}
                  value={a.maintenance.login_path ?? ""}
                  placeholder="/sign-in"
                  wide
                  onCommit={(v) =>
                    edit(a.account_id, (d) => {
                      d.maintenance.login_path = v.trim() || null;
                    })
                  }
                />
                {a.maintenance.checkin_enabled && (
                  <>
                    <TextField
                      label={t("accounts.checkin_path")}
                      hint={t("accounts.checkin_path_hint")}
                      value={a.maintenance.checkin_path ?? ""}
                      placeholder="/console/personal"
                      wide
                      onCommit={(v) =>
                        edit(a.account_id, (d) => {
                          d.maintenance.checkin_path = v.trim() || null;
                        })
                      }
                    />
                    <TextField
                      label={t("accounts.checkin_selector")}
                      hint={t("accounts.checkin_selector_hint")}
                      value={a.maintenance.checkin_trigger_selector ?? ""}
                      wide
                      onCommit={(v) =>
                        edit(a.account_id, (d) => {
                          d.maintenance.checkin_trigger_selector = v.trim() || null;
                        })
                      }
                    />
                  </>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
      <div className="controls">
        <TextField
          label={t("accounts.id_label")}
          hint={t("accounts.id_hint")}
          value={newId}
          onCommit={(v) => setNewId(v)}
        />
        <TextField
          label={t("accounts.key_label_add")}
          hint={t("accounts.key_hint_add")}
          value={newKey}
          password
          onCommit={(v) => setNewKey(v)}
        />
        <div className="field-actions">
          <Button
            kind="primary"
            disabled={busy || !newId.trim()}
            onClick={() => void addAccount()}
          >
            {t("accounts.add")}
          </Button>
        </div>
      </div>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />
    </Section>
  );
}
