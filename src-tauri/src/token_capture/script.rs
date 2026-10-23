//! The page side of token capture: where the panel keeps a credential, and how
//! to ask for one durable token without ever sending the credential anywhere.
//!
//! # Why the page has to do it
//!
//! Current NewAPI keeps the dashboard's access token in memory and persists
//! only an HttpOnly refresh cookie; older forks keep the token in
//! `localStorage`. Neither is reachable from Rust, and the personal-token
//! endpoint sits behind the panel's own security proof, which is a browser
//! session. So the browser finds a bearer, asks the panel to mint the durable
//! personal token, and returns **only that token**; the credential the browser
//! holds never leaves it.
//!
//! # Idempotence
//!
//! The probe runs every two seconds. Everything with a side effect — the
//! one-shot refresh and the mint — is recorded on `window.__zrCapture`, and the
//! interceptors that learn a bearer from the dashboard's own requests are
//! installed once per document. Refreshing on every tick would rotate the
//! refresh cookie and trip the endpoint's rate limit; minting on every tick
//! would invalidate the token it just handed back.
//!
//! # What it returns
//!
//! A JSON string, so one shape crosses `Runtime.evaluate` regardless of how the
//! page resolved it. [`parse`] validates the token before Rust ever stores it.

use serde_json::Value;

/// Where a panel that has no configured sign-in path is expected to sign in.
///
/// Current NewAPI serves `/sign-in`; older forks use `/login`. The path is a
/// declaration and can be edited per account, so the default only has to be
/// right for the current generation.
pub const DEFAULT_LOGIN_PATH: &str = "/sign-in";

/// The longest token a panel is allowed to hand back.
pub const MAX_TOKEN_BYTES: usize = 4096;

/// The panel root: the base url with any `/api` or `/v1` suffix removed.
///
/// Repeated because bases arrive pasted from several places: `…/api/v1` is a
/// combination operators do type, and stripping only one suffix would leave
/// `…/api` as the root.
pub fn panel_root(base_url: &str) -> String {
    let mut root = base_url.trim().trim_end_matches('/').to_string();
    loop {
        let stripped = root
            .strip_suffix("/api")
            .or_else(|| root.strip_suffix("/v1"))
            .map(|value| value.trim_end_matches('/').to_string());
        match stripped {
            Some(next) if !next.is_empty() && next != root => root = next,
            _ => break,
        }
    }
    root
}

/// The page a token capture opens.
pub fn login_url(base_url: &str, login_path: Option<&str>) -> Result<String, String> {
    let root = panel_root(base_url);
    if !(root.starts_with("http://") || root.starts_with("https://")) {
        return Err(format!(
            "`{base_url}` is not an http(s) panel url, so a browser cannot open it"
        ));
    }
    let path = login_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(DEFAULT_LOGIN_PATH);
    if path.starts_with("http://") || path.starts_with("https://") {
        return Err("the panel sign-in path must be relative to the panel, not a full url".into());
    }
    Ok(format!("{root}/{}", path.trim_start_matches('/')))
}

/// The scheme-and-authority part of a url, for the page's origin check.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if scheme.is_empty() || authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
}

/// What the page reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureResult {
    /// Signed out, or signed in but no credential is reachable yet.
    Waiting,
    /// A durable token was minted (or a stored one was accepted).
    Ready {
        token: String,
        username: Option<String>,
        source: String,
    },
    /// The panel wants the operator to complete a verification step.
    NeedsVerification { message: Option<String> },
    /// The panel has no token endpoint this capture can use.
    Unsupported { message: String },
    /// The probe itself failed.
    Error { message: String },
}

/// Read the probe's answer out of the value `Runtime.evaluate` returned.
pub fn parse(value: &Value) -> CaptureResult {
    let Some(text) = value.as_str() else {
        return CaptureResult::Error {
            message: "the capture probe returned no text".into(),
        };
    };
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        return CaptureResult::Error {
            message: format!(
                "the capture probe returned unreadable JSON: {}",
                truncate(text)
            ),
        };
    };
    match json.get("state").and_then(Value::as_str) {
        Some("waiting") => CaptureResult::Waiting,
        Some("ready") => {
            let Some(token) = json.get("token").and_then(Value::as_str) else {
                return CaptureResult::Error {
                    message: "the panel reported a token without one".into(),
                };
            };
            match storable_token(token) {
                Ok(token) => CaptureResult::Ready {
                    token,
                    username: json
                        .get("username")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    source: json
                        .get("source")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                },
                Err(reason) => CaptureResult::Error {
                    message: format!("the panel handed back an unusable token: {reason}"),
                },
            }
        }
        Some("needs_verification") => CaptureResult::NeedsVerification {
            message: json
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        Some("unsupported") => CaptureResult::Unsupported {
            message: json
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the panel offers no durable access token")
                .to_string(),
        },
        Some(other) => CaptureResult::Error {
            message: format!("the capture probe reported an unknown state `{other}`"),
        },
        None => CaptureResult::Error {
            message: format!("the capture probe reported no state: {}", truncate(text)),
        },
    }
}

/// Whether a value may be stored as the account credential.
///
/// A relay key (`sk-…`) is the most important refusal: it authenticates the
/// `/v1` relay and no dashboard endpoint, and storing it would produce a 401
/// that reads as "the token expired" when the truth is "this is the wrong kind
/// of credential".
pub fn storable_token(raw: &str) -> Result<String, String> {
    let token = raw.trim();
    if token.is_empty() {
        return Err("the token is empty".into());
    }
    if token.len() > MAX_TOKEN_BYTES {
        return Err(format!("the token exceeds {MAX_TOKEN_BYTES} bytes"));
    }
    if token.chars().any(char::is_whitespace) {
        return Err("the token contains whitespace".into());
    }
    if token.starts_with("sk-") {
        return Err(
            "this is a relay API key; the dashboard needs the panel's personal access token".into(),
        );
    }
    Ok(token.to_string())
}

fn truncate(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}…")
}

/// The expression the capture polls.
///
/// The expected origin is embedded as a JSON string literal, so a host name
/// cannot close the expression and append its own — the same discipline
/// [`super::super::cdp::BrowserSession::click`] applies to a selector.
pub fn capture_expression(expected_origin: &str) -> String {
    let origin = serde_json::to_string(expected_origin).unwrap_or_else(|_| "\"\"".into());
    PROBE.replace("__EXPECTED_ORIGIN__", &origin)
}

/// The probe. Kept as one raw string so the code it sends is the code that was
/// read and reviewed, with a single placeholder for the origin.
const PROBE: &str = r#"(async () => {
  const EXPECTED_ORIGIN = __EXPECTED_ORIGIN__;
  const out = (value) => JSON.stringify(value);
  if (location.origin !== EXPECTED_ORIGIN) {
    return out({ state: 'unsupported', message: 'the browser left the panel (its origin is ' + location.origin + ')' });
  }
  const C = (window.__zrCapture = window.__zrCapture || {
    installed: false, triedRefresh: false, minted: null, bearer: null
  });
  const withTimeout = (promise, ms) => Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), ms)),
  ]);
  const verify = async (token) => {
    try {
      const response = await withTimeout(fetch('/api/user/self', {
        headers: { Authorization: 'Bearer ' + token },
        credentials: 'same-origin',
      }), 5000);
      if (!response.ok) return null;
      const body = await response.json();
      return body && body.success ? body : null;
    } catch (error) {
      return null;
    }
  };
  const mint = async (token) => {
    if (C.minted) return { token: C.minted };
    try {
      const response = await withTimeout(fetch('/api/user/token', {
        headers: { Authorization: 'Bearer ' + token },
        credentials: 'same-origin',
      }), 8000);
      if (response.status === 403) return { needsVerification: true };
      if (!response.ok) return null;
      const body = await response.json();
      const minted = typeof body.data === 'string'
        ? body.data
        : (body.data && (body.data.token || body.data.access_token));
      if (body.success && typeof minted === 'string' && minted.length > 0) {
        C.minted = minted;
        return { token: minted };
      }
      return null;
    } catch (error) {
      return null;
    }
  };
  const note = (value) => {
    if (typeof value !== 'string') return;
    const match = value.match(/^Bearer\s+(.+)$/i);
    if (match) C.bearer = match[1].trim();
  };
  if (!C.installed) {
    C.installed = true;
    const original = window.fetch;
    if (typeof original === 'function') {
      window.fetch = function (input, init) {
        try {
          const headers = (init && init.headers) || (input && input.headers);
          if (headers) {
            if (typeof headers.get === 'function') note(headers.get('authorization'));
            else if (Array.isArray(headers)) headers.forEach((pair) => {
              if (String(pair[0]).toLowerCase() === 'authorization') note(pair[1]);
            });
            else Object.keys(headers).forEach((key) => {
              if (key.toLowerCase() === 'authorization') note(headers[key]);
            });
          }
        } catch (error) {}
        return original.apply(this, arguments);
      };
    }
    const xhr = window.XMLHttpRequest && window.XMLHttpRequest.prototype;
    if (xhr && xhr.setRequestHeader) {
      const setHeader = xhr.setRequestHeader;
      xhr.setRequestHeader = function (name, value) {
        try {
          if (String(name).toLowerCase() === 'authorization') note(value);
        } catch (error) {}
        return setHeader.apply(this, arguments);
      };
    }
  }
  if (C.minted && (await verify(C.minted))) {
    return out({ state: 'ready', token: C.minted, username: null, source: 'minted' });
  }
  const candidates = [];
  if (C.bearer) candidates.push(C.bearer);
  const scan = (storage) => {
    try {
      for (let i = 0; i < storage.length; i += 1) {
        const key = storage.key(i) || '';
        const raw = storage.getItem(key);
        if (!raw) continue;
        if (/token/i.test(key)) candidates.push(raw.trim());
        try {
          const parsed = JSON.parse(raw);
          if (parsed && typeof parsed === 'object') {
            ['token', 'access_token', 'accessToken'].forEach((name) => {
              if (typeof parsed[name] === 'string') candidates.push(parsed[name].trim());
            });
          }
        } catch (error) {}
      }
    } catch (error) {}
  };
  scan(window.localStorage);
  scan(window.sessionStorage);
  const seen = new Set();
  for (const candidate of candidates) {
    if (!candidate || seen.has(candidate)) continue;
    seen.add(candidate);
    if (!(await verify(candidate))) continue;
    const minted = await mint(candidate);
    if (minted && minted.needsVerification) {
      return out({ state: 'needs_verification', message: 'the panel asked for a verification step' });
    }
    if (minted && minted.token) {
      return out({ state: 'ready', token: minted.token, username: null, source: 'minted' });
    }
    // A login JWT is short-lived and has no refresh cookie the app can keep,
    // so it is not a durable account credential.
    if (/^ey[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\./.test(candidate)) {
      return out({ state: 'unsupported', message: 'the panel did not return a durable access token' });
    }
    return out({ state: 'ready', token: candidate, username: null, source: 'stored' });
  }
  if (!C.triedRefresh) {
    C.triedRefresh = true;
    try {
      const response = await withTimeout(fetch('/api/user/auth/refresh', {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json' },
      }), 8000);
      if (response.ok) {
        const body = await response.json();
        const token = body && body.data && body.data.access_token;
        if (typeof token === 'string' && (await verify(token))) {
          const minted = await mint(token);
          if (minted && minted.needsVerification) {
            return out({ state: 'needs_verification', message: 'the panel asked for a verification step' });
          }
          if (minted && minted.token) {
            return out({ state: 'ready', token: minted.token, username: null, source: 'minted' });
          }
          return out({ state: 'unsupported', message: 'the panel did not return a durable access token' });
        }
      }
    } catch (error) {}
  }
  return out({ state: 'waiting' });
})()"#;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_panel_root_strips_repeated_api_and_version_suffixes() {
        assert_eq!(
            panel_root("https://relay.example/"),
            "https://relay.example"
        );
        assert_eq!(
            panel_root("https://relay.example/v1"),
            "https://relay.example"
        );
        assert_eq!(
            panel_root("https://relay.example/api"),
            "https://relay.example"
        );
        assert_eq!(
            panel_root("https://relay.example/api/v1/"),
            "https://relay.example"
        );
        // A path that merely ends in those letters is not a suffix.
        assert_eq!(
            panel_root("https://relay.example/av1"),
            "https://relay.example/av1"
        );
    }

    #[test]
    fn the_login_url_defaults_to_the_current_panels_page() {
        assert_eq!(
            login_url("https://relay.example/v1", None).unwrap(),
            "https://relay.example/sign-in"
        );
        assert_eq!(
            login_url("https://relay.example", Some("/login")).unwrap(),
            "https://relay.example/login"
        );
        assert_eq!(
            login_url("https://relay.example", Some("login")).unwrap(),
            "https://relay.example/login"
        );
        // A full url would leave the panel; the field is panel-relative.
        assert!(login_url("https://relay.example", Some("https://evil.example/login")).is_err());
        assert!(login_url("file:///etc/passwd", None).is_err());
    }

    #[test]
    fn an_origin_is_the_scheme_and_authority() {
        assert_eq!(
            origin_of("https://relay.example:8443/sign-in?x=1").as_deref(),
            Some("https://relay.example:8443")
        );
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn a_minted_token_is_read_from_the_probe() {
        let answer =
            json!(r#"{"state":"ready","token":"abc123","username":"alice","source":"minted"}"#);
        assert_eq!(
            parse(&answer),
            CaptureResult::Ready {
                token: "abc123".into(),
                username: Some("alice".into()),
                source: "minted".into(),
            }
        );
    }

    #[test]
    fn the_probe_states_map_to_typed_results() {
        assert_eq!(
            parse(&json!(r#"{"state":"waiting"}"#)),
            CaptureResult::Waiting
        );
        assert!(matches!(
            parse(&json!(
                r#"{"state":"needs_verification","message":"turnstile"}"#
            )),
            CaptureResult::NeedsVerification { .. }
        ));
        assert!(matches!(
            parse(&json!(r#"{"state":"unsupported","message":"no endpoint"}"#)),
            CaptureResult::Unsupported { .. }
        ));
        assert!(matches!(
            parse(&json!(r#"{"state":"signed-in"}"#)),
            CaptureResult::Error { .. }
        ));
        assert!(matches!(parse(&json!(42)), CaptureResult::Error { .. }));
        assert!(matches!(
            parse(&json!("not json")),
            CaptureResult::Error { .. }
        ));
    }

    #[test]
    fn a_relay_key_is_never_stored_as_a_panel_token() {
        for token in [
            "",
            "   ",
            "sk-abc123",
            "has space",
            "Bearer abc",
            &"x".repeat(MAX_TOKEN_BYTES + 1),
        ] {
            assert!(
                storable_token(token).is_err(),
                "{token:?} must not be storable"
            );
        }
        assert_eq!(
            storable_token("  eyJhbGciOi.abc.def  ").unwrap(),
            "eyJhbGciOi.abc.def"
        );
    }

    #[test]
    fn the_expression_embeds_the_origin_as_a_literal() {
        let expression = capture_expression("https://relay.example");
        assert!(expression.contains(r#"const EXPECTED_ORIGIN = "https://relay.example";"#));
        // A quote in the value cannot close the literal: it is escaped by the
        // JSON encoder, so the page sees one string and the expression keeps
        // its shape.
        let expression = capture_expression("https://relay.example/\");alert(1);//");
        assert!(
            expression.contains("\\\");alert(1);//"),
            "the quote has to be escaped in the expression: {expression}"
        );
        assert!(expression.contains("const EXPECTED_ORIGIN = \"https://relay.example/\\\""));
    }
}
