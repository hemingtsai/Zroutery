//! One response shape over the two upstream transports.
//!
//! Almost every provider answers over the shared `reqwest` pool. The OpenCode
//! free tier additionally needs the ClientHello the gateway fingerprints, which
//! only a BoringSSL client can present, so those requests are sent by `wreq`
//! (Windows and macOS only). Both stacks have to feed the same callers: status,
//! headers, a byte stream and a text body. This module is that boundary, not a
//! general HTTP abstraction — it exists so the streaming, JSON and error paths
//! above it never learn which client carried the request.

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

/// The body of one upstream response, already reduced to transport failures.
pub(super) type BodyStream =
    Pin<Box<dyn Stream<Item = std::result::Result<Bytes, BodyError>> + Send>>;

/// A failure below the HTTP message, kept as the transport that produced it.
#[derive(Debug)]
pub(super) enum BodyError {
    Reqwest(reqwest::Error),
    #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
    Fingerprint(wreq::Error),
}

impl BodyError {
    /// The crate error for this failure, named by provider but never by URL.
    ///
    /// A transport source can embed the full request URL, which for some
    /// providers carries account identifiers; the client-facing message stays
    /// at provider name plus failure kind, and the source survives only in the
    /// log through the error chain.
    pub(super) fn into_upstream(self, provider: &str) -> crate::Error {
        match self {
            BodyError::Reqwest(source) => crate::Error::Transport {
                provider: provider.to_string(),
                source,
            },
            #[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
            BodyError::Fingerprint(source) => crate::Error::FingerprintTransport {
                provider: provider.to_string(),
                // wreq's Display names the failure kind only; the URL is not
                // part of it, unlike reqwest's chain for the same failure.
                message: source.to_string(),
            },
        }
    }
}

/// One upstream response, whichever client produced it.
pub(super) struct UpstreamResponse {
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    body: BodyStream,
}

impl UpstreamResponse {
    pub(super) fn status(&self) -> reqwest::StatusCode {
        self.status
    }

    pub(super) fn headers(&self) -> &reqwest::header::HeaderMap {
        &self.headers
    }

    /// The body, consuming the response. Callers that only need a small JSON
    /// error body use [`Self::text`]; a streamed answer consumes this.
    pub(super) fn bytes_stream(self) -> BodyStream {
        self.body
    }

    /// The whole body as text, lossily decoded.
    ///
    /// `reqwest::Response::text` decodes the charset from the content type;
    /// every body this proxy reads as text is JSON or an SSE error frame, both
    /// UTF-8 by contract, so lossy UTF-8 is the same result for valid input and
    /// a reportable replacement character for malformed input.
    pub(super) async fn text(self) -> std::result::Result<String, BodyError> {
        let mut body = self.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl From<reqwest::Response> for UpstreamResponse {
    fn from(response: reqwest::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let body: BodyStream = Box::pin(
            response
                .bytes_stream()
                .map(|chunk| chunk.map_err(BodyError::Reqwest)),
        );
        Self {
            status,
            headers,
            body,
        }
    }
}

#[cfg(all(feature = "opencode", any(target_os = "windows", target_os = "macos")))]
impl From<wreq::Response> for UpstreamResponse {
    fn from(response: wreq::Response) -> Self {
        let status = response.status();
        let headers = response.headers().clone();
        let body: BodyStream = Box::pin(
            response
                .bytes_stream()
                .map(|chunk| chunk.map_err(BodyError::Fingerprint)),
        );
        Self {
            status,
            headers,
            body,
        }
    }
}
