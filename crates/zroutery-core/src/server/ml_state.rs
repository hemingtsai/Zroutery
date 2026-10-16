//! Model attachment, promotion rounds, shadow verdicts, and the ML status the console reads.

use super::projection_log::ProjectionLog;
use super::shadow_candidate::*;
use super::state::AppState;
use std::collections::BTreeMap;

impl AppState {
    /// Bring the serving model back in line with the durable pointer.
    ///
    /// Returns whether a model is attached afterwards.
    ///
    /// This exists because a rollback that only rewrites the pointer is a lie:
    /// the router would keep ranking with the model the operator just withdrew,
    /// and every status document would then report a state the process is not
    /// in. The same seam is used after a promotion for the same reason.
    ///
    /// A pointer that cannot be read is *not* treated as "no model" — it leaves
    /// whatever is attached alone and reports the fault, because the one thing an
    /// operator must not lose by a transient read error is a model they chose.
    #[cfg(feature = "ml")]
    pub fn reload_active_model(&self) -> crate::ml::ReloadOutcome {
        let store = match self.active_models.as_ref() {
            Some(store) => store,
            None => {
                return crate::ml::ReloadOutcome::fault(
                    self.ml_routing.is_attached(),
                    "no durable state directory is configured, so no model can be attached",
                )
            }
        };
        let outcome = match store.active() {
            Ok(Some(predictor)) => {
                let commit = predictor.commit_id().as_str().to_string();
                if self.config().ml_routing.enabled {
                    self.ml_routing.attach(predictor);
                } else {
                    // Installed but deliberately inert. Withdrawing rather than
                    // leaving a stale predictor in place is what makes a
                    // configuration change take effect without a restart.
                    self.ml_routing.withdraw();
                }
                tracing::info!(commit_id = commit, "the active model pointer was reloaded");
                crate::ml::ReloadOutcome::ok(self.ml_routing.is_attached(), Some(commit))
            }
            Ok(None) => {
                self.ml_routing.withdraw();
                tracing::info!("no model is promoted; routing is deterministic");
                crate::ml::ReloadOutcome::ok(false, None)
            }
            Err(error) => {
                crate::ml::ReloadOutcome::fault(self.ml_routing.is_attached(), error.to_string())
            }
        };
        outcome
    }

    /// Withdraw the current model and return to the previously promoted one.
    ///
    /// The operator's kill switch, and the reason the promotion gate is
    /// trustworthy: a model that turns out to be wrong can be removed without
    /// stopping the proxy, without editing a file by hand, and without waiting
    /// for a restart.
    ///
    /// Two things happen and both are required. The durable pointer moves, so
    /// the change survives a restart; and the router is reloaded from that
    /// pointer, so the process stops using the withdrawn model *now*. Doing only
    /// the first is the cosmetic rollback this module was written to prevent.
    ///
    /// Reports `false` when there is no previous model, which is a genuine "there
    /// was nothing to roll back to" rather than a failure — a fresh installation
    /// has a history of length zero and saying so is more useful than an error.
    #[cfg(feature = "ml")]
    pub fn rollback_active_model(&self) -> crate::ml::ReloadOutcome {
        let store = match self.active_models.as_ref() {
            Some(store) => store,
            None => {
                return crate::ml::ReloadOutcome::fault(
                    self.ml_routing.is_attached(),
                    "no durable state directory is configured, so nothing can be rolled back",
                )
            }
        };
        match store.rollback() {
            Ok(true) => {
                tracing::info!("the active model was rolled back by an operator");
                self.reload_active_model()
            }
            Ok(false) => crate::ml::ReloadOutcome::fault(
                self.ml_routing.is_attached(),
                "there is no earlier promoted model to roll back to",
            ),
            Err(error) => crate::ml::ReloadOutcome::fault(
                self.ml_routing.is_attached(),
                format!("the rollback was refused: {error}"),
            ),
        }
    }

    /// A test of the loaded model against real request history.
    ///
    /// This is the shadow analysis from the outside: the same `analyse` the
    /// offline gate uses, fed by the operator's own traffic rather than a
    /// fixture, and run against the weights actually attached to the router.
    ///
    /// Bounded on purpose. `limit` caps how many records are read, because this
    /// is callable from a desktop button and an unbounded read would take the
    /// app down rather than answer a question.
    ///
    /// Run one promotion round over the durable history, and optionally install
    /// what the gate authorised.
    ///
    /// This is the product entry point the loop was missing. It is in-process rather
    /// than a separate command because promotion has to reach the *live* router:
    /// `reload_active_model` attaches on this `AppState`, and nothing watches the
    /// pointer file, so a model installed by another process would sit unread until
    /// a restart. That is a fact about the design rather than a preference, which is
    /// why the entry point is here and not in a CLI.
    ///
    /// # `install` is not implied
    ///
    /// Judging and installing are separate acts, and the default is to judge only.
    /// A promotion changes what every subsequent request is served by, so it should
    /// be something an operator asked for, not something that happened because
    /// someone polled an endpoint. A caller that wants the model live asks for it.
    ///
    /// That makes the third of the three open product decisions — *when* a round
    /// happens — a property of the caller rather than of this code. Nothing here
    /// schedules anything, retries anything, or holds a cadence; an operator's own
    /// scheduler decides, and it decides by choosing when to call.
    ///
    /// The gate is still what judges. `install` only obeys a `PROMOTED` verdict; it
    /// cannot install a refusal.
    #[cfg(feature = "ml")]
    pub fn ml_run_promotion_round(
        &self,
        config: crate::ml::RoundConfig,
        install: bool,
        revision: Option<String>,
    ) -> crate::ml::status::PromotionRoundStatus {
        if self.active_models.is_none() {
            return crate::ml::status::PromotionRoundStatus::unavailable(
                "no durable state directory is configured, so there is no history to learn from",
            );
        }
        let round = match crate::ml::run_promotion_round(&self.ml_state_dir(), &config, revision) {
            Ok(round) => round,
            Err(error) => {
                return crate::ml::status::PromotionRoundStatus::unavailable(error.to_string())
            }
        };

        let current_state = || {
            // Nothing was installed, so the pointer is unchanged and there is nothing
            // to re-read. Reporting the current state keeps the response honest about
            // whether a model is serving.
            crate::ml::ReloadOutcome::ok(
                self.ml_routing.is_attached(),
                self.ml_routing.attached_commit(),
            )
        };

        let installed = if install {
            let store = self.active_models.as_ref().expect("checked above");
            match round.install(store) {
                Ok(installed) => installed,
                Err(error) => {
                    // The round judged fine and the store refused. Reporting this as
                    // an unavailable round would throw away the verdict, which is the
                    // part an operator asked for, so the decision travels back with
                    // the fault attached. No reload: the install failed, so the
                    // pointer is whatever it was before this call.
                    let mut status = crate::ml::status::PromotionRoundStatus::from_round(
                        &round,
                        current_state(),
                    );
                    status.error = Some(format!(
                        "the gate authorised this model but it could not be installed: {error}"
                    ));
                    return status;
                }
            }
        } else {
            None
        };

        let reloaded = if installed.is_some() {
            self.reload_active_model()
        } else {
            current_state()
        };
        crate::ml::status::PromotionRoundStatus::from_round(&round, reloaded)
    }

    /// The durable state directory, derived the same way `new` derived it when it
    /// opened the log and the store. Taken from the configuration rather than from
    /// the open `TraceLog` so that no accessor is added to that type for this one
    /// caller.
    #[cfg(feature = "ml")]
    pub(super) fn ml_state_dir(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.config().ml_routing.state_dir)
    }

    /// Reports *why* it could not run instead of returning an empty analysis,
    /// because "no model is attached" and "the model disagreed with production on
    /// nothing" are different answers and only the first is a problem.
    #[cfg(feature = "ml")]
    pub fn ml_shadow_analysis(&self, limit: usize) -> crate::ml::ShadowAnalysisStatus {
        const MAX_RECORDS: usize = 50_000;
        let limit = limit.min(MAX_RECORDS);
        let traces =
            match self.traces.as_ref() {
                None => return crate::ml::ShadowAnalysisStatus::unavailable(
                    0,
                    "no durable state directory is configured, so there is no history to replay",
                ),
                Some(log) => match log.tail(limit) {
                    Ok(traces) => traces,
                    Err(error) => {
                        return crate::ml::ShadowAnalysisStatus::unavailable(0, error.to_string())
                    }
                },
            };
        let read = traces.len();
        if read == 0 {
            return crate::ml::ShadowAnalysisStatus::unavailable(
                0,
                "no request history has been recorded yet",
            );
        }
        // Replay the attached model, not whatever was promoted most recently.
        let (ensemble, commit_id) = match (
            self.ml_routing.attached_ensemble(),
            self.ml_routing.attached_commit(),
        ) {
            (Some(ensemble), Some(commit_id)) => (ensemble, commit_id),
            _ => {
                return crate::ml::ShadowAnalysisStatus::unavailable(
                    read,
                    "no model is attached to the router, so there is nothing to replay; \
                     promote a model and enable ml_routing",
                )
            }
        };
        let reward_policy = self.config().ml_routing_reward_policy();
        let policy = crate::ml::MlPolicy::from_ensemble(ensemble, reward_policy.clone());
        let evidence = crate::ml::ShadowEvidence::from_policy(&traces, &policy, BTreeMap::new());
        let analysis = crate::ml::analyse(&traces, &evidence, &reward_policy);
        crate::ml::ShadowAnalysisStatus {
            traces_read: read,
            commit_id: Some(commit_id),
            reason: None,
            analysis: Some(analysis),
        }
    }

    /// Read the operator-facing view of the learned model.
    ///
    /// Everything here is read from the live components; nothing is recomputed.
    /// A store that cannot be read reports itself as unreadable rather than as
    /// empty, because "there is no model" and "I could not tell whether there is
    /// a model" are different answers and an operator acting on the second one
    /// as if it were the first is how a broken deployment keeps routing.
    #[cfg(feature = "ml")]
    pub fn ml_status(&self) -> crate::ml::MlStatus {
        let store = self.active_models.as_ref();
        let (active, active_decision, history) = match store {
            None => (None, None, Vec::new()),
            Some(store) => {
                let pointer = store.read_pointer().ok().flatten();
                let current = pointer.map(|pointer| pointer.current);
                let active = current
                    .as_ref()
                    .map(|model| crate::ml::PromotedModelStatus {
                        model_id: model.model_id.clone(),
                        commit_id: model.commit_id.clone(),
                        verdict: model.promotion.verdict,
                        dataset_fingerprint: model.promotion.dataset_fingerprint.to_string(),
                        fitted_partition_fingerprint: model
                            .promotion
                            .fitted_partition_fingerprint
                            .to_string(),
                        gate_config_identity: model.promotion.gate_config_identity.clone(),
                        required_baseline: model.promotion.baseline.clone(),
                        paired_requests: model.promotion.paired_requests,
                        holdout_loss: model.promotion.holdout_loss,
                        promoted_at: model.promoted_at,
                    });
                let decision = current.map(|model| model.promotion.clone());
                let history = store
                    .audit()
                    .map(|entries| {
                        entries
                            .iter()
                            .map(crate::ml::PromotionHistoryEntry::from)
                            .collect()
                    })
                    .unwrap_or_default();
                (active, decision, history)
            }
        };

        // What the configuration names, narrowed to the candidates that could
        // actually be routed to. This is the classifier's own view — an enabled
        // provider with an enabled model entry — and it is what makes the blind
        // spot list a statement about the configuration the operator just edited
        // rather than a second, drifting copy of what is callable.
        let configured: Vec<(String, String)> = {
            let config = self.config();
            config
                .models
                .iter()
                .filter(|entry| entry.enabled)
                .filter(|entry| {
                    config
                        .providers
                        .iter()
                        .any(|provider| provider.enabled && provider.id == entry.provider_id)
                })
                .map(|entry| (entry.exposed_id(), entry.provider_id.clone()))
                .collect()
        };
        let observations = self.router().observations();
        let blind_candidates = crate::ml::BlindCandidate::unobserved(
            configured
                .iter()
                .map(|(model_id, provider_id)| (model_id.as_str(), provider_id.as_str())),
            observations,
        );

        crate::ml::MlStatus {
            routing_enabled: self.config().ml_routing.enabled,
            durable_state: self.config().ml_routing.has_state_dir(),
            traces_open: self.traces.is_some(),
            model_store_open: store.is_some(),
            active,
            active_decision,
            history,
            routing: self.ml_routing.counts(),
            exploration_probability: self.config().ml_routing.exploration_probability,
            exploration_seed: self.config().ml_routing.exploration_seed,
            blind_candidates,
            dataset: self.dataset.counters(),
            traces: self.traces.as_ref().map(crate::ml::TraceLog::counters),
            shadow: crate::ml::ShadowStatus {
                enabled: self.shadow().enabled(),
                decisions_recorded: self.shadow().store().len(),
                faults: self.shadow().fault_count(),
            },
            read_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Evaluate one request's shadow counterfactual, against whatever is attached.
    ///
    /// The predictor does not leave this struct. That is a deliberate boundary,
    /// not an encapsulation convenience: an accepted gate asserts that
    /// `AppState` never names a learning seam, and a method that handed out the
    /// ensemble predictor would put one in reach of this module and of every
    /// caller downstream of it. Exposing only the *evaluation* means the request
    /// path cannot obtain the predictor at all, so the only thing it can do with
    /// a candidate is record what it would have decided — which is the whole of
    /// what a shadow is.
    ///
    /// `None` when the candidate is withdrawn, in which case the engine's own
    /// cold-start predictor runs and the record names the engine's commit. That
    /// is the behaviour this state had before a candidate existed, and it is an
    /// ordinary outcome rather than an error.
    #[cfg(feature = "ml")]
    pub fn shadow_evaluated(
        &self,
        request_id: &str,
        input: &crate::ml::ShadowInput,
    ) -> Option<crate::ml::ShadowDecision> {
        let shadow = &self.shadow;
        let predictor = crate::sync::read(&self.shadow_attachment).predictor();
        match predictor {
            Some(predictor) => shadow.evaluate_with(request_id, input, predictor.as_ref()),
            None => shadow.evaluate(request_id, input),
        }
    }

    /// The commit id of the attached candidate, or `None` when withdrawn.
    #[cfg(feature = "ml")]
    pub fn shadow_candidate_commit_id(&self) -> Option<String> {
        crate::sync::read(&self.shadow_attachment)
            .attached_commit_id()
            .map(str::to_string)
    }

    /// Whether a named candidate is currently attached.
    ///
    /// Answerable in every build. Without the ML feature it is always `false`,
    /// and that is the honest answer rather than a missing method: the desktop
    /// application compiles no shadow and therefore has no candidate.
    pub fn shadow_candidate_attached(&self) -> bool {
        crate::sync::read(&self.shadow_attachment).is_attached()
    }

    /// Withdraw the shadow candidate.
    ///
    /// This is the rollback lever and it is deliberately one-way: there is no
    /// counterpart that can attach a candidate, because the candidate is a
    /// program constant verified at construction and re-attaching it at runtime
    /// would be a swap rather than a rollback. After this returns, every
    /// subsequent evaluation names the engine's own cold-start predictor, which
    /// is the behaviour this node started from.
    ///
    /// Returns whether anything was withdrawn, so a caller can tell a real
    /// rollback from a no-op rather than assuming one.
    #[cfg(feature = "ml")]
    pub fn withdraw_shadow_candidate(&self) -> bool {
        let mut attachment = crate::sync::write(&self.shadow_attachment);
        if !attachment.is_attached() {
            return false;
        }
        *attachment = ShadowAttachment::withdrawn();
        tracing::warn!(
            "the shadow model candidate was withdrawn; the shadow is back on its own predictor"
        );
        true
    }

    /// The retained outcomes paired with their routing decisions, so the
    /// observability projection can be read back.
    ///
    /// Not a routing input and not a training source: a diagnostic view of the
    /// same single accounting the request log records.
    pub fn projections(&self) -> &ProjectionLog {
        &self.projections
    }
}
