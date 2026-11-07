/**
 * The providers page: the list, the add form, the CC Switch import, and the
 * drawer. The row, the form, the drawer and the account section live in
 * `pages/providers/`; the tested submission unit is re-exported here so the
 * test keeps its import path.
 */
import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type AppConfig, type CcSwitchPreview, type Provider, type Snapshot } from "../api";
import { describeBackendError, useI18n } from "../i18n";
import { groupProviders } from "../providerGroups";
import { hasChatCredential } from "../providerCredential";
import { Button, ConfirmDialog, Empty, PageHead, Section, useToast, type ConfirmRequest } from "../components";
import type { TaskKind } from "../saveQueue";
import { revealAddFormLanding } from "./Models";
import { AddProviderForm } from "./providers/AddProviderForm";
import { CcSwitchRow } from "./providers/CcSwitchRow";
import { ProviderRow } from "./providers/ProviderRow";
import { hasBalanceSource } from "./providers/balance";
import { needsKey } from "./providers/kindMeta";
import { ProviderDrawer } from "./providers/ProviderDrawer";

/**
 * The provider list. A provider is a connection to one upstream — endpoint,
 * credential, dialect — and the drawer is where it lives in full. Models
 * belong to a provider but have their own page; this drawer only lists them.
 */
export default function Providers({
  snapshot,
  save,
  run,
  busy,
}: {
  snapshot: Snapshot;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  run: (task: () => Promise<Snapshot>, kind?: TaskKind) => Promise<boolean>;
  busy: boolean;
}) {
  const { config, keys, balances } = snapshot;
  const { t, plural } = useI18n();
  const notify = useToast();
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);

  const [ccPreview, setCcPreview] = useState<CcSwitchPreview | null>(null);
  const [ccLoading, setCcLoading] = useState(false);
  // The add form's folded state. Opened from the page head; closes itself
  // when a provider is added, because the drawer that opens over it is the
  // continuation of that task.
  const [adding, setAdding] = useState(false);
  /*
   * …and when it opens, the reader is handed it: the landing under the head
   * is brought into the viewport (a no-op from the top, where the trigger
   * put them) and the focus lands in the form's first field. The behavior
   * and its order live in `revealAddFormLanding`, shared with the model
   * page's entry; StrictMode's double effect just lands the same focus twice.
   */
  const addLandingRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!adding) return;
    revealAddFormLanding(addLandingRef.current, "input");
  }, [adding]);
  const [ccSelected, setCcSelected] = useState<Record<string, boolean>>({});
  const isMounted = useRef(true);
  useEffect(() => () => { isMounted.current = false; }, []);

  /**
   * How many models each provider owns.
   *
   * Every row of the list used to ask the model list this question on its
   * own — one scan of the whole model list per row — so a single render cost
   * O(providers x models), and the number it wanted was a count. Counting
   * once and looking the answer up makes the render O(models), and the number
   * on the row is the same either way.
   */
  const modelCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const model of config.models) {
      counts.set(model.provider_id, (counts.get(model.provider_id) ?? 0) + 1);
    }
    return counts;
  }, [config.models]);

  /*
   * The list, grouped by vendor.
   *
   * `groupProviders` owns the rule and its tests; this page only draws the
   * result, so a change to what counts as the same vendor cannot be made here
   * without the case that motivated it.
   */
  const grouped = useMemo(() => groupProviders(config.providers), [config.providers]);

  /**
   * Opening a provider, and nothing else.
   *
   * The list is drawn one memoised row at a time, so the callback a row is
   * given has to be the same function every render: a new one would make
   * every row look changed and re-render the whole list to open a drawer.
   */
  const openProvider = useCallback((id: string) => setOpenId(id), []);

  /**
   * Apply an edit to the provider the save is actually about.
   *
   * `mutate` receives the provider from the config being committed rather
   * than one captured at render time, so a save queued behind an in-flight
   * save merges into that save's result instead of carrying a whole stale
   * `quirks`/`balance` object back over it.
   */
  const update = (id: string, mutate: (provider: Provider) => void) => {
    void save((cfg) => {
      const next = structuredClone(cfg);
      const provider = next.providers.find((p) => p.id === id);
      if (!provider) return null;
      mutate(provider);
      return next;
    });
  };

  /**
   * Remove a provider, its models and its credential in one backend call.
   *
   * Saving the configuration first and clearing the key afterwards lost the
   * provider's real `key_ref` — the clear could only guess `provider:{id}`, so
   * a custom reference stayed in the credential store. The backend reads the
   * reference before changing the configuration and refuses to delete one
   * another provider still uses.
   */
  const removeProvider = async (id: string) => {
    const models = config.models.filter((m) => m.provider_id === id);
    const ok = await run(() => api.removeProvider(id));
    if (!ok || !isMounted.current) return;
    setOpenId(null);
    if (models.length) {
      notify("ok", t("providers.removed_models_notice", { n: models.length }));
    }
  };

  // ------------------------------------------------------- CC Switch import

  const loadCcPreview = async () => {
    setCcLoading(true);
    try {
      const preview = await api.ccswitchPreview();
      setCcPreview(preview);
      const selection: Record<string, boolean> = {};
      for (const p of preview.providers) {
        selection[p.source_id] = !p.already_imported;
      }
      setCcSelected(selection);
    } catch (e) {
      notify("error", describeBackendError(t, errorText(e)));
    } finally {
      setCcLoading(false);
    }
  };

  /**
   * Refresh every balance, as an observation.
   *
   * The readings travel through the same serial chain as a save — an
   * observation applying an older document over a fresher write would roll
   * the rebase point back — but the words are the observation's own: the
   * readings are current, and nothing here claims a configuration was saved.
   * A per-provider failure arrives inside the snapshot and is said beside that
   * provider's balance; a rejection means the refresh itself could not run,
   * and the last readings stand untouched.
   */
  const refreshAllBalances = async () => {
    const ok = await run(api.refreshBalances, "observe");
    if (ok) notify("ok", t("toast.balances_refreshed"));
    else notify("error", t("toast.balances_refresh_failed"));
  };

  const importSelected = async () => {
    const ids = Object.entries(ccSelected)
      .filter(([, on]) => on)
      .map(([id]) => id);
    if (ids.length === 0) return;
    const ok = await run(() => api.ccswitchImport(ids));
    if (!ok || !isMounted.current) return;
    notify("ok", t("cc.imported_notice", { n: ids.length }));
    await loadCcPreview();
  };

  const open = config.providers.find((p) => p.id === openId) ?? null;
  const openModelCount = open ? (modelCounts.get(open.id) ?? 0) : 0;

  /*
   * Whether any provider has a balance to ask for at all.
   *
   * The gate for the refresh-all button. A provider with `preset: "none"` has
   * nothing to ask for, so on an installation where every provider is like that
   * the button would press, spend a request per provider, and change nothing —
   * which is the same "control this build cannot serve" problem the account
   * section below guards against, in the other direction.
   *
   * A provider that *declares accounts* is counted too, because the balance of
   * a panel-backed provider does not come from the relay: it comes from the
   * panel, read with the user's access token. The relay publishes nothing.
   */
  const anyBalance = config.providers.some((p) => hasBalanceSource(p, snapshot));

  /*
   * Providers the wallet check has paused. The hold is a property of the
   * provider, so it is marked on its own row and explained in its drawer; it
   * used to be a second list above the first, printing the same two providers
   * again under a heading whose only new content was the release button.
   */
  const heldProviders = useMemo(
    () => new Set(snapshot.wallet_holds.map((hold) => hold.provider_id)),
    [snapshot.wallet_holds],
  );

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      <PageHead
        lede={
          config.providers.length === 0
            ? t("providers.lede_none")
            : t("count.providers", { n: config.providers.length })
        }
        actions={
          <>
            {/*
             * Adding is the page's primary verb, so its entry is the page's
             * first action and the form it opens is collapsed until asked
             * for — a permanently expanded three-field form under the list
             * was furniture nobody had asked to sit on. cc-switch puts its
             * add entry in the header and opens a wizard; here the form
             * unfolds directly under this head, above the list — the old
             * berth at the bottom surfaced it a whole list-length below the
             * button that opened it, and the landing of an unfolded form is
             * wherever it is mounted. The layout harness expands it the same
             * way a reader would: by clicking this link.
             */}
            <button type="button"
              className="linky"
              onClick={() => setAdding((open) => !open)}
            >
              {adding ? t("common.collapse") : t("providers.add")}
            </button>
            {/*
              `refresh_balances` asks every provider for its balance and probes
              every account, then answers with one snapshot — so the chips on this
              page and the figures in each drawer go current without opening any of
              them. The account probe below asks the same accounts but answers with
              the per-account outcome, which is the only way to learn *which* one is
              unsupported. They overlap deliberately: one refreshes, one diagnoses.
            */}
            {anyBalance && (
              <button type="button"
                className="linky"
                onClick={() => void refreshAllBalances()}
                disabled={busy}
              >
                {t("providers.refresh_all")}
              </button>
            )}
            <button type="button" className="linky" onClick={loadCcPreview} disabled={ccLoading || busy}>
              {ccLoading ? t("providers.reading_cc") : t("providers.import_cc")}
            </button>
          </>
        }
      />

      {/*
       * The form unfolds from the page head's entry — and it unfolds here,
       * above the list, because the landing of an unfolded form is wherever
       * it is mounted: at the bottom it surfaced a whole list-length below
       * the button that opened it. Opening hands the focus to the form's
       * first field (the effect beside `adding`); the layout harness expands
       * it the same way a reader would, by clicking the head link.
       */}
      {adding && (
        <div ref={addLandingRef}>
          <AddProviderForm
            providers={config.providers}
            save={save}
            busy={busy}
            opencodeAvailable={snapshot.opencode_available}
            onAdded={(id) => {
              setAdding(false);
              openProvider(id);
            }}
          />
        </div>
      )}

      {config.providers.length === 0 ? (
        <div className="empty-state">
          <p>{t("providers.empty")}</p>
          <p className="muted">{t("providers.empty_hint")}</p>
        </div>
      ) : (
        <div className="list">
          {grouped.top.map((p) => (
            <Fragment key={p.id}>
              <ProviderRow
                id={p.id}
                name={p.name}
                baseUrl={p.base_url}
                kind={p.kind}
                modelCount={modelCounts.get(p.id) ?? 0}
                status={
                  !p.enabled
                    ? "off"
                    : !needsKey(p.kind) || hasChatCredential(p, keys)
                      ? "ok"
                      : "warn"
                }
                selected={openId === p.id}
                showBalance={hasBalanceSource(p, snapshot)}
                balance={balances[p.id]}
                held={heldProviders.has(p.id)}
                onOpen={openProvider}
              />
              {(grouped.nested.get(p.id) ?? []).map((d) => (
                <ProviderRow
                  key={d.id}
                  id={d.id}
                  name={d.name}
                  baseUrl={d.base_url}
                  kind={d.kind}
                  modelCount={modelCounts.get(d.id) ?? 0}
                  status={
                    !d.enabled
                      ? "off"
                      : !needsKey(d.kind) || hasChatCredential(d, keys)
                        ? "ok"
                        : "warn"
                  }
                  selected={openId === d.id}
                  showBalance={hasBalanceSource(d, snapshot)}
                  balance={balances[d.id]}
                  held={heldProviders.has(d.id)}
                  nested
                  onOpen={openProvider}
                />
              ))}
            </Fragment>
          ))}
        </div>
      )}

      {/* The account probe moved to the Accounts page: it is a runtime reading
          over every provider's accounts, not a property of this page's list, and
          the drawer never had a good place to put a cross-provider report. */}

      {ccPreview && (
        <Section
          title={t("providers.import_cc")}
          hint={ccPreview.source ? t("cc.source_hint", { path: ccPreview.source }) : undefined}
          actions={
            <>
              <Button kind="ghost" onClick={loadCcPreview} disabled={ccLoading || busy}>
                {t("cc.reload")}
              </Button>
              <Button
                kind="primary"
                onClick={importSelected}
                disabled={busy || !Object.values(ccSelected).some(Boolean)}
              >
                {t("cc.import")}
              </Button>
            </>
          }
        >
          {ccPreview.providers.length === 0 ? (
            <Empty>{t("cc.empty")}</Empty>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">{t("cc.import")}</th>
                  <th scope="col">{t("field.name")}</th>
                  <th scope="col">{t("providers.endpoint")}</th>
                  <th scope="col">{t("nav.models")}</th>
                </tr>
              </thead>
              <tbody>
                {ccPreview.providers.map((p) => (
                  <CcSwitchRow
                    key={p.source_id}
                    draft={p}
                    checked={ccSelected[p.source_id] ?? false}
                    onSelect={(id, on) => setCcSelected({ ...ccSelected, [id]: on })}
                  />
                ))}
              </tbody>
            </table>
          )}
        </Section>
      )}

      {open && (
        <ProviderDrawer
          key={open.id}
          provider={open}
          snapshot={snapshot}
          busy={busy}
          save={save}
          run={run}
          onClose={() => setOpenId(null)}
          onUpdate={(mutate) => update(open.id, mutate)}
          onRemove={() =>
            setConfirm({
              title: t("confirm.remove_provider"),
              body: plural(openModelCount, "confirm.remove_provider_body"),
              confirmLabel: t("providers.remove"),
              danger: true,
              onConfirm: () => void removeProvider(open.id),
            })
          }
        />
      )}
    </>
  );
}

// The tested submission unit keeps its old import path (Providers.test.ts).

export { createAddProviderSubmission } from "./providers/addProvider";

export type { AddProviderDraft, AddProviderOutcome } from "./providers/addProvider";
