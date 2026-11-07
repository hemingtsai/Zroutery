/**
 * The provider drawer: summary, wallet holds, the API key field, the NewAPI
 * panel, configuration (geek mode), balance, decision endpoint and quirks —
 * every edit handed to `onUpdate(mutate)` against the committed config.
 */
import { useMemo, useState } from "react";
import {
  api,
  BALANCE_PRESETS,
  defaultProbe,
  type AppConfig,
  type Provider,
  type Snapshot,
} from "../../api";
import { describeBackendError, useI18n, type TKey } from "../../i18n";
import { useMode } from "../../mode";
import { SaveFeedbackReset, type TaskKind } from "../../saveQueue";
import type { ProviderRepairOverrides } from "../../api";

/** The repair flags a provider may override, with the Routing page's label
 * for each — the drawer speaks the same words as the global defaults. */
const REPAIR_OVERRIDES: { flag: keyof ProviderRepairOverrides; labelKey: TKey }[] = [
  { flag: "thinking_signature", labelKey: "rectifier.thinking_signature" },
  { flag: "media_fallback", labelKey: "rectifier.media_fallback" },
  { flag: "thinking_budget", labelKey: "rectifier.thinking_budget" },
  { flag: "thinking_fallback", labelKey: "rectifier.thinking_fallback" },
  { flag: "codex_metadata", labelKey: "rectifier.codex_metadata" },
  { flag: "codex_auto", labelKey: "rectifier.codex_auto" },
];
import {
  Button,
  Disclosure,
  Drawer,
  Field,
  KeyValue,
  NumberField,
  Section,
  Segment,
  Select,
  StatusDot,
  TextField,
  Toggle,
  useToast,
} from "../../components";
import NewApiPanel from "../NewApiPanel";
import { hasBalanceSource, hasPanelBalance } from "./balance";
import { CLIENT_PROFILES, clientProfilesFor, decisionUrl, defaultBaseUrl, defaultClientProfile, isOpencodeKind, kindLabelKey, kindsFor, needsKey } from "./kindMeta";
import { AccountSection } from "./AccountSection";

/**
 * The credential field, holding its own draft.
 *
 * The draft used to live in `ProviderDrawer`, one component above a catalogue
 * table that can be as long as the provider's own model list — a gateway
 * publishing several hundred models puts several hundred Radix selects in that
 * table. Typing a 40-character key therefore re-rendered all of them forty
 * times, and it measured: 325 ms a keystroke against a 600-row catalogue, where
 * a keystroke into a field that owned its own draft costs one frame. Nothing
 * above this field reads the draft, so the drawer no longer re-renders while a
 * key is being typed.
 *
 * Not [`TextField`]: that commits on blur, and a credential must never reach
 * the store half-written because focus moved.
 */
function ApiKeyField({
  providerId,
  busy,
  run,
}: {
  providerId: string;
  busy: boolean;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
}) {
  const { t } = useI18n();
  const [draft, setDraft] = useState("");

  const saveKey = async () => {
    const value = draft.trim();
    if (!value) return;
    const ok = await run(() => api.setKey(providerId, value));
    if (ok) setDraft("");
  };

  return (
    <Section title={t("providers.api_key")} hint={t("providers.api_key_hint")}>
      <div className="controls">
        <input
          type="password"
          aria-label={t("providers.api_key")}
          autoComplete="off"
          placeholder="sk-…"
          value={draft}
          onChange={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && void saveKey()}
        />
        <Button kind="primary" onClick={saveKey} disabled={busy || !draft.trim()}>
          {t("providers.save_key")}
        </Button>
      </div>
    </Section>
  );
}

/**
 * One provider in full: credential, endpoint, dialect quirks and the models
 * it offers. Saving a key never round-trips the config — it goes to the
 * credential store alone, and the drawer shows only whether one exists.
 */
export function ProviderDrawer({
  provider,
  snapshot,
  busy,
  save,
  run,
  onClose,
  onUpdate,
  onRemove,
}: {
  provider: Provider;
  snapshot: Snapshot;
  busy: boolean;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>, kind?: TaskKind) => Promise<boolean>;
  onClose: () => void;
  /** Mutates the provider being committed; see `Providers.update`. */
  onUpdate: (mutate: (provider: Provider) => void) => void;
  onRemove: () => void;
}) {
  const { t } = useI18n();
  const notify = useToast();
  // Simple mode collapses the drawer to its facts: what this provider is, how
  // it authenticates, whether it is on. The panels, the probe, the quirks and
  // the account declarations keep existing and keep running — the mode only
  // stops drawing them, and geek mode draws every one again.
  const geek = useMode() === "geek";
  const hasKey = snapshot.keys[provider.id] ?? false;
  // Chat credentials an account declares: the auth row would otherwise say
  // "no key" beside a green dot, because the provider's own reference is empty
  // while the account's inference key is what requests are signed with.
  const accountKeys = (provider.accounts ?? []).reduce(
    (n, account) =>
      n +
      account.inference_keys.filter((key) => snapshot.keys[key.key_ref] ?? false)
        .length,
    0,
  );
  const [checkingBalance, setCheckingBalance] = useState(false);
  // Hoisted so the type is narrowed once rather than at two separate index
  // reads, and so the translation has one string to work with.
  const balanceError = snapshot.balances[provider.id]?.error ?? null;
  // What the chosen probe asks for, said under the dropdown.
  const presetHint = (() => {
    const preset = BALANCE_PRESETS.find((p) => p.id === provider.balance.preset);
    return preset ? t(preset.hintKey as Parameters<typeof t>[0]) : null;
  })();

  /**
   * The provider this instance edits, frozen at mount.
   *
   * The parent keys the drawer by provider id, so a switch remounts it and the
   * key draft never travels to the next provider; reading the id here in the
   * same render keeps the two guarantees in one place.
   */
  const providerId = provider.id;

  /**
   * The address this provider is called at.
   *
   * A relay's base url is its endpoint. A decision endpoint's is the base url
   * plus the path it answers on, and the summary's *Endpoint* row has to say
   * the address the request goes to rather than the base it starts from.
   */
  const endpoint =
    provider.kind === "decision"
      ? decisionUrl(provider.base_url, provider.decision.path)
      : provider.base_url;

  /** How many models this provider contributes, for the summary row. */
  const models = useMemo(
    () => snapshot.config.models.filter((m) => m.provider_id === providerId),
    [snapshot.config.models, providerId],
  );

  /** Wallet holds for this provider, one per paused account or key. */
  const holds = snapshot.wallet_holds.filter((hold) => hold.provider_id === providerId);

  return (
      <Drawer
        title={
          <>
            <StatusDot
              tone={
                holds.length > 0
                  ? "danger"
                  : provider.enabled
                    ? hasKey ||
                      accountKeys > 0 ||
                      provider.key_ref === "" ||
                      !needsKey(provider.kind)
                      ? "ok"
                      : "warn"
                    : "off"
              }
            />
            {provider.name}
          </>
        }
        onClose={onClose}
      >
      {/* A drawer opened for one provider starts with no verdict inherited
          from the surface before it — not from the startup read, not from
          another provider's last save. */}
      <SaveFeedbackReset />
      <KeyValue
        rows={[
          [t("providers.endpoint"), <span className="mono">{endpoint}</span>],
          [t("providers.dialect"), t(kindLabelKey(provider.kind))],
          [
            t("providers.auth"),
            hasKey ? (
              <span className="row gap">
                <span className="mono">••••••••••••</span>
                <Button kind="ghost" onClick={() => void run(() => api.clearKey(provider.id))}>
                  {t("common.remove")}
                </Button>
              </span>
            ) : accountKeys > 0 ? (
              t("providers.auth_account_key")
            ) : provider.key_ref === "" || !needsKey(provider.kind) ? (
              t("providers.no_cred")
            ) : (
              t("providers.no_key")
            ),
          ],
          [
            t("providers.models_section"),
            models.length
              ? t("providers.models_count", { n: models.length })
              : t("providers.models_none"),
          ],
        ]}
      />

      {/*
        The hold lives where the release button does. On the page it was a
        second list of the same providers, sorted by nothing and offering the
        one action the row itself could not; here the subject is already named
        by the drawer, so the block says what happened and how to undo it.
      */}
      {holds.length > 0 && (
        <Section title={t("providers.wallet_holds")} hint={t("providers.wallet_holds_hint")}>
          <div className="list">
            {holds.map((hold) => {
              const scope = [hold.account_id, hold.key_id].filter(Boolean).join(" / ");
              return (
                <div
                  className="list-row"
                  key={`${hold.provider_id}:${hold.account_id ?? ""}:${hold.key_id ?? ""}`}
                  style={{ cursor: "default" }}
                >
                  <StatusDot tone="danger" state={t("providers.wallet_empty")} />
                  <div className="row-main">
                    <span className="row-title">
                      {t("providers.wallet_empty")}
                      {scope && <span className="muted mono"> · {scope}</span>}
                    </span>
                    <span className="row-sub">
                      {t("providers.wallet_held_since", {
                        time: new Date(hold.held_at * 1000).toLocaleString(),
                      })}
                    </span>
                  </div>
                  <Button
                    kind="ghost"
                    disabled={busy}
                    onClick={() =>
                      // Releasing a hold is a router runtime action, not a
                      // configuration write.
                      void run(
                        () =>
                          api.releaseWalletHold(hold.provider_id, hold.account_id, hold.key_id),
                        "observe",
                      )
                    }
                  >
                    {t("providers.wallet_release")}
                  </Button>
                </div>
              );
            })}
          </div>
        </Section>
      )}
      {/* A free provider has nothing to paste: the field asks for a credential
          the kind cannot use, and the row beside it already says so. */}
      {!hasKey && needsKey(provider.kind) && provider.key_ref !== "" && (
        <ApiKeyField providerId={providerId} busy={busy} run={run} />
      )}

      {geek && snapshot.newapi_available && (
        <NewApiPanel provider={provider} models={models} busy={busy} run={run} />
      )}

      {geek ? (
      <Section title={t("providers.configuration")}>
        <div className="controls">
          <TextField
            label={t("field.name")}
            value={provider.name}
            onCommit={(name) =>
              name.trim() &&
              onUpdate((p) => {
                p.name = name;
              })
            }
          />
          <TextField
            label={t("field.base_url")}
            hint={t("field.base_url_hint")}
            value={provider.base_url}
            onCommit={(base_url) =>
              base_url.trim() &&
              onUpdate((p) => {
                p.base_url = base_url;
              })
            }
            wide
          />
          <Field label={t("field.api_dialect")}>
            <Segment
              ariaLabel={t("field.api_dialect")}
              value={provider.kind}
              onChange={(kind) =>
                onUpdate((p) => {
                  const previousDefault = defaultBaseUrl(p.kind);
                  p.kind = kind;
                  // The profile is not free alongside the kind: config.rs
                  // refuses an OpenCode provider that is not `opencode_cli`,
                  // and Claude Code is only the sensible pairing for an
                  // Anthropic endpoint. Re-derive it so switching kind in the
                  // drawer cannot leave a config that will not validate.
                  p.client_profile = defaultClientProfile(kind);
                  // Follow the new endpoint only if the base URL is still the
                  // one the old kind defaulted to; a hand-edited one is left
                  // alone.
                  if (p.base_url.trim() === previousDefault) {
                    p.base_url = defaultBaseUrl(kind);
                  }
                })
              }
              options={kindsFor(snapshot.opencode_available).map((k) => ({ value: k.id, label: t(k.labelKey) }))}
            />
          </Field>
          {isOpencodeKind(provider.kind) || provider.kind === "openai_compatible" ? (
            <Field label={t("providers.api_surface")} hint={t("providers.api_surface_hint")}>
              <Segment
                ariaLabel={t("providers.api_surface")}
                value={provider.api_surface}
                onChange={(surface) =>
                  onUpdate((p) => {
                    p.api_surface = surface;
                  })
                }
                options={[
                  { value: "chat", label: t("providers.surface_chat") },
                  { value: "responses", label: t("providers.surface_responses") },
                ]}
              />
            </Field>
          ) : null}
        </div>
        <div className="grid-two">
          <Toggle
            label={t("common.enabled")}
            checked={provider.enabled}
            onChange={(enabled) =>
              onUpdate((p) => {
                p.enabled = enabled;
              })
            }
          />
        </div>
        {/*
          * The advanced fold: timeouts, proxy, client identity and the
          * Anthropic wire contract are fields most providers never need to
          * touch — the defaults are correct for the overwhelming majority.
          * Collapsed so the drawer opens showing what the operator came for.
          */}
        <Disclosure title={t("providers.advanced_settings")}>
          <div className="controls">
            <NumberField
              label={t("field.timeout")}
              hint={t("field.timeout_hint")}
              min={1}
              integer
              value={provider.timeout_secs}
              onCommit={(timeout_secs) =>
                onUpdate((p) => {
                  p.timeout_secs = timeout_secs ?? 600;
                })
              }
            />
            <NumberField
              label={t("field.connect_timeout")}
              min={1}
              integer
              value={provider.connect_timeout_secs}
              onCommit={(connect_timeout_secs) =>
                onUpdate((p) => {
                  p.connect_timeout_secs = connect_timeout_secs ?? 15;
                })
              }
            />
            <Field label={t("providers.proxy_policy")} hint={t("providers.proxy_policy_hint")}>
              <Select
                ariaLabel={t("providers.proxy_policy")}
                value={provider.bypass_proxy == null ? "inherit" : provider.bypass_proxy ? "direct" : "system"}
                onChange={(value) => onUpdate((p) => {
                  p.bypass_proxy = value === "inherit" ? null : value === "direct";
                })}
                options={[
                  { value: "inherit", label: t("providers.proxy_inherit") },
                  { value: "direct", label: t("providers.proxy_direct") },
                  { value: "system", label: t("providers.proxy_system") },
                ]}
              />
            </Field>
            {provider.kind === "anthropic" && (
              <>
                <Field label={t("providers.f_version")} hint={t("providers.f_version_hint")}>
                  <Segment
                    ariaLabel={t("providers.f_version")}
                    value={provider.anthropic_version ?? "2023-06-01"}
                    onChange={(version) =>
                      onUpdate((p) => {
                        p.anthropic_version = version === "2023-06-01" ? null : version;
                      })
                    }
                    options={[
                      { value: "2023-06-01", label: "2023-06-01" },
                      { value: "2026-10-21", label: "2026-10-21" },
                    ]}
                  />
                </Field>
                <TextField
                  label={t("providers.f_beta")}
                  hint={t("providers.f_beta_hint")}
                  value={provider.extra_headers["anthropic-beta"] ?? ""}
                  placeholder="context-management-2025-06-27"
                  onCommit={(v) =>
                    onUpdate((p) => {
                      const trimmed = v.trim();
                      if (trimmed) p.extra_headers["anthropic-beta"] = trimmed;
                      else delete p.extra_headers["anthropic-beta"];
                    })
                  }
                />
              </>
            )}
          </div>
          <div className="grid-two">
            <Field
              label={t("providers.client_profile")}
              hint={
                isOpencodeKind(provider.kind)
                  ? t("providers.client_profile_locked_hint")
                  : t("providers.client_profile_hint")
              }
            >
              <Select
                ariaLabel={t("providers.client_profile")}
                value={provider.client_profile ?? "auto"}
                disabled={isOpencodeKind(provider.kind)}
                onChange={(client_profile) =>
                  onUpdate((p) => {
                    p.client_profile = client_profile;
                  })
                }
                options={
                  isOpencodeKind(provider.kind)
                    ? CLIENT_PROFILES.filter((c) => c.id === "opencode_cli").map((c) => ({
                        value: c.id,
                        label: t(c.labelKey),
                      }))
                    : clientProfilesFor(snapshot.opencode_available).map((c) => ({ value: c.id, label: t(c.labelKey) }))
                }
              />
            </Field>
            {provider.kind === "anthropic" && (
              <Toggle
                label={t("providers.bearer_auth")}
                hint={t("providers.bearer_auth_hint")}
                checked={provider.bearer_auth}
                onChange={(bearer_auth) =>
                  onUpdate((p) => {
                    p.bearer_auth = bearer_auth;
                  })
                }
              />
            )}
          </div>
        </Disclosure>
      </Section>
      ) : (
        /* Simple mode keeps the one switch that decides whether this provider
           serves traffic, and nothing else from the configuration block. */
        <div className="grid-two">
          <Toggle
            label={t("common.enabled")}
            checked={provider.enabled}
            onChange={(enabled) =>
              onUpdate((p) => {
                p.enabled = enabled;
              })
            }
          />
        </div>
      )}

      {geek && (
      <Section
        title={t("providers.balance")}
        hint={
          hasPanelBalance(provider, snapshot)
            ? t("providers.balance_hint_panel")
            : provider.balance.preset !== "none"
              ? t("providers.balance_hint")
              : t("providers.balance_hint_none")
        }
      >
        <div className="controls">
          <Field label={t("field.probe")}>
            <Select
              ariaLabel={t("field.probe")}
              value={provider.balance.preset}
              onChange={(preset) =>
                onUpdate((p) => {
                  p.balance = {
                    preset,
                    custom:
                      preset === "custom" ? p.balance.custom ?? defaultProbe() : null,
                  };
                })
              }
              options={BALANCE_PRESETS.map((p) => ({
                value: p.id,
                label: t(p.labelKey as Parameters<typeof t>[0]),
              }))}
            />
          </Field>
          {/*
            * What the chosen probe asks for. The data was in the table all
            * along and nothing showed it, so the operator had to know that
            * "DeepSeek" meant "/user/balance" by recognising the provider.
            */}
          {presetHint && <p className="field-hint">{presetHint}</p>}
          <div className="field-actions">
            <Button
              kind="ghost"
              onClick={() => {
                setCheckingBalance(true);
                void run(() => api.refreshBalance(provider.id), "observe")
                  .then((ok) => {
                    // The probe's own failure is recorded in the snapshot and
                    // rendered beside the balance below, with the last reading
                    // kept; a rejection means the check never ran at all.
                    if (!ok) notify("error", t("toast.balances_refresh_failed"));
                  })
                  .finally(() => setCheckingBalance(false));
              }}
              disabled={busy || checkingBalance || !hasBalanceSource(provider, snapshot)}
              title={
                hasBalanceSource(provider, snapshot)
                  ? undefined
                  : t("providers.balance_no_source")
              }
            >
              {checkingBalance ? t("providers.checking") : t("providers.check_now")}
            </Button>
          </div>
        </div>
        {balanceError && (
          <p className="field-hint">{describeBackendError(t, balanceError)}</p>
        )}
      </Section>
      )}

      {/* A decision endpoint is asked a question, not a chat, so it has settings
          of its own: where the decision endpoint lives, and how sure the answer
          has to be before the Auto Mode acts on it. */}
      {geek && provider.kind === "decision" && (
        <Section
          title={t("providers.decision")}
          hint={t("providers.decision_hint")}
        >
          <div className="grid-two">
            <Field label={t("providers.decision_path")} hint={t("providers.decision_path_hint")}>
              <input
                value={provider.decision.path}
                onChange={(e) =>
                  onUpdate((p) => {
                    p.decision.path = e.currentTarget.value;
                  })
                }
              />
            </Field>
            <Field
              label={t("providers.decision_threshold")}
              hint={t("providers.decision_threshold_hint")}
            >
              <input
                type="number"
                min={0.5}
                max={1}
                step={0.05}
                value={provider.decision.threshold}
                onChange={(e) =>
                  onUpdate((p) => {
                    const parsed = Number(e.currentTarget.value);
                    p.decision.threshold = Number.isFinite(parsed) ? parsed : 0.85;
                  })
                }
              />
            </Field>
          </div>
        </Section>
      )}

      {geek && (
      <Section title={t("providers.compatibility")} hint={t("providers.compatibility_hint")}>
        <div className="grid-two">
          <Toggle
            label={t("quirk.max_completion_tokens")}
            checked={provider.quirks.use_max_completion_tokens}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.use_max_completion_tokens = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_temperature")}
            checked={provider.quirks.drop_temperature}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_temperature = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_top_p")}
            checked={provider.quirks.drop_top_p}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_top_p = v;
              })
            }
          />
          <Toggle
            label={t("quirk.drop_stop")}
            hint={t("quirk.drop_stop_hint")}
            checked={provider.quirks.drop_stop}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.drop_stop = v;
              })
            }
          />
          <Toggle
            label={t("quirk.system_as_developer")}
            checked={provider.quirks.system_as_developer}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.system_as_developer = v;
              })
            }
          />
          <Toggle
            label={t("quirk.reasoning_effort")}
            checked={provider.quirks.send_reasoning_effort}
            onChange={(v) =>
              onUpdate((p) => {
                p.quirks.send_reasoning_effort = v;
              })
            }
          />
          <div className="grid-span">
            <Toggle
              label={t("quirk.portable_history")}
              hint={t("quirk.portable_history_hint")}
              checked={provider.quirks.portable_history}
              onChange={(v) =>
                onUpdate((p) => {
                  p.quirks.portable_history = v;
                })
              }
            />
          </div>
        </div>
      </Section>
      )}

      {geek && (
      <Section title={t("providers.repair.title")} hint={t("providers.repair.hint")}>
        {/*
          * Each repair starts as "follow the global default" set on the
          * Routing page; a tri-state select is the same grammar the proxy
          * policy uses — undefined is inherit, not a fourth state.
          */}
        {REPAIR_OVERRIDES.map(({ flag, labelKey }) => (
          <Field key={flag} label={t(labelKey)}>
            <Select
              ariaLabel={t(labelKey)}
              value={provider.repair?.[flag] == null ? "inherit" : provider.repair[flag] ? "on" : "off"}
              onChange={(value) =>
                onUpdate((p) => {
                  p.repair ??= {};
                  if (value === "inherit") delete p.repair[flag];
                  else p.repair[flag] = value === "on";
                })
              }
              options={[
                { value: "inherit", label: t("providers.repair.inherit") },
                { value: "on", label: t("providers.repair.on") },
                { value: "off", label: t("providers.repair.off") },
              ]}
            />
          </Field>
        ))}
      </Section>
      )}

      {/*
        Account declarations are offered only when the build actually contains
        the account subsystem, for the same reason the maintenance section
        below asks first: a command that was never registered looks exactly
        like one that failed.
      */}
      {geek && snapshot.account_available && (
        <AccountSection
          provider={provider}
          snapshot={snapshot}
          busy={busy}
          save={save}
          run={run}
          onUpdate={onUpdate}
        />
      )}

      {/* Account maintenance — the runtime face — moved to the Accounts page.
          This drawer keeps the declaration, which is the part that changes when
          the user edits configuration rather than when the account is used. */}

      <div>
        <Button kind="danger" disabled={busy} onClick={onRemove}>
          {t("providers.remove")}
        </Button>
      </div>
    </Drawer>
  );
}

