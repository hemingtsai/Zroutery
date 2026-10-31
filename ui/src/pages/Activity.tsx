import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MouseEvent,
  type ReactNode,
} from "react";
import {
  api,
  compactCount,
  costText,
  isSideKind,
  money,
  ms,
  tierName,
  tps,
  totalsText,
  modelName,
  type BudgetScope,
  type RequestKind,
  type RequestRecord,
  type ClientTotals,
  type Snapshot,
  type UsageDay,
  type AppConfig,
} from "../api";
import {
  Badge,
  Button,
  ConfirmDialog,
  Disclosure,
  Drawer,
  KeyValue,
  PageHead,
  Section,
  Segment,
  Select,
  StatusDot,
  Tabs,
  num,
  useToast,
  type ConfirmRequest,
} from "../components";
import { useI18n } from "../i18n";
import { useMode } from "../mode";
import UsageCharts, { HEATMAP_DAYS, type Metric as ChartMetric } from "./UsageCharts";
import Budgets from "./Budgets";

/**
 * stats.rs (`UNRESOLVED_MODEL_KEY`) files every request that never resolved
 * under one shared bucket by this name, while the records themselves carry
 * `resolved_model: null`. The two spellings of the same fact are joined in the
 * stream filter, so the one ledger row whose name the backend invented filters
 * to the requests it actually counts instead of to nothing.
 */
const UNRESOLVED_MODEL_KEY = "unresolved";

/*
 * Radix's Select rejects an empty-string value, so "no facet" is a real
 * sentinel option — the same move the catalogue row makes. It is the first
 * option and the value the control holds while nothing is chosen, so the
 * trigger reads "All providers" exactly when no facet narrows the list, and
 * the reader can always walk back to it.
 */
const FACET_ALL = "__all__";

/*
 * The stream's records carry the client fingerprint on the record — stats.rs
 * stamps every RequestRecord with the id its registry filed the request under,
 * the very id the per-client ledger is keyed by — but api.ts's RequestRecord
 * interface does not declare the field yet, so it is read structurally here
 * rather than by editing the shared contract from inside a page. A record
 * without it (the mock bridge's fixture, archives written before client
 * attribution existed) matches no client, which is the honest answer: a stream
 * that cannot say who sent a request must not claim it for one.
 */
type AttributedRecord = RequestRecord & { client?: { client_id?: string } };

const recordClientId = (record: RequestRecord): string | null =>
  (record as AttributedRecord).client?.client_id ?? null;

/** An id this long does not fit a cell; the full value rides on the title. */
const truncateId = (id: string) => (id.length > 24 ? `${id.slice(0, 23)}…` : id);

/*
 * A saved-file path shown in one line: the file name and the separator before
 * it, with an ellipsis standing for everything above. The full path rides on
 * the title of whatever renders the short form — a toast or a hint line that
 * spelled out every directory would wrap, and the reader copying a path wants
 * the whole string anyway, which the hover gives.
 */
const shortPath = (path: string) => {
  const cut = Math.max(path.lastIndexOf("\\"), path.lastIndexOf("/"));
  return cut >= 0 ? `…${path.slice(cut)}` : path;
};

/*
 * A ledger row is clickable — clicking it filters the stream below to that
 * row's requests — but the buttons inside it are not part of the row's click:
 * clearing a client must not also filter the stream to it, and each name cell
 * is a button of its own for the keyboard. So the row's handler stands down
 * for anything that is itself a control.
 */
const rowActivate =
  (toggle: () => void) =>
  (event: MouseEvent<HTMLTableRowElement>): void => {
    if ((event.target as HTMLElement).closest("button, a")) return;
    toggle();
  };

/**
 * One ledger row's clickable name: the keyboard's way into the row.
 *
 * The row is the pointer's target; this button is what Tab and a screen
 * reader meet. Its pressed state IS the stream's filter — the same grammar as
 * the KPI cells that drive the chart's measure — so a control's state is the
 * fact it claims to control, never a memory of having been clicked.
 */
function LedgerName({
  pressed,
  onToggle,
  title,
  children,
}: {
  pressed: boolean;
  onToggle: () => void;
  /** The raw id, for hover, when the label is a resolved name. */
  title?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="ledger-name"
      aria-pressed={pressed}
      title={title}
      onClick={onToggle}
    >
      {children}
    </button>
  );
}

/**
 * How a client renders wherever it appears in the ledger.
 *
 * A named client reads "name · id". Two ways to be unnamed: the field is
 * empty, or the backend filled it with the raw id itself (a user-agent the
 * attribution could not name) — either way a naive fallback renders the same
 * string twice. An unknown client says what it is once, with the full id on
 * hover. One rendering, used by the main table and the stray drawer alike, so
 * the two tables cannot grow different ideas of what a client looks like.
 */
function ClientNameCell({
  client,
  pressed,
  onToggle,
}: {
  client: ClientTotals;
  pressed: boolean;
  onToggle: () => void;
}) {
  const { t } = useI18n();
  if (client.client_name && client.client_name !== client.client_id) {
    return (
      <LedgerName pressed={pressed} onToggle={onToggle}>
        <span className="mono">{client.client_name}</span>
        <span className="muted"> · {client.client_id}</span>
      </LedgerName>
    );
  }
  return (
    <LedgerName pressed={pressed} onToggle={onToggle} title={client.client_id}>
      <span className="muted">
        {t("activity.client_unknown")} <span className="mono">{truncateId(client.client_id)}</span>
      </span>
    </LedgerName>
  );
}

/**
 * The usage page as one question — where did the usage and the money go —
 * answered at two zoom levels. The ledger sums every recorded request and cuts
 * the sum two ways, by client and by model; the stream shows the requests a
 * sum is made of. Clicking a ledger row walks from one to the other. The
 * headline totals live in the KPI row above and appear nowhere else: the same
 * figure under three headings is three figures to reconcile.
 */
export default function Activity({
  snapshot,
  run,
  save,
  busy,
}: {
  snapshot: Snapshot;
  run: (task: () => Promise<Snapshot>) => Promise<boolean>;
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
}) {
  const { summary, recent, health } = snapshot;
  const { t } = useI18n();
  // Simple mode keeps the usage ledger alone: the internal log and the budgets
  // are advanced surfaces. The mode only filters visibility — the panes keep
  // working, and a reader who was on one when the mode narrowed lands back on
  // the ledger rather than a strip promising a tab that is not listed.
  const mode = useMode();
  const simpleOnly = mode === "simple";
  const [openId, setOpenId] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
  const [tab, setTab] = useState<"usage" | "log" | "budgets">("usage");
  useEffect(() => {
    if (simpleOnly && tab !== "usage") setTab("usage");
  }, [simpleOnly, tab]);
  /*
   * The internal log, here rather than on the settings page it used to live on.
   *
   * It was the one observability surface not on this page, which is otherwise
   * entirely observability — health, spend, timing, usage. What a reader wants
   * from a log is the same thing they came here for, and sending them to
   * "Advanced" on a settings page to find it is what makes a working feature
   * look absent.
   *
   * `null` means not following, which is the distinction the button draws. It is
   * not an empty log: a log that cannot be read arrives as `[]` from `loadLogs`,
   * so "stopped following" and "there is nothing logged" never render the same.
   */
  const [logs, setLogs] = useState<string[] | null>(null);
  const [history, setHistory] = useState<"export" | "import" | null>(null);
  const [archive, setArchive] = useState("");
  const [historyError, setHistoryError] = useState<string | null>(null);
  /*
   * The file the last successful native export was written to, kept so the
   * path survives its toast: the full string rides on the hint line's title,
   * because a toast is gone in three seconds and a file location is the one
   * fact a reader may need long after.
   */
  const [exportedPath, setExportedPath] = useState<string | null>(null);
  const notify = useToast();

  /*
   * The chart's series, loaded once and then refreshed at most every ten
   * seconds. The counters poll every two seconds; a year of history is a
   * different weight of read, and a trend a few seconds behind is still a
   * trend. Resetting the counters changes `summary.requests`, which is what
   * brings the chart back in step with the table.
   */
  const [usageDays, setUsageDays] = useState<UsageDay[] | null>(null);
  const usageLoadedAt = useRef(0);
  useEffect(() => {
    if (Date.now() - usageLoadedAt.current < 10_000) return;
    usageLoadedAt.current = Date.now();
    api
      .usageDays(HEATMAP_DAYS)
      .then(setUsageDays)
      .catch(() => setUsageDays((current) => current ?? []));
  }, [summary.requests]);

  /*
   * The measure the traffic chart draws, owned here rather than inside
   * UsageCharts because the KPI row below is the chart's second controller:
   * one state, two switches (the chart's own segment and this row), so the two
   * can never disagree about which measure is on.
   */
  const [chartMetric, setChartMetric] = useState<ChartMetric>("requests");

  /*
   * Which cut of the ledger is showing. One table, two views — not two tables
   * stacked: the reader asks about either clients or models at a moment, the
   * columns are the same shape (requests, tokens, spend), and a second table
   * would be a second promise that both are worth scanning at once. The
   * segment lives in the section head because the switch IS the section's
   * subject, the way the chart's own segment names its measure.
   */
  const [ledgerDim, setLedgerDim] = useState<"client" | "model">("client");

  /*
   * Export, in three branches, because the command answers three ways:
   *
   * a path — the native save dialog ran, the archive is on disk, and the
   * confirmation is an info toast (the short form of the path in the line,
   * the full path on the hint below, which outlives the toast);
   *
   * null — the reader looked at the dialog and cancelled it. Cancelling a
   * save is a decision, not an event: no toast, no drawer, the page exactly
   * as it stood;
   *
   * a rejection — an older backend without the file command, or a dialog
   * that failed to open. The string-archive path still works there, so the
   * old textarea drawer opens and an error toast says why the flow changed
   * under the pointer; only if the fallback itself fails does the section's
   * error line take the raw text, as it always did.
   */
  const openExport = async () => {
    setHistoryError(null);
    try {
      const path = await api.exportUsageHistoryFile();
      if (path === null) return;
      setExportedPath(path);
      notify("info", t("activity.exported_to", { path: shortPath(path) }));
    } catch {
      notify("error", t("activity.export_file_failed"));
      try {
        setArchive(await api.exportUsageHistory());
        setHistory("export");
      } catch (fallbackError) {
        setHistoryError(String(fallbackError));
      }
    }
  };

  const openImport = () => {
    setArchive("");
    setHistoryError(null);
    setHistory("import");
  };

  const importArchive = async () => {
    if (!archive.trim()) return;
    const ok = await run(() => api.importUsageHistory(archive));
    if (ok) {
      setHistory(null);
      setArchive("");
    }
  };

  const clearClient = (client_id: string, client_name: string) =>
    setConfirm({
      title: t("activity.clear_client"),
      body: t("activity.clear_client_body", { name: client_name || client_id }),
      confirmLabel: t("activity.clear_client"),
      danger: true,
      onConfirm: () => void run(() => api.clearClientUsage(client_id)),
    });

  const loadLogs = useCallback(async () => {
    try {
      setLogs(await api.logs());
    } catch {
      setLogs([]);
    }
  }, []);

  useEffect(() => {
    if (logs === null) return;
    const timer = setInterval(loadLogs, 1500);
    return () => clearInterval(timer);
  }, [logs !== null, loadLogs]);

  const successRate =
    summary.requests > 0
      ? Math.round(((summary.requests - summary.failures) / summary.requests) * 1000) / 10
      : null;

  /*
   * Cache hit = summary.cache_read_tokens ÷ summary.input_tokens, the
   * backend's own aggregate (stats.rs applies the same accounting rule the
   * ledger applies per record, so summing per_model here again would be work
   * the summary has already done — and two roads to one number is one road
   * to disagreeing numbers). The denominator is cache-inclusive — the input
   * column already contains the cache reads — so the quotient is a true share
   * and can never pass 1; the "?" beside the label carries this formula,
   * because a percentage with an unstated denominator is a rumour.
   *
   * The dash is reserved for an undefined quotient — no input tokens recorded
   * at all, which means every request failed before any usage was reported —
   * and not for a zero. A zero with traffic behind it is what the counters
   * say (the upstream reported no cache reads), and hiding that behind "—"
   * would claim we cannot know what the counters do say.
   */
  const cacheInputTokens = summary.input_tokens;
  const cacheHitRate =
    cacheInputTokens > 0
      ? Math.round((summary.cache_read_tokens / cacheInputTokens) * 1000) / 10
      : null;

  const classifier = (() => {
    // Every side kind is Auto review traffic, whatever protocol it was asked in.
    const sides = summary.per_kind.filter((k) => isSideKind(k.kind as RequestKind));
    if (sides.length === 0) return undefined;
    return {
      kind: "auto_mode",
      requests: sides.reduce((n, k) => n + k.requests, 0),
      failures: sides.reduce((n, k) => n + k.failures, 0),
      avg_latency_ms: sides.length
        ? sides.reduce((n, k) => n + k.avg_latency_ms, 0) / sides.length
        : 0,
    };
  })();
  const open = recent.find((r) => r.id === openId) ?? null;

  /*
   * The ledger records every client that ever reached the gateway, and one
   * stray probe (a scanner, a misconfigured tool, a single request that
   * failed outright) would otherwise sit in the table beside OpenCode's
   * million tokens with equal billing. A client is a stray when it made one
   * request, failed it, and spent nothing: it gets its own folded table,
   * clear button included — junk is exactly what one wants to clear.
   */
  const isStray = (client: ClientTotals) =>
    client.requests <= 1 && client.failures >= client.requests && Object.keys(client.cost).length === 0;
  /*
   * Both cuts read biggest first. The backend hands the dimensions over in
   * map order, which churns as the two-second counters poll; a table the
   * reader is asked to click must hold still between polls, and "where did
   * it go" is answered loudest-first anyway.
   */
  const majorClients = summary.per_client
    .filter((c) => !isStray(c))
    .sort((a, b) => b.requests - a.requests || a.client_id.localeCompare(b.client_id));
  const strayClients = summary.per_client.filter(isStray);
  const ledgerModels = [...summary.per_model].sort(
    (a, b) => b.requests - a.requests || a.model_id.localeCompare(b.model_id),
  );

  /*
   * The model cut resolves ids to the names every other part of this page
   * uses. An id this config no longer knows — a model removed since it
   * served — falls back to the id itself; the requests still happened.
   */
  const modelLabel = (model_id: string) => {
    const index = snapshot.exposed_ids.indexOf(model_id);
    return index >= 0 ? modelName(snapshot.config.models[index]) : model_id;
  };

  /* The strip names the subject in words, the same two ways to be unnamed. */
  const clientLabel = (client: ClientTotals) =>
    client.client_name && client.client_name !== client.client_id
      ? client.client_name
      : t("activity.client_unknown");

  /*
   * A budget's scope in words, the Budgets table's own rendering: everything,
   * a tier in its readable name, or the provider's display name with the raw
   * id standing in for one the config has since dropped.
   */
  const budgetScopeName = (scope: BudgetScope): string =>
    scope.kind === "global"
      ? t("scope.global")
      : scope.kind === "tier"
        ? tierName(scope.tier, t)
        : snapshot.config.providers.find((p) => p.id === scope.id)?.name ?? scope.id;

  /*
   * Health joins the model cut by id, once per render. A model with no
   * health record draws no dot: the circuit breaker has never tripped and
   * the model has never failed — the absence of a mark is itself the
   * healthy reading, and a row of green dots saying so row after row is
   * furniture, the very thing that made the old health section feel like a
   * "no data" stall parked mid-page.
   */
  const healthByModel = new Map(health.map((h) => [h.model_id, h]));

  /*
   * The request list is a window over a much longer ledger, so it reads a
   * page at a time — cc-switch's log table, converted: one page size, a
   * clamp instead of a ceremony when the ledger shrinks under the reader,
   * and a failure filter because the first thing anyone does with a mixed
   * list is hunt for the red rows. The filter and the page reset together:
   * a page number is meaningless across a change of denominator.
   */
  const LOG_PAGE_SIZE = 50;
  const [page, setPage] = useState(0);
  const [failedOnly, setFailedOnly] = useState(false);
  /*
   * The stream's subject, set by clicking a ledger row, cleared by clicking
   * the row again or by the strip's dismiss above the stream. One subject at
   * a time: client and model are two answers to the same "where did it go",
   * and a reader who moves from one cut to the other has changed the
   * question, not stacked two of them. It stacks with the failure filter,
   * which narrows by outcome rather than by origin — together they answer
   * "this client's failures", the compound a reader actually hunts for. Any
   * change of denominator resets the page, because a page number is
   * meaningless across one.
   */
  const [streamClient, setStreamClient] = useState<string | null>(null);
  const [streamModel, setStreamModel] = useState<string | null>(null);
  /*
   * The facet pair — provider first, model cascaded inside it — and why it
   * and the ledger-row cut above take turns instead of stacking.
   *
   * Both are "show only X" lenses over the same list. Stacking them sounds
   * harmless until the two disagree: the row cut says "only DeepSeek's
   * requests", the facet says "only Aurora Relay's", and the conjunction is
   * an empty list while both controls keep claiming to be showing something.
   * Union semantics lies the other way round — an "only A" that quietly also
   * shows B's rows is worse than an honest empty list. Two "only"s cannot
   * both be true at once, so this page never lets them try: whichever lens
   * moved last owns the stream, and moving one clears the other. "Later
   * wins" is the only rule under which every control's state stays a true
   * sentence about the list below it. The reader who wanted a compound
   * question answered still has one: the facet's own provider→model cascade
   * is a conjunction the two selects can honestly express between them.
   */
  const [facetProvider, setFacetProvider] = useState<string | null>(null);
  const [facetModel, setFacetModel] = useState<string | null>(null);
  const toggleStreamClient = (client_id: string) => {
    setStreamModel(null);
    setStreamClient((current) => (current === client_id ? null : client_id));
    setFacetProvider(null);
    setFacetModel(null);
    setPage(0);
  };
  const toggleStreamModel = (model_id: string) => {
    setStreamClient(null);
    setStreamModel((current) => (current === model_id ? null : model_id));
    setFacetProvider(null);
    setFacetModel(null);
    setPage(0);
  };
  const clearStreamCut = () => {
    setStreamClient(null);
    setStreamModel(null);
    setPage(0);
  };
  const chooseFacetProvider = (provider: string | null) => {
    // Later wins: the facet just moved, so the ledger cut stands down. The
    // cascade resets too — a different provider's model list is a different
    // question, and the previous model choice is not an answer to it.
    setStreamClient(null);
    setStreamModel(null);
    setFacetModel(null);
    setFacetProvider(provider);
    setPage(0);
  };
  const chooseFacetModel = (model: string | null) => {
    setFacetModel(model);
    setPage(0);
  };
  const filtered = recent.filter(
    (r) =>
      (!failedOnly || !r.ok) &&
      (streamClient !== null
        ? recordClientId(r) === streamClient
        : streamModel !== null
          ? r.resolved_model === streamModel ||
            (streamModel === UNRESOLVED_MODEL_KEY && r.resolved_model === null)
          : true) &&
      (facetProvider !== null
        ? r.provider_name === facetProvider &&
          (facetModel === null ||
            r.resolved_model === facetModel ||
            (facetModel === UNRESOLVED_MODEL_KEY && r.resolved_model === null))
        : true),
  );

  /* The strip's subject in words, resolved while the row that set it exists. */
  const streamSubject = (() => {
    if (streamClient !== null) {
      const client = summary.per_client.find((c) => c.client_id === streamClient);
      return client ? clientLabel(client) : streamClient;
    }
    if (streamModel !== null) return modelLabel(streamModel);
    return null;
  })();
  /*
   * Facet counts, one pass over the kept window — cc-switch's dashboard does
   * the same with two Maps, zero backend. The basis is deliberately the
   * window *before* the failure segment: counting the failure-filtered list
   * instead would re-price every option whenever 仅失败 toggles (and flatten
   * them all into per-provider failure counts), so a figure the reader
   * compares across options would move under a filter that has nothing to
   * do with it. The counts answer "what is in the window"; the failure
   * segment and the ledger cut only narrow what is *shown*. The same goes
   * for the ledger cut, which cannot be active while the facets are (they
   * take turns), so "the window" is one stable denominator for both levels.
   *
   * Records with no provider — archives from before attribution existed,
   * probes that never reached an upstream — match no facet option and sit
   * outside every count: a menu that cannot offer them must not count them
   * under a provider they never had. The model bucket joins by
   * resolved_model, the same key the ledger's by-model cut and its row-click
   * filter use, with the backend's own "unresolved" bucket for the requests
   * that never resolved.
   */
  const facetCounts = (() => {
    const providers = new Map<string, number>();
    const models = new Map<string, Map<string, number>>();
    for (const record of recent) {
      const provider = record.provider_name;
      if (!provider) continue;
      providers.set(provider, (providers.get(provider) ?? 0) + 1);
      let bucket = models.get(provider);
      if (bucket === undefined) {
        bucket = new Map();
        models.set(provider, bucket);
      }
      const key = record.resolved_model ?? UNRESOLVED_MODEL_KEY;
      bucket.set(key, (bucket.get(key) ?? 0) + 1);
    }
    return { providers, models };
  })();

  /* Biggest first, the ledger's own ordering: the option a reader is here
   * for is the busy one, and a stable order keeps the menu from churning
   * between the two-second polls. */
  const facetByCount = (a: [string, number], b: [string, number]) =>
    b[1] - a[1] || a[0].localeCompare(b[0]);

  const facetProviderOptions = [
    { value: FACET_ALL, label: t("activity.facet_all_providers") },
    ...[...facetCounts.providers].sort(facetByCount).map(([name, count]) => ({
      value: name,
      label: `${name} · ${num(count)}`,
    })),
  ];

  /* The cascaded level: the selected provider's own models and their counts
   * within that provider's records — a question about one provider's
   * traffic, so its denominator is that provider, not the whole window. */
  const facetModelOptions =
    facetProvider === null
      ? []
      : [
          { value: FACET_ALL, label: t("activity.facet_all_models") },
          ...[...(facetCounts.models.get(facetProvider) ?? [])].sort(facetByCount).map(
            ([model, count]) => ({
              value: model,
              label: `${modelLabel(model)} · ${num(count)}`,
            }),
          ),
        ];

  const totalPages = Math.max(1, Math.ceil(filtered.length / LOG_PAGE_SIZE));
  const safePage = Math.min(page, totalPages - 1);
  const pageOf = (rows: typeof recent) =>
    rows.slice(safePage * LOG_PAGE_SIZE, (safePage + 1) * LOG_PAGE_SIZE);

  const logPane = (
    <Section title={t("activity.log_section")} hint={t("activity.log_hint")}>
      {logs === null ? (
        <Button kind="ghost" onClick={loadLogs}>
          {t("activity.show_log")}
        </Button>
      ) : (
        <>
          <pre className="log-view">{logs.join("\n")}</pre>
          <Button kind="ghost" onClick={() => setLogs(null)}>
            {t("activity.stop_follow")}
          </Button>
        </>
      )}
    </Section>
  );

  /*
   * The client cut: the ledger's durable dimension, keyed the way clearance
   * is keyed — `clear_client_usage` can erase a client's recorded history,
   * and that lever exists per client and nowhere else, so this is the only
   * view whose rows carry a Clear button. The model rows have no such
   * backend ability and do not pretend to.
   */
  const clientCut = (
    <>
      {summary.per_client.length === 0 ? (
        <p className="empty">{t("activity.nothing")}</p>
      ) : (
        <>
          {majorClients.length > 0 && (
            <div className="table-scroll">
              <table className="table">
                <thead>
                  <tr>
                    <th scope="col">{t("activity.client")}</th>
                    <th scope="col">{t("models.requests")}</th>
                    <th scope="col">{t("field.input")} / {t("field.output")}</th>
                    <th scope="col">{t("activity.spend")}</th>
                    <th scope="col">{t("common.actions")}</th>
                  </tr>
                </thead>
                <tbody>
                  {majorClients.map((client) => (
                    <tr
                      key={client.client_id}
                      className={`selectable${streamClient === client.client_id ? " selected" : ""}`}
                      onClick={rowActivate(() => toggleStreamClient(client.client_id))}
                    >
                      <td>
                        <ClientNameCell
                          client={client}
                          pressed={streamClient === client.client_id}
                          onToggle={() => toggleStreamClient(client.client_id)}
                        />
                      </td>
                      <td className="mono nowrap">
                        {num(client.requests)}
                        {client.failures > 0 && (
                          <Badge tone="danger">{t("activity.n_failed", { n: client.failures })}</Badge>
                        )}
                      </td>
                      <td className="muted mono">
                        {num(client.input_tokens)} / {num(client.output_tokens)}
                      </td>
                      <td className="mono nowrap">{totalsText(client.cost)}</td>
                      <td>
                        <Button
                          kind="ghost"
                          ariaLabel={t("activity.clear_client_named", {
                            name: client.client_name || client.client_id,
                          })}
                          onClick={() => clearClient(client.client_id, client.client_name)}
                        >
                          {t("activity.clear_client")}
                        </Button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {strayClients.length > 0 && (
            <Disclosure title={t("activity.minor_clients", { n: strayClients.length })}>
              <div className="table-scroll">
                <table className="table">
                  <tbody>
                    {strayClients.map((client) => (
                      <tr
                        key={client.client_id}
                        className={`selectable${streamClient === client.client_id ? " selected" : ""}`}
                        onClick={rowActivate(() => toggleStreamClient(client.client_id))}
                      >
                        <td>
                          <ClientNameCell
                            client={client}
                            pressed={streamClient === client.client_id}
                            onToggle={() => toggleStreamClient(client.client_id)}
                          />
                        </td>
                        <td className="mono nowrap">
                          {num(client.requests)}
                          {client.failures > 0 && (
                            <Badge tone="danger">{t("activity.n_failed", { n: client.failures })}</Badge>
                          )}
                        </td>
                        <td className="muted mono">
                          {num(client.input_tokens)} / {num(client.output_tokens)}
                        </td>
                        <td className="mono nowrap">{totalsText(client.cost)}</td>
                        <td>
                          <Button
                            kind="ghost"
                            ariaLabel={t("activity.clear_client_named", {
                              name: client.client_name || client.client_id,
                            })}
                            onClick={() => clearClient(client.client_id, client.client_name)}
                          >
                            {t("activity.clear_client")}
                          </Button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </Disclosure>
          )}
        </>
      )}
    </>
  );

  /*
   * The model cut: the routing dimension, where the ledger meets the circuit
   * breaker. The health dot is joined from snapshot.health per row — cool-
   * down warns, failures outnumbering successes marks danger, everything
   * else is the quiet ok. No clear button: there is no per-model clearance
   * in the backend, and a row that cannot clear must not wear the button
   * that says so.
   */
  const modelCut =
    ledgerModels.length > 0 ? (
      <div className="table-scroll">
        <table className="table">
          <thead>
            <tr>
              <th scope="col">{t("field.model_name")}</th>
              <th scope="col">{t("models.requests")}</th>
              <th scope="col">{t("field.input")} / {t("field.output")}</th>
              <th scope="col">{t("activity.spend")}</th>
              <th scope="col">{t("activity.health_col")}</th>
            </tr>
          </thead>
          <tbody>
            {ledgerModels.map((m) => {
              const health = healthByModel.get(m.model_id);
              /*
               * The dot's words, for the pointer and for the screen reader at
               * once: the ok/failed counts, the cooling state when one is
               * running, the last error when there is one. The old section
               * spent a whole row per model saying this; a mark that cannot
               * answer "why" on hover is a colour, and a colour is invisible
               * to exactly the reader who needs it most.
               */
              const healthState = health
                ? [
                    health.total_success + health.total_failure > 0
                      ? t("activity.ok_failed", {
                          ok: num(health.total_success),
                          failed: num(health.total_failure),
                        })
                      : null,
                    health.cooldown_remaining_secs > 0
                      ? t(
                          (health.rate_limit_remaining_secs ?? 0) > 0
                            ? "activity.rate_cooling"
                            : "activity.cooling",
                          { n: health.cooldown_remaining_secs },
                        )
                      : null,
                    health.last_error,
                  ]
                    .filter(Boolean)
                    .join(" · ")
                : undefined;
              return (
                <tr
                  key={m.model_id}
                  className={`selectable${streamModel === m.model_id ? " selected" : ""}`}
                  onClick={rowActivate(() => toggleStreamModel(m.model_id))}
                >
                  <td>
                    <LedgerName
                      pressed={streamModel === m.model_id}
                      onToggle={() => toggleStreamModel(m.model_id)}
                      title={m.model_id}
                    >
                      <span className="mono">{modelLabel(m.model_id)}</span>
                    </LedgerName>
                  </td>
                  <td className="mono nowrap">
                    {num(m.requests)}
                    {m.failures > 0 && (
                      <Badge tone="danger">{t("activity.n_failed", { n: m.failures })}</Badge>
                    )}
                  </td>
                  <td className="muted mono">
                    {num(m.input_tokens)} / {num(m.output_tokens)}
                  </td>
                  <td className="mono nowrap">{totalsText(m.cost)}</td>
                  <td>
                    {health && (
                      <span title={healthState}>
                        <StatusDot
                          tone={
                            health.cooldown_remaining_secs > 0
                              ? "warn"
                              : health.total_failure > health.total_success
                                ? "danger"
                                : "ok"
                          }
                          state={healthState}
                        />
                      </span>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    ) : (
      <p className="empty">{t("activity.nothing")}</p>
    );

  const usagePane = (
    <>
      {summary.requests > 0 && (
        <Section title={t("activity.traffic")} hint={t("activity.traffic_hint")}>
          <UsageCharts
            days={usageDays}
            metric={chartMetric}
            onMetricChange={setChartMetric}
          />
        </Section>
      )}

      {classifier && classifier.requests > 0 && (
        <p className="section-hint">
          <StatusDot tone="ok" />{" "}
          {t("activity.review_line", {
            n: num(classifier.requests),
            failed: classifier.failures,
            latency: ms(classifier.avg_latency_ms),
          })}
        </p>
      )}

      {/*
       * The ledger: one table, two cuts. This replaces three sections — the
       * durable usage history, the spend table and the model health list —
       * because they answered one question ("where did the usage and the
       * money go?") with the same numbers filed under three headings: spend
       * appeared once as a total, once per client, once per model, and an
       * empty health list stood in the middle of a page that had traffic.
       *
       * The cuts follow the two lifetimes the data actually has. Per client
       * is the durable dimension: usage history is kept per client across
       * restarts and cleared per client, so that is the view with the Clear
       * buttons and the export/import verbs. Per model is the routing
       * dimension: models come and go with the config, their totals are what
       * a routing decision acts on, and the circuit breaker's state joins
       * them here as one dot instead of a section that mostly said "no
       * data".
       *
       * The spend total lives in the KPI row above and nowhere else. A
       * figure that appears under three headings is three figures to
       * reconcile; a ledger shows each number once, at the granularity the
       * reader asked for.
       *
       * It sits between the traffic charts and the stream — the page reads
       * in zoom order: the trend, the sums behind it, then the individual
       * requests a sum is made of — and clicking a ledger row is the walk
       * from the second level to the third.
       */}
      {(summary.per_client.length > 0 || summary.per_model.length > 0) && (
        <Section
          title={t("activity.ledger")}
          hint={t("activity.ledger_hint")}
          actions={
            <div className="row gap">
              {/*
               * The segment that switches the table's cut sits beside the
               * export/import verbs it shares the head with — one control
               * choosing what the table counts, two verbs administering the
               * records underneath. Both are head furniture; the body is
               * only ever the table.
               */}
              <Segment<"client" | "model">
                ariaLabel={t("activity.ledger_cut")}
                value={ledgerDim}
                onChange={(next) => setLedgerDim(next)}
                options={[
                  { value: "client", label: t("activity.by_client") },
                  { value: "model", label: t("activity.by_model") },
                ]}
              />
              <button type="button" className="linky" onClick={() => void openExport()}>
                {t("activity.export_history")}
              </button>
              <button type="button" className="linky" onClick={openImport}>
                {t("activity.import_history")}
              </button>
            </div>
          }
        >
          {historyError && <p className="field-error">{historyError}</p>}
          {/*
            Where the last native export landed. The toast is the moment's
            confirmation; this line is the record — the short form in the
            text, the full path on the title, because "which folder was it"
            is asked long after three seconds have passed.
          */}
          {exportedPath && (
            <p className="field-hint" title={exportedPath}>
              {t("activity.exported_to", { path: shortPath(exportedPath) })}
            </p>
          )}
          {ledgerDim === "client" ? clientCut : modelCut}
        </Section>
      )}

      {/*
       * The stream's active cut, one dismissible line above the filters. It
       * states what the list below now means — a filtered list that does not
       * say so reads as a shorter internet — and carries the way back.
       */}
      {recent.length > 0 && streamSubject !== null && (
        <p className="filter-strip">
          <span>{t("activity.stream_only", { subject: streamSubject })}</span>
          <button
            type="button"
            className="linky"
            aria-label={t("activity.stream_clear")}
            onClick={clearStreamCut}
          >
            ×
          </button>
        </p>
      )}

      {recent.length > 0 && (
        <div className="list-toolbar">
          <Segment<"all" | "failed">
            ariaLabel={t("activity.filter")}
            value={failedOnly ? "failed" : "all"}
            onChange={(next) => {
              setFailedOnly(next === "failed");
              setPage(0);
            }}
            options={[
              { value: "all", label: t("activity.filter_all") },
              { value: "failed", label: t("activity.filter_failed") },
            ]}
          />
          {/*
           * The facets: the direct jump the ledger row-click is not. A row
           * click drills from a sum already on screen; a facet names its
           * subject outright, its options priced with the counts a reader
           * needs before choosing. Selecting one stands the ledger cut down
           * (later wins — see the facet state above), and the model select
           * exists only inside a provider, the cascade cc-switch's dashboard
           * draws: "which model" is a question about a provider's traffic.
           * Both stack with the failure segment, which narrows by outcome
           * rather than by origin.
           */}
          <Select
            ariaLabel={t("activity.facet_provider")}
            value={facetProvider ?? FACET_ALL}
            onChange={(next) => chooseFacetProvider(next === FACET_ALL ? null : next)}
            options={facetProviderOptions}
          />
          {facetProvider !== null && (
            <Select
              ariaLabel={t("activity.facet_model")}
              value={facetModel ?? FACET_ALL}
              onChange={(next) => chooseFacetModel(next === FACET_ALL ? null : next)}
              options={facetModelOptions}
            />
          )}
        </div>
      )}

      <div className="list">
        <div className="timeline">
          {filtered.length === 0 ? (
            <div style={{ padding: "14px 16px" }}>
              <p className="empty">
                {streamSubject !== null || facetProvider !== null
                  ? /*
                     * The subject's own message, ahead of the generic ones:
                     * the ledger reaches further back than the kept window,
                     * so a client with recorded history can still have
                     * nothing here, and a facet chosen from the window can
                     * still come up empty once 仅失败 narrows it further —
                     * the provider's rows are all successes. "Nothing yet"
                     * would be a lie with a ledger and a count on the same
                     * screen saying otherwise.
                     */
                    t("activity.no_match_window")
                  : failedOnly && recent.length > 0
                    ? t("activity.no_failures")
                    : t("activity.nothing")}
              </p>
            </div>
          ) : (
            pageOf(filtered).map((r) => (
              <TimelineRow key={r.id} record={r} onOpen={() => setOpenId(r.id)} />
            ))
          )}
        </div>
        {/*
         * The pager, cc-switch's shape: one quiet line under the rows — where
         * you are, how many there are, one step each way. It hides itself on
         * a single page, and a page that runs past the end (the poll brought
         * a smaller list) snaps back rather than showing an empty table.
         */}
        {totalPages > 1 && (
          <div className="pager">
            <span className="muted">
              {t("activity.page_status", {
                from: safePage * LOG_PAGE_SIZE + 1,
                to: Math.min((safePage + 1) * LOG_PAGE_SIZE, filtered.length),
                total: num(filtered.length),
              })}
            </span>
            <div className="row gap">
              <button
                type="button"
                className="linky"
                disabled={safePage === 0}
                onClick={() => setPage(safePage - 1)}
              >
                {t("activity.prev_page")}
              </button>
              <button
                type="button"
                className="linky"
                disabled={safePage >= totalPages - 1}
                onClick={() => setPage(safePage + 1)}
              >
                {t("activity.next_page")}
              </button>
            </div>
          </div>
        )}
      </div>

    </>
  );

  return (
    <>
      <ConfirmDialog request={confirm} onClose={() => setConfirm(null)} />

      {/*
        The headline numbers moved into the KPI row below. The lede used to
        carry "N requests · X% succeeded · in/out tokens" and the row repeats
        four of those five figures — the same numbers twice in the first
        hundred pixels of the page, each read once and discounted once. So the
        two split the work along their shapes: the lede keeps the one thing a
        figure cannot say (the period the totals cover), and the row keeps the
        figures at display size.
      */}
      <PageHead
        lede={
          summary.requests === 0
            ? t("activity.lede_none")
            : t("activity.lede_since", {
                since: new Date(summary.since).toLocaleString([], {
                  dateStyle: "medium",
                  timeStyle: "short",
                }),
              })
        }
        // Nothing to reset while nothing has been counted; a destructive
        // button that can only clear zeros teaches doubt about every other.
        actions={
          summary.requests === 0 ? undefined : (
            <Button
              kind="ghost"
              onClick={() =>
                setConfirm({
                  title: t("confirm.reset_stats"),
                  body: t("confirm.reset_stats_body"),
                  confirmLabel: t("activity.reset"),
                  danger: true,
                  onConfirm: () => void run(api.clearStats),
                })
              }
            >
              {t("activity.reset")}
            </Button>
          )
        }
      />

      {summary.requests > 0 && (
        <div className="kpi-row" role="group" aria-label={t("activity.kpi_row")}>
          {/*
            The KPI row, cc-switch's UsageHero converted: the page's headline
            figures are the chart's gears, and clicking one drives the measure
            the traffic chart below draws. Not all of the five have a per-day
            series to drive, though — the durable ledger buckets days by
            requests, failures and tokens, and since the cache columns joined
            UsageDay a day's cache reads/writes are a series too. What still
            is not wired here is a per-currency cost view; UsageDay already
            carries those totals. The cache KPI is a ratio (reads over
            cache-inclusive input), rather than the cache-token series. So
            请求数 and Tokens stay the two controllers (they carry the chart's
            selected state, both ways — the chart's own segment moves them
            too), the chart's 缓存 tier is driven from its own segment alone,
            and success rate, cache hit and spend remain readouts. A
            highlighted cell that leaves the chart where it stood would be a
            control claiming an action it cannot perform, which is the same
            lie the fabricated series would have told, one step further from
            the pixels.
          */}
          <Kpi
            label={t("activity.metric_requests")}
            value={compactCount(summary.requests)}
            title={`${num(summary.requests)} · ${t("activity.n_failed", { n: summary.failures })}`}
            selected={chartMetric === "requests"}
            onSelect={() => setChartMetric("requests")}
          />
          <Kpi
            label={t("activity.kpi_success")}
            value={successRate === null ? t("common.dash") : `${successRate}%`}
            title={
              successRate === null
                ? undefined
                : t("activity.ok_failed", {
                    ok: summary.requests - summary.failures,
                    failed: summary.failures,
                  })
            }
          />
          <Kpi
            label={t("activity.kpi_cache")}
            value={cacheHitRate === null ? t("common.dash") : `${cacheHitRate}%`}
            title={
              cacheHitRate === null
                ? undefined
                : `${t("activity.cache_read")} ${num(summary.cache_read_tokens)} / ${t("activity.input_tokens")} ${num(cacheInputTokens)}`
            }
            help={t("activity.kpi_cache_help")}
          />
          <Kpi
            label={t("activity.metric_tokens")}
            value={compactCount(summary.input_tokens + summary.output_tokens)}
            title={`${t("activity.input_tokens")} ${num(summary.input_tokens)} · ${t("activity.output_tokens")} ${num(summary.output_tokens)}`}
            selected={chartMetric === "tokens"}
            onSelect={() => setChartMetric("tokens")}
          />
          {/*
            Spend is exact at two decimals — a currency figure abbreviated to
            "149" would round away the cents a budget is watched with — and
            totalsText is already the full, unrounded statement. What a title
            can still add is the caveat on the estimate itself: unpriced
            requests are the ones whose cost is wholly or partly unknown
            (stats.rs), so their true spend is not inside the figure shown.
            The note rides the title only when there is something unpriced —
            a caveat attached to a clean total would invent a problem.
          */}
          <Kpi
            label={t("activity.estimated")}
            value={totalsText(summary.cost)}
            title={
              summary.unpriced_requests > 0
                ? `${totalsText(summary.cost)} · ${t("activity.kpi_unpriced", { n: summary.unpriced_requests })}`
                : undefined
            }
          />
        </div>
      )}

      {/*
       * One muted line, not a section. The ledger's reliability is metadata
       * about every number above it — a titled section promised a body of
       * ledger content that lives elsewhere (per-client history and spend
       * render further down), and a heading over a single sentence read as
       * a promise with nothing behind it. The detail drawer keeps the real
       * per-request accounting facts, where "unknown cost" means something.
       */}
      {snapshot.accounting && (
        <>
          <p className={snapshot.accounting.available ? "field-hint" : "field-error"} role="status">
            {t(snapshot.accounting.durable ? "accounting.durable" : "accounting.unavailable")}
            {snapshot.accounting.pending_settlements > 0 && ` · ${t("accounting.pending", { n: snapshot.accounting.pending_settlements })}`}
            {snapshot.accounting.unreconciled_attempts > 0 && ` · ${t("accounting.unreconciled", { n: snapshot.accounting.unreconciled_attempts })}`}
          </p>
          {snapshot.accounting.error && <p className="field-error">{snapshot.accounting.error}</p>}
        </>
      )}

      {/*
       * The budgets, read-only at home distance. Simple mode has no budgets
       * tab, and a budget that is silently shaping routing while its spend
       * climbs is exactly the fact a first user needs on screen: one muted
       * line per budget, the period named because two budgets may cover the
       * same scope on different windows, and the badge when the limit is
       * passed. Editing stays on the geek tab — this line claims no lever.
       */}
      {simpleOnly && snapshot.budgets.length > 0 && (
        <>
          {snapshot.budgets.map(({ budget, spent, used }) => (
            <p className="field-hint" key={budget.id}>
              {t("budget.readonly", {
                scope: budgetScopeName(budget.scope),
                spent: money(spent.currency, spent.amount),
                limit: money(budget.limit.currency, budget.limit.amount),
                used: Math.round(used * 1000) / 10,
              })}
              {` · ${t(budget.period === "day" ? "period.day" : "period.month")}`}
              {used >= 1 && (
                <>
                  {" "}
                  <Badge tone="danger">{t("budget.used_up")}</Badge>
                </>
              )}
            </p>
          ))}
        </>
      )}

      {/*
        The drawer is deliberately outside the strip. It is opened from a row in
        the usage pane, so it belongs to the page rather than to whichever pane
        happens to be showing — inside, switching to the log would close the
        request the reader was reading.
      */}
      <Tabs
        value={simpleOnly ? "usage" : tab}
        onChange={(next) => setTab(next as typeof tab)}
        ariaLabel={t("activity.tab_group")}
        tabs={
          simpleOnly
            ? [{ value: "usage", label: t("activity.tab_usage"), content: usagePane }]
            : [
                { value: "usage", label: t("activity.tab_usage"), content: usagePane },
                { value: "log", label: t("activity.tab_log"), content: logPane },
                { value: "budgets", label: t("settings.tab_budgets"), content: <Budgets snapshot={snapshot} save={save} busy={busy} /> },
              ]
        }
      />

      {history && (
        <Drawer
          title={history === "export" ? t("activity.export_history") : t("activity.import_history")}
          onClose={() => setHistory(null)}
        >
          <p className="field-hint">
            {history === "export" ? t("activity.export_hint") : t("activity.import_hint")}
          </p>
          <textarea
            className="text-input"
            style={{ width: "100%", minHeight: 280, fontFamily: "var(--font-mono)" }}
            value={archive}
            readOnly={history === "export"}
            placeholder={history === "import" ? t("activity.import_placeholder") : undefined}
            onChange={(event) => setArchive(event.target.value)}
          />
          <div className="controls" style={{ marginTop: 12 }}>
            {history === "export" ? (
              <Button onClick={() => void api.copy(archive)}>{t("common.copy")}</Button>
            ) : (
              <Button onClick={() => void importArchive()} disabled={!archive.trim()}>
                {t("activity.import_history")}
              </Button>
            )}
          </div>
        </Drawer>
      )}

      {open && <RequestDrawer record={open} onClose={() => setOpenId(null)} />}
    </>
  );
}

function TimelineRow({
  record,
  onOpen,
}: {
  record: RequestRecord;
  onOpen: () => void;
}) {
  const isAuto = isSideKind(record.kind);
  const { t } = useI18n();
  const speed = tps(record);
  return (
    <button type="button" className="timeline-item" onClick={onOpen}>
      <span className="timeline-time">
        {new Date(record.at).toLocaleTimeString([], { hour12: false })}
      </span>
      <span className={`timeline-kind ${isAuto ? "auto-mode" : ""}`}>
        {isAuto ? t("kind.review") : t("kind.main")}
      </span>
      <span className="timeline-main-cell">
        {/*
         * The dot is the row's only sign of whether the request worked — the
         * row carries the model, the provider and the latency, and nothing
         * else said "this one failed". A dot is a colour, and a colour is
         * invisible to a screen reader, so the outcome is said in words here.
         */}
        <StatusDot
          tone={record.ok ? "ok" : "danger"}
          state={record.ok ? t("status.succeeded") : t("status.failed")}
        />
        <span className="timeline-model">{record.model_name ?? record.resolved_model ?? record.requested_model}</span>
        {record.provider_name && <span className="timeline-provider">{record.provider_name}</span>}
        {record.attempts > 1 && (
          <span className="timeline-provider">{t("activity.tries", { n: record.attempts })}</span>
        )}
      </span>
      {/*
       * First token and generation speed sit beside the latency because they
       * are timing figures too. Each renders only when the record can say it
       * honestly — the same convention as the cost cell: a figure the data
       * does not support renders nothing, not a dash or a zero (cc-switch's
       * rule that a missing first-token time means "no speed", never 0). Both
       * are ok-gated like cost: a failed row's meta reads its status code,
       * not performance numbers.
       */}
      <span className="timeline-meta">
        <span>{ms(record.latency_ms)}</span>
        {record.ok && record.stream && record.ttft_ms !== null && (
          <span>{t("activity.ttft", { v: ms(record.ttft_ms) })}</span>
        )}
        {record.ok && speed !== null && <span>{Math.round(speed)} tok/s</span>}
        {record.ok ? (
          record.cost ? <span>{costText(record.cost)}</span> : null
        ) : (
          <span>{record.status}</span>
        )}
      </span>
    </button>
  );
}

/**
 * Cache hit rate as a percent of the cache-inclusive input total: stats.rs
 * documents `cache_read_tokens` as a subset of `input_tokens`, never an
 * addition, so the denominator is the input figure itself. One decimal is
 * kept when it has one — a 99.6% hit must not round into a claimed-perfect
 * 100%, which cc-switch reserves for a true full read of the prompt.
 */
function cacheHitRate(read: number, input: number): string {
  const pct = Math.round((read / input) * 1000) / 10;
  return `${Number.isInteger(pct) ? pct : pct.toFixed(1)}%`;
}

/** One request's whole story: the three model names, the outcome, the usage. */
function RequestDrawer({ record, onClose }: { record: RequestRecord; onClose: () => void }) {
  const { t } = useI18n();
  return (
    <Drawer
      title={
        <>
          <StatusDot
            tone={record.ok ? "ok" : "danger"}
            state={record.ok ? t("status.succeeded") : t("status.failed")}
          />
          {isSideKind(record.kind) ? t("activity.review") : t("activity.request")}
        </>
      }
      onClose={onClose}
    >
      <KeyValue
        rows={[
          [t("activity.asked_for"), <span className="mono">{record.requested_model}</span>],
          [
            t("activity.answered_by"),
            record.resolved_model ? (
              <span className="mono">{record.resolved_model}</span>
            ) : (
              t("common.dash")
            ),
          ],
          [t("models.provider"), record.provider_name ?? t("common.dash")],
          [
            t("activity.kind"),
            isSideKind(record.kind) ? t("kind.check_kv") : t("kind.main_kv"),
          ],
          ["API", record.ingress],
          [
            t("activity.result"),
            record.ok ? (
              <Badge tone="ok">200</Badge>
            ) : (
              <Badge tone="danger">{record.status}</Badge>
            ),
          ],
          [t("activity.attempts"), <span className="mono">{record.attempts}</span>],
        ]}
      />

      {record.error && <p className="field-hint">{record.error}</p>}

      <Section title={t("activity.timing")}>
        <KeyValue
          rows={[
            [t("activity.total"), <span className="mono">{ms(record.latency_ms)}</span>],
            [
              t("activity.first_token"),
              record.stream ? (
                <span className="mono">{ms(record.ttft_ms)}</span>
              ) : (
                t("common.dash")
              ),
            ],
            [t("activity.streamed"), record.stream ? t("common.yes") : t("common.no")],
          ]}
        />
      </Section>

      <Section title={t("activity.usage")}>
        <KeyValue
          rows={[
            [t("activity.input_tokens"), <span className="mono">{num(record.usage.input_tokens)}</span>],
            [t("activity.output_tokens"), <span className="mono">{num(record.usage.output_tokens)}</span>],
            [t("activity.cache_read"), <span className="mono">{num(record.usage.cache_read_tokens)}</span>],
            [t("activity.cache_write"), <span className="mono">{num(record.usage.cache_write_tokens)}</span>],
            /*
             * A ratio needs a prompt to be a ratio over: with zero input
             * tokens there is no rate, so the row stays out entirely instead
             * of printing 0% — a request without prompt tokens did not miss
             * the cache, it had nothing to hit.
             */
            ...(record.usage.input_tokens > 0
              ? ([
                  [
                    t("activity.cache_hit_rate"),
                    <span className="mono" title={t("activity.cache_hit_hint")}>
                      {cacheHitRate(record.usage.cache_read_tokens, record.usage.input_tokens)}
                    </span>,
                  ],
                ] as [ReactNode, ReactNode][])
              : []),
            [
              t("activity.cost"),
              record.cost ? (
                <span className="mono">{costText(record.cost)}</span>
              ) : (
                t("activity.unpriced")
              ),
            ],
          ]}
        />
      </Section>
      {record.accounting && (
        <Section title={t("accounting.title")}>
          <KeyValue rows={[
            [t("accounting.state"), t(`accounting.${record.accounting.persistence}`)],
            [t("accounting.known_cost"), totalsText(record.accounting.known_cost)],
            [t("accounting.unknown_cost"), num(record.accounting.unknown_cost_attempts)],
            [t("accounting.unknown_usage"), num(record.accounting.unknown_usage_attempts)],
            [t("accounting.identity"), [record.account_id, record.inference_key_id].filter(Boolean).join(" / ") || t("common.dash")],
          ]} />
          <p className="field-hint">{t("accounting.no_replay")}</p>
        </Section>
      )}
    </Drawer>
  );
}

/**
 * One figure of the KPI row: a small muted label over a display-size value,
 * separated from its neighbours by a hairline rather than a card.
 *
 * With `onSelect` the cell is a chart controller — a pressed button whose
 * pressed state IS the chart's measure, not a memory of its own. Without one
 * the cell is a readout and renders as plain content: no pointer, no pressed
 * state, no tab stop. A cell that activates and leaves the chart where it
 * stood would promise an action the row cannot perform.
 */
function Kpi({
  label,
  value,
  title,
  help,
  selected,
  onSelect,
}: {
  label: string;
  value: string;
  /** The exact figure behind an abbreviated value, on hover. */
  title?: string;
  /** A formula note, rendered as a "?" beside the label. */
  help?: string;
  selected?: boolean;
  onSelect?: () => void;
}) {
  const body = (
    <>
      <span className="kpi-label">
        {label}
        {help && (
          // The "?" carries its own title, which also wins while the pointer
          // rests on it inside a titled cell — the formula is what a reader
          // parked on the mark is asking for, not the figure again. It stays
          // in the accessibility tree on purpose: the formula is content,
          // and a screen-reader user is the reader least likely to have it
          // already.
          <span className="kpi-help" title={help}>
            ?
          </span>
        )}
      </span>
      <span className="kpi-value">{value}</span>
    </>
  );
  if (onSelect === undefined) {
    return (
      <div className="kpi-cell" title={title}>
        {body}
      </div>
    );
  }
  return (
    <button
      type="button"
      className="kpi-cell kpi-trigger"
      aria-pressed={selected}
      title={title}
      onClick={onSelect}
    >
      {body}
    </button>
  );
}
