//! The local HTTP server.
//!
//! Exposes one endpoint per dialect (`POST /v1/messages` and
//! `POST /v1/chat/completions`) plus a merged `GET /v1/models` listing that
//! satisfies both Anthropic and OpenAI clients.
//!
//! Security: it binds loopback by default and requires a local token. Anything
//! that can reach this port can spend the configured API keys.

mod pipeline;
mod projection_log;
mod shadow_candidate;

// The account reconciler's production caller. Gated on its own feature and on
// nothing else: `account` and `ml` are independent, so this cannot perturb ML
// feature order.
#[cfg(feature = "account")]
mod accounts;

// ServerHandle is the only thing left in this file, so these are the only
// imports it needs; each child carries its own.
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::error::{Error, Result};

pub use shadow_candidate::ShadowAttachment;

#[cfg(feature = "ml")]
pub use shadow_candidate::{
    ShadowCandidate, ShadowCandidateArtifact, ShadowCandidateError, SHADOW_CANDIDATE_ARTIFACT,
    SHADOW_CANDIDATE_ENVELOPE_VERSION,
};

/// A running server plus its shutdown handle.
pub struct ServerHandle {
    pub addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    /// Bind and serve. Returns as soon as the socket is listening.
    pub async fn start(state: Arc<AppState>) -> Result<ServerHandle> {
        let cfg = state.config();
        let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
        let listener = TcpListener::bind(&addr).await.map_err(|e| {
            // The raw OS string for a taken port is a localised paragraph the
            // operator cannot act on ("通常每个套接字地址... os error 10048"). Name
            // the condition instead, and keep the code for diagnosis; the
            // desktop layer adds the "another instance may be running" hint on
            // top of this wording.
            let reason = match e.kind() {
                std::io::ErrorKind::AddrInUse => match e.raw_os_error() {
                    Some(code) => format!("the address is already in use (os error {code})"),
                    None => "the address is already in use".to_string(),
                },
                _ => e.to_string(),
            };
            Error::internal(format!("cannot bind {addr}: {reason}"))
        })?;
        let addr = listener
            .local_addr()
            .map_err(|e| Error::internal(format!("cannot read local addr: {e}")))?;

        let app = build_app(state);
        let (tx, rx) = oneshot::channel();
        let join = tokio::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            if let Err(e) = served {
                tracing::error!("server stopped: {e}");
            }
        });

        Ok(ServerHandle {
            addr,
            shutdown: Some(tx),
            join,
        })
    }

    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}
mod accessors;
mod config_state;
mod cors;
mod handlers;
mod ledger;
#[cfg(feature = "ml")]
mod ml_state;
mod state;

#[cfg(test)]
mod tests;

// The pipeline sits two levels down and reaches back for these.
pub(crate) use cors::is_valid_origin;
pub use handlers::build_app;
pub(crate) use handlers::error_response;
pub(crate) use state::AdmissionGuard;
pub use state::AppState;
