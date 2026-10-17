//! The SSE body: how a stream is read, held, and turned into a terminal state.

use std::collections::VecDeque;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::watch;

use super::super::{AdmissionGuard, AppState};
use super::{RequestLifecycle, Settlement, TerminalKind};
use crate::billing::Pricing;
use crate::config::ModelTier;
use crate::error::Error;
use crate::ir::{StoredResponse, StreamEvent, Usage};
use crate::protocol::{self, SseFrame, StreamEncoder};
use crate::query::RequestKind;

/// Why a stream stopped producing events.
///
/// A stream has four genuinely different endings, and only the stream knows
/// which one happened. Collapsing them — as a success-defaulted drop path does
/// — is what turns an abandoned answer into a served one.
pub(super) enum StreamTerminal {
    /// The upstream closed the stream after its end.
    Completed,
    /// The upstream reported an error part way through.
    UpstreamError(Error),
    /// The client asked for this request to be cancelled.
    Cancelled,
    /// The client went away before the stream finished.
    ClientDisconnected,
}

pub(super) struct SseState {
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    pending: VecDeque<SseFrame>,
    lifecycle: RequestLifecycle,
    state: Arc<AppState>,
    usage: Usage,
    pricing: Option<Pricing>,
    /// Who to bill, once the stream reports what it used.
    provider_id: String,
    tier: Option<ModelTier>,
    /// Set for classifier streams, which accumulate their text so the verdict
    /// can be checked once the stream ends. Main streams never pay for this.
    classifier_text: Option<String>,
    finished: bool,
    /// Whether any answer byte has been produced for this client yet. A client
    /// that disappears after that truncated the answer it was reading.
    emitted: bool,
    /// Pre-generated response ID for in-flight tracking (Responses API).
    response_id: Option<String>,
    /// Cancel receiver for cancellation detection (Responses API).
    cancel_rx: Option<watch::Receiver<bool>>,
    /// What the client asked the proxy to retain for this response.
    storage: protocol::responses::StoragePolicy,
    /// The Responses input items, kept so a completed stream can be retrieved
    /// and continued exactly like a buffered answer.
    input_items: Vec<Value>,
    /// The response this stream continues, when the client sent one.
    previous_response_id: Option<String>,
    /// The admission permits for this request's budgeted scopes.
    ///
    /// Nothing reads it: it is held for its drop. `Drop for SseState` runs the
    /// terminal transition — which charges the ledger — before any field is
    /// released, so the next request for the scope can never be admitted against
    /// a total this stream has not written yet, and a client that walks away
    /// releases the scope through the same path.
    _admission: AdmissionGuard,
}

impl SseState {
    /// How long this stream has been running, for the record's own fields.
    fn elapsed_ms(&self) -> u64 {
        self.lifecycle.elapsed_ms()
    }

    /// Keep the finished stream, so its id answers a later `GET`.
    ///
    /// Only a confirmed normal terminal is stored, and only when the client
    /// asked for retention: a truncated, failed, cancelled or `store: false`
    /// stream has nothing to replay, and a truncated one must never be handed
    /// back as a completed answer. The in-flight entry is gone by the time this
    /// runs, so a cancellation arriving now finds the stored response instead
    /// of overwriting it with a placeholder.
    fn store_completed(&self) {
        if !self.storage.retains() {
            return;
        }
        let Some(ref id) = self.response_id else {
            return;
        };
        let Some(output) = self.encoder.response_output() else {
            return;
        };
        self.state.response_store.put(StoredResponse::completed(
            id.clone(),
            self.lifecycle.served_model(),
            self.input_items.clone(),
            output,
            self.usage,
            self.previous_response_id.clone(),
            self.lifecycle.decision(),
        ));
    }

    /// The stream's one terminal transition.
    ///
    /// Idempotent, so [`Drop`] can call it for a client that walked away
    /// without disturbing a stream that already ended normally.
    fn finalize(&mut self, terminal: StreamTerminal) {
        if self.lifecycle.is_terminal() {
            return;
        }
        // Clean up in-flight tracking for Responses API streams.
        if let Some(ref id) = self.response_id {
            self.state.response_store.complete_in_flight(id);
        }
        if let StreamTerminal::Completed = terminal {
            self.store_completed();
        }
        // A classifier stream that reached its end without producing a verdict
        // is worth a warning. The bytes have already gone out, so there is
        // nothing to fail over to — but the client will fail closed on its own
        // parse, and this line is how that shows up in the proxy's log.
        if let StreamTerminal::Completed = terminal {
            if let Some(text) = self.classifier_text.take() {
                if crate::classifier::parse_verdict(&text)
                    == crate::classifier::ClassifierVerdict::Unparseable
                {
                    tracing::warn!(
                        model = %self.lifecycle.served_model(),
                        "streamed classifier response carried no <block> verdict"
                    );
                }
            }
        }
        let kind = match terminal {
            StreamTerminal::Completed => TerminalKind::Served,
            StreamTerminal::UpstreamError(error) => TerminalKind::failed(&error),
            StreamTerminal::Cancelled => TerminalKind::ClientCancelled,
            StreamTerminal::ClientDisconnected => TerminalKind::ClientDisconnected {
                emitted: self.emitted,
            },
        };
        let settlement = Settlement {
            usage: self.usage,
            pricing: self.pricing.clone(),
            provider_id: self.provider_id.clone(),
            tier: self.tier,
        };
        self.lifecycle.finalize(kind, settlement);
    }
}

impl Drop for SseState {
    fn drop(&mut self) {
        // A client disconnect drops the body stream without the unfold loop ever
        // reaching the end of it. That is a terminal state in its own right:
        // the answer was abandoned, and an abandoned answer is never a served
        // one. Activity, the ledger and the outcome still record what happened,
        // exactly once, and the provider's health is left alone.
        self.finalize(StreamTerminal::ClientDisconnected);
    }
}

/// What the SSE pipeline needs to know about the request it is serving.
pub(super) struct StreamContext {
    // Constructed field-by-field by the attempt loop, then owned by the SSE body.
    // Every field is `pub(super)` because the builder lives in the parent module
    // and there is no constructor worth adding just to hide them.
    pub(super) lifecycle: RequestLifecycle,
    pub(super) usage: Usage,
    pub(super) pricing: Option<Pricing>,
    pub(super) provider_id: String,
    pub(super) tier: Option<ModelTier>,
    pub(super) kind: RequestKind,
    /// Pre-generated response ID for in-flight tracking (Responses API).
    pub(super) response_id: Option<String>,
    /// Cancel receiver for cancellation detection (Responses API).
    pub(super) cancel_rx: Option<watch::Receiver<bool>>,
    /// What the client asked the proxy to retain for this response.
    pub(super) storage: protocol::responses::StoragePolicy,
    /// The Responses input items, replayed by a later `GET`.
    pub(super) input_items: Vec<Value>,
    /// The response this stream continues, when the client sent one.
    pub(super) previous_response_id: Option<String>,
    /// Handed to the stream state, which holds it until the stream is over.
    pub(super) admission: AdmissionGuard,
}

/// Pipe canonical events through the egress encoder into an SSE byte stream.
pub(super) fn sse_body(
    app: Arc<AppState>,
    events: crate::upstream::EventStream,
    encoder: Box<dyn StreamEncoder>,
    context: StreamContext,
) -> impl futures_util::Stream<Item = std::result::Result<bytes::Bytes, std::io::Error>> {
    use futures_util::StreamExt;

    let classifier_text = (!context.kind.is_main()).then(String::new);
    let state = SseState {
        events,
        encoder,
        pending: VecDeque::new(),
        lifecycle: context.lifecycle,
        state: app,
        usage: context.usage,
        pricing: context.pricing,
        provider_id: context.provider_id,
        tier: context.tier,
        classifier_text,
        finished: false,
        emitted: false,
        response_id: context.response_id,
        cancel_rx: context.cancel_rx,
        storage: context.storage,
        input_items: context.input_items,
        previous_response_id: context.previous_response_id,
        _admission: context.admission,
    };

    futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(frame) = st.pending.pop_front() {
                return Some((Ok(bytes::Bytes::from(frame.to_wire())), st));
            }
            if st.finished {
                return None;
            }
            // Race upstream events against cancellation signal for immediate abort.
            let next_event = if let Some(ref mut rx) = st.cancel_rx {
                tokio::select! {
                    event = st.events.next() => event,
                    result = rx.changed() => {
                        if result.is_ok() && *rx.borrow() {
                            st.finished = true;
                            // The cancelled placeholder carries no content, but
                            // it is still a retained response: a client that
                            // asked for `store: false` gets none.
                            if st.storage.retains() {
                                if let Some(ref id) = st.response_id {
                                    let model = st.lifecycle.served_model();
                                    st.state.response_store.mark_cancelled(id, model);
                                }
                            }
                            // An explicit cancellation is a terminal state of its
                            // own, never a success.
                            st.finalize(StreamTerminal::Cancelled);
                            return None;
                        }
                        // Spurious wakeup or channel closed — continue
                        st.events.next().await
                    }
                }
            } else {
                st.events.next().await
            };
            match next_event {
                Some(Ok(event)) => {
                    // The response's public id is the one the proxy registered
                    // as in-flight before the first byte went out. It is not a
                    // substitute for an empty upstream id: an upstream id the
                    // client could only repeat back to a cancel endpoint that
                    // never heard of it is not an identity, so the upstream id
                    // is never published.
                    let event = match (&st.response_id, &event) {
                        (Some(pregen_id), StreamEvent::Start { model, usage, .. }) => {
                            StreamEvent::Start {
                                id: pregen_id.clone(),
                                model: model.clone(),
                                usage: *usage,
                            }
                        }
                        _ => event,
                    };
                    match &event {
                        StreamEvent::ThinkingDelta { .. } => {
                            let elapsed = st.elapsed_ms();
                            st.lifecycle.ttft_ms(elapsed);
                        }
                        StreamEvent::TextDelta { text, .. } => {
                            let elapsed = st.elapsed_ms();
                            st.lifecycle.ttft_ms(elapsed);
                            // Classifier streams keep their text so the verdict
                            // can be checked once the stream ends.
                            if let Some(acc) = st.classifier_text.as_mut() {
                                acc.push_str(text);
                            }
                        }
                        StreamEvent::Start { usage, .. } => st.usage = *usage,
                        StreamEvent::Stop { usage, .. } => st.usage = *usage,
                        _ => {}
                    }
                    let frames = st.encoder.encode(&event);
                    // From here the client holds part of the answer, so a
                    // disconnect that happens later truncates it.
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                }
                Some(Err(err)) => {
                    st.finished = true;
                    // The upstream may have ended a truncated body after
                    // reporting usage: keep that accounting even though the
                    // request itself is failing, so the partial answer's real
                    // spend is not silently dropped.
                    if let Error::InterruptedStream { usage } = &err {
                        st.usage = *usage;
                    }
                    let frames = st.encoder.error(&err);
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                    st.finalize(StreamTerminal::UpstreamError(err));
                }
                None => {
                    st.finished = true;
                    let frames = st.encoder.finish();
                    st.emitted |= !frames.is_empty();
                    st.pending.extend(frames);
                    st.finalize(StreamTerminal::Completed);
                }
            }
        }
    })
}
