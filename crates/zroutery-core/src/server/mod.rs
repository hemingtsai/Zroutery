//! The local HTTP server.
//!
//! Exposes one endpoint per dialect (`POST /v1/messages` and
//! `POST /v1/chat/completions`) plus a merged `GET /v1/models` listing that
//! satisfies both Anthropic and OpenAI clients.
//!
//! Security: it binds loopback by default and requires a local token. Anything
//! that can reach this port can spend the configured API keys.
//!
//! # Layout
//!
//! | module | what it owns |
//! |---|---|
//! | [`state`] | [`AppState`] and the admission permits that keep budget scopes exclusive |
//! | [`handlers`] | the route table and one function per route, plus the auth layer |
//! | [`cors`] | the CORS policy, and the origin validation behind it |
//! | [`ledger`] | construction, the spend ledger, and budget admission |
//! | [`accessors`] | the read-only handles other subsystems hold on the process |
//! | [`ml_state`] | model attachment, promotion rounds, shadow verdicts, ML status |
//! | [`config_state`] | swapping configuration and the registry it feeds |
//!
//! `ledger`, `accessors`, `ml_state` and `config_state` are the method groups of
//! one type. Rust merges split inherent `impl` blocks, so this is legal rather
//! than a workaround, but it does make the table above the only map from a
//! method name to a file.

mod pipeline;
mod projection_log;
mod shadow_candidate;

// The account reconciler's production caller. Gated on its own feature and on
// nothing else: `account` and `ml` are independent, so this cannot perturb ML
// feature order.
#[cfg(feature = "account")]
mod accounts;

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
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| Error::internal(format!("cannot bind {addr}: {e}")))?;
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
