/** The add-provider form. Owns its drafts; hands the save to the queue. */
import { useEffect, useMemo, useRef, useState } from "react";
import { type AppConfig, type Provider, type ProviderKind } from "../../api";
import { useI18n } from "../../i18n";
import { Banner, Button, Field, Section, Segment } from "../../components";
import {
  defaultBaseUrl,
  kindsFor,
} from "./kindMeta";
import { createAddProviderSubmission } from "./addProvider";

/**
 * The add-a-provider form, with its own drafts.
 *
 * The drafts used to live in the page above, so typing a provider name
 * re-rendered the whole list — and every row of it re-counted the models it
 * owns, which is a walk over every model in the configuration. Nothing above
 * this form reads a draft, so the form owns them now and a keystroke costs
 * one input.
 *
 * The form is still the same form: the same fields, in the same order, with
 * the same names, and the provider it adds still opens its own drawer. What
 * changed is when: the drawer opens only after the save that added the
 * provider has landed, and a refused save leaves the form and its draft
 * standing for a retry.
 */
export function AddProviderForm({
  providers,
  save,
  busy,
  opencodeAvailable,
  onAdded,
}: {
  providers: Provider[];
  save: (mutate: (config: AppConfig) => AppConfig | null) => Promise<boolean>;
  busy: boolean;
  /** Whether this build can talk to OpenCode's gateway at all. */
  opencodeAvailable: boolean;
  /** The provider that was added, so the page can open its drawer. */
  onAdded: (id: string) => void;
}) {
  const { t } = useI18n();
  const [newName, setNewName] = useState("");
  const [newKind, setNewKind] = useState<ProviderKind>("openai_compatible");
  const [newBaseUrl, setNewBaseUrl] = useState("");
  const [nameError, setNameError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  // The form folds the moment its work ends, so a save that lands after the
  // fold — or after the page itself is gone — must not touch state that no
  // longer exists.
  const mounted = useRef(true);
  useEffect(() => {
    // StrictMode's mount–cleanup–mount reruns this pair on the same ref.
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const submission = useMemo(() => createAddProviderSubmission(save), [save]);

  const addProvider = async () => {
    // One intent, one write: while a save is in flight the press is ignored,
    // not queued behind itself.
    if (submission.inFlight()) return;
    setSaveError(null);
    setSubmitting(true);
    try {
      const outcome = await submission.run(
        { name: newName, baseUrl: newBaseUrl, kind: newKind },
        providers,
      );
      if (!mounted.current) return;
      if (outcome.kind === "added") {
        // A real add only. The drawer that opens over this form is where the
        // credential is filled in next, so the draft is cleared and the new
        // provider's own drawer opens on it.
        setNewName("");
        setNewBaseUrl("");
        setNameError(null);
        setSaveError(null);
        onAdded(outcome.id);
      } else if (outcome.kind === "duplicate") {
        setNameError(t("providers.duplicate", { id: outcome.id }));
      } else if (outcome.kind === "name_required") {
        setNameError(t("providers.name_required"));
      } else if (outcome.kind === "write_failed") {
        // The write was refused or dropped; the draft stays as typed for a
        // retry. The queue's own error is already on the window — this line
        // ties the refusal to the form that made the attempt.
        setSaveError(t("providers.add_failed"));
      }
      // `ignored`: another intent is mid-save, and this press made no write.
    } finally {
      if (mounted.current) setSubmitting(false);
    }
  };

  return (
    <Section title={t("providers.add_section")} hint={t("providers.add_hint")}>
      {saveError && <Banner tone="danger">{saveError}</Banner>}
      <div className="controls">
        <Field label={t("field.name")} danger={Boolean(nameError)} hint={nameError ?? t("providers.name_optional")}>
          <input
            value={newName}
            placeholder={t("providers.name_placeholder")}
            className={nameError ? "input-error" : undefined}
            onChange={(e) => {
              setNewName(e.currentTarget.value);
              setNameError(null);
            }}
            onKeyDown={(e) => e.key === "Enter" && addProvider()}
          />
        </Field>
        <Field label={t("field.api_dialect")}>
          <Segment
            ariaLabel={t("field.api_dialect")}
            value={newKind}
            onChange={(kind) => setNewKind(kind)}
            options={kindsFor(opencodeAvailable).map((k) => ({ value: k.id, label: t(k.labelKey) }))}
          />
        </Field>
        <Field label={t("field.base_url")} hint={t("field.base_url_hint")}>
          <input
            value={newBaseUrl}
            placeholder={defaultBaseUrl(newKind)}
            onChange={(e) => setNewBaseUrl(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && addProvider()}
          />
        </Field>
        <div className="field-actions">
          <Button
            kind="primary"
            onClick={addProvider}
            disabled={busy || submitting || (!newName.trim() && !newBaseUrl.trim())}
          >
            {t("providers.add")}
          </Button>
        </div>
      </div>
    </Section>
  );
}
