//! Driving a real, headed Chromium browser over the DevTools protocol.
//!
//! # What this is, and what it is not
//!
//! This module launches the user's own Chrome or Edge as a visible window on an
//! isolated profile and speaks the DevTools protocol to it over a WebSocket. It
//! is the **execution** half of browser check-in: it produces a page and a
//! verdict about that page. Deciding what the verdict means is
//! [`super::waf`]'s job, and deciding what the account did with it is the
//! adapter's.
//!
//! # Why a real window and not a headless one
//!
//! The operation needs a human, so the browser has to be able to be one. A
//! headless browser has no window to solve a challenge in, no profile the user
//! can sign in to by hand once, and — for several vendors — is detected and
//! refused outright. There is no headless mode here because there is no version
//! of this operation that works in one.
//!
//! # Why the DevTools port is left to the browser
//!
//! `--remote-debugging-port=0` makes the browser pick a free port and write it
//! into `DevToolsActivePort` in its profile directory. Picking the port
//! ourselves would mean binding a socket, releasing it, and hoping the browser
//! got it before anything else did. Letting the browser bind it and then reading
//! back where it landed removes the race entirely.
//!
//! # What this module deliberately does not hold
//!
//! No cookie, no credential and no page DOM crosses this boundary as data. The
//! browser holds the session, in its own profile directory, which is what makes
//! §17's "credentials live in the keychain or the browser profile, never in
//! application state" true by construction rather than by discipline: there is
//! nowhere here for a cookie to be stored.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use futures_util::{SinkExt as _, StreamExt as _};

use super::profile;

/// How long to wait for a launched browser to write its debugging port.
///
/// Generous, because this is a cold start of a full desktop browser on a
/// possibly-slow disk, and giving up early would mean killing a browser the user
/// is about to see.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a protocol call to answer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// A launched browser and the connection to it.
///
/// Holds the child process and the WebSocket. Dropping it does **not** close the
/// browser: [`Self::shutdown`] does, explicitly. That asymmetry is deliberate —
/// a check-in paused on a challenge must survive being dropped from a
/// collection, and only an explicit close ends a session.
pub struct BrowserSession {
    /// The process this handle refers to.
    ///
    /// On Windows this is the launcher, which exits as soon as it has handed off,
    /// so it is kept only as a kill fallback and is **not** used to decide whether
    /// the browser is running. [`Self::is_running`] asks the port instead.
    child: tokio::process::Child,
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    /// The browser's own debugging port, for the liveness check.
    port: u16,
    next_id: u64,
    profile_dir: PathBuf,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hand-written rather than derived: a derived debug would print the
        // socket and the child, and the child's arguments include a profile
        // directory derived from the provider and account ids.
        f.debug_struct("BrowserSession")
            .field("profile_dir", &self.profile_dir)
            .finish_non_exhaustive()
    }
}

/// Why a browser could not be started or driven.
#[derive(Debug)]
pub enum BrowserError {
    /// No Chromium browser could be found.
    NotFound,
    /// The browser did not report a debugging port in time.
    LaunchTimeout,
    /// The browser could not be started.
    Spawn(std::io::Error),
    /// The debugging endpoint did not accept a connection.
    Connect(String),
    /// The DevTools protocol reported an error.
    Protocol { method: String, message: String },
    /// The browser exited while the operation was running.
    Exited,
    /// The command was cancelled before it answered.
    Cancelled,
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(
                f,
                "no Chromium browser was found; set MaintenanceConfig::browser_executable"
            ),
            Self::LaunchTimeout => {
                write!(f, "the browser did not report a debugging port in time")
            }
            Self::Spawn(e) => write!(f, "the browser could not be started: {e}"),
            Self::Connect(detail) => write!(f, "the browser debugging endpoint refused: {detail}"),
            Self::Protocol { method, message } => {
                write!(f, "the browser refused {method}: {message}")
            }
            Self::Exited => write!(f, "the browser exited during the operation"),
            Self::Cancelled => write!(f, "the operation was cancelled"),
        }
    }
}

impl std::error::Error for BrowserError {}

impl From<std::io::Error> for BrowserError {
    fn from(e: std::io::Error) -> Self {
        Self::Spawn(e)
    }
}

/// Find a Chromium browser to drive.
///
/// `configured` wins if it is set and usable; otherwise the known locations are
/// tried, and bare names are resolved through `PATH`. Returns the first that
/// exists rather than the first that is merely plausible, so a machine with both
/// a per-user and a machine-wide Chrome uses one that is actually installed.
pub fn find_browser(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        // An explicit path is used as given: if the operator named a file that is
        // not there, saying so beats silently driving a different browser.
        return Some(path);
    }
    profile::browser_candidates()
        .iter()
        .find_map(|candidate| resolve(&candidate.to_string_lossy()))
}

fn resolve(candidate: &str) -> Option<PathBuf> {
    if profile::is_absolute_candidate(candidate) || profile::is_windows_style_absolute(candidate) {
        let path = PathBuf::from(candidate);
        return path.is_file().then_some(path);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(candidate))
        .find(|path| path.is_file())
}

/// The arguments a browser is launched with.
///
/// [`build_args`] is separated from the launching so the argument list is
/// testable on a machine with no browser at all, which is the situation CI is in.
///
/// The two that matter:
///
/// * `--user-data-dir` is the isolation boundary. Without it the browser would
///   use the user's everyday profile and check in as whoever they are signed in
///   as, in a profile Zroutery would then be reading and writing.
/// * `--remote-debugging-port=0` makes the browser bind and report its own port,
///   which removes the race a fixed port would have.
///
/// The rest suppress the first-run and default-browser flows that would otherwise
/// put a modal in front of the user during an unattended check-in.
pub fn build_args(profile_dir: &Path, start_url: &str) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", profile_dir.display()),
        "--remote-debugging-port=0".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        // The panel's own page is the operation; nothing else should be in the way.
        "--disable-session-crashed-bubble".to_string(),
        "--disable-infobars".to_string(),
        start_url.to_string(),
    ]
}

/// Launch a headed browser on an isolated profile and attach to it.
pub async fn launch(
    executable: &Path,
    profile_dir: &Path,
    start_url: &str,
) -> Result<BrowserSession, BrowserError> {
    tokio::fs::create_dir_all(profile_dir).await?;

    let child = tokio::process::Command::new(executable)
        .args(build_args(profile_dir, start_url))
        // A browser's own standard streams are not ours to read, and inheriting
        // them would let a diagnostic banner end up in Zroutery's log through the
        // tracing writer the app installs. They are discarded instead.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false)
        .spawn()?;

    // A `DevToolsActivePort` left behind by a browser that was killed rather than
    // closed would otherwise be read as this browser's, and the connect would fail
    // against a port nobody is listening on. Deleting it first makes "the file
    // exists" mean "this launch wrote it".
    let _ = tokio::fs::remove_file(profile_dir.join(profile::DEVTOOLS_PORT_FILE)).await;

    let endpoint = wait_for_endpoint(profile_dir).await?;
    let port = endpoint
        .split("127.0.0.1:")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(0);
    let socket = connect(&endpoint).await?;

    Ok(BrowserSession {
        child,
        socket,
        port,
        next_id: 0,
        profile_dir: profile_dir.to_path_buf(),
    })
}

/// Wait for the browser to publish its debugging endpoint.
///
/// # The spawned process is not a liveness signal
///
/// On Windows, `chrome.exe` **exits as soon as it has handed off** — measured on
/// this repository's own verification: the launched process had already exited
/// while 20 `chrome` processes were serving traffic and the DevTools HTTP
/// endpoint answered. Treating that exit as a failed launch makes the first thing
/// a correct browser does look like a crash.
///
/// So the port file is the only readiness signal, and the process handle is not
/// consulted for liveness at all. It is kept for one thing: a kill fallback on
/// platforms where the handle still refers to the browser.
async fn wait_for_endpoint(profile_dir: &Path) -> Result<String, BrowserError> {
    // The file lives inside the profile directory this browser was launched with,
    // so it is read where it is rather than re-derived from ids the caller no
    // longer has in hand.
    let port_file = profile_dir.join(profile::DEVTOOLS_PORT_FILE);

    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Some(endpoint) = read_endpoint(&port_file).await {
            return Ok(endpoint);
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::LaunchTimeout);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether something is still listening on the browser's debugging port.
///
/// Used to confirm a close actually took effect, for the reason above: the child
/// handle is not a reliable answer on Windows, but the port either accepts a
/// connection or it does not.
async fn port_is_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|result| result.is_ok())
    .unwrap_or(false)
}

/// The debugging endpoint from a `DevToolsActivePort` file, if it holds one yet.
///
/// Returns `None` while the file is absent, empty or mid-write — all normal
/// during a browser's first moments. The second line carries the per-launch
/// WebSocket path and is required: a fixed `/devtools/browser/unknown` is
/// refused by the browser, so the endpoint cannot be assembled from the port
/// alone.
async fn read_endpoint(path: &Path) -> Option<String> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    endpoint_from_port_file(&text)
}

/// The port from a `DevToolsActivePort` file, if it holds one yet.
///
/// Returns `None` while the file is absent, empty or mid-write. A partially
/// written file is normal during browser start-up and is not an error.
///
/// Retained alongside [`read_endpoint`] because the two validate different
/// things: this one refuses a port that cannot be connected to, and its absence
/// would otherwise be indistinguishable from a missing file.
#[cfg(test)]
async fn read_port(path: &Path) -> Option<u16> {
    let text = tokio::fs::read_to_string(path).await.ok()?;
    let port = text.lines().next()?.trim();
    // Validated rather than trusted: the file is inside a directory the browser
    // owns and the path to it is derived from user-supplied ids.
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port.parse::<u16>().ok().filter(|p| *p > 0)
}

/// The WebSocket path Chrome actually serves.
///
/// The browser's own path appears on the second line of `DevToolsActivePort` and
/// carries a per-launch id, so a fixed `/devtools/browser/unknown` is refused and
/// this has to be read rather than assumed.
///
/// The path is required to start with `/` and to carry nothing else. That is not
/// pedantry: this string is appended to a host Zroutery chose, and a malformed
/// file could otherwise turn into `ws://127.0.0.1:9222` followed by a second
/// scheme, which is a connection to somewhere the browser is not.
fn endpoint_from_port_file(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port = lines.next()?.trim();
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let path = lines.next().unwrap_or_default().trim();
    if !path.starts_with('/') || path.contains("://") || path.contains(' ') {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

async fn connect(endpoint: &str) -> Result<BrowserSocket, BrowserError> {
    let (socket, _) = tokio_tungstenite::connect_async(endpoint)
        .await
        .map_err(|e| BrowserError::Connect(e.to_string()))?;
    Ok(socket)
}

/// The browser socket type, named so [`connect`] has a signature that reads.
type BrowserSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A page inside the browser, and the protocol session bound to it.
///
/// # Why the session id matters
///
/// The socket this module connects to is the **browser** endpoint, and a browser
/// connection does not serve the page domains: `Runtime.evaluate` sent on one is
/// answered with *"Runtime.evaluate wasn't found"*, which is exactly what the
/// first real run of this code produced. Page-level commands have to carry the
/// `sessionId` that `Target.attachToTarget` hands back, and holding it here is
/// what keeps that requirement from being met by accident.
///
/// Keeping it on the same socket rather than opening a second one is deliberate:
/// a page lives as long as its session, and one socket is one thing whose
/// lifetime has to be reasoned about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBinding {
    /// The DevTools target id.
    pub target_id: String,
    /// The session id page-level commands must be sent with.
    pub session_id: String,
}

impl BrowserSession {
    /// Send one DevTools command and wait for its answer.
    ///
    /// Events arriving while a command is outstanding are discarded rather than
    /// buffered: nothing in this flow reads `Page.loadEventFired`, and a
    /// mis-routed event must not be mistaken for a command reply.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, BrowserError> {
        self.call_scoped(None, method, params).await
    }

    /// Send one page-level command on `page`'s session.
    pub async fn call_page(
        &mut self,
        page: &PageBinding,
        method: &str,
        params: Value,
    ) -> Result<Value, BrowserError> {
        self.call_scoped(Some(&page.session_id), method, params)
            .await
    }

    async fn call_scoped(
        &mut self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, BrowserError> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session_id) = session_id {
            message["sessionId"] = Value::String(session_id.to_string());
        }
        self.socket
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|e| BrowserError::Connect(e.to_string()))?;

        let deadline = tokio::time::Instant::now() + COMMAND_TIMEOUT;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(BrowserError::Protocol {
                    method: method.to_string(),
                    message: "no answer within the timeout".into(),
                });
            }
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .map_err(|_| BrowserError::Protocol {
                    method: method.to_string(),
                    message: "no answer within the timeout".into(),
                })?
                // `None` means the stream ended without a frame, which is the same
                // fact as the browser having exited.
                .ok_or(BrowserError::Exited)?
                // A read error means the socket is gone, which is the same fact as
                // the browser having exited as far as this call is concerned.
                .map_err(|e| BrowserError::Connect(e.to_string()))?;

            let Message::Text(text) = frame else {
                // Ping, pong, binary and close frames carry no command reply. A
                // close means the browser is gone.
                if matches!(frame, Message::Close(_)) {
                    return Err(BrowserError::Exited);
                }
                continue;
            };
            let value: Value = serde_json::from_str(&text).map_err(|e| BrowserError::Protocol {
                method: method.to_string(),
                message: format!("unreadable frame: {e}"),
            })?;

            // An answer carries our id; anything else is an event.
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(BrowserError::Protocol {
                    method: method.to_string(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unspecified")
                        .to_string(),
                });
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Open a page and attach to it, returning the binding page commands need.
    ///
    /// Two calls rather than one: creating a target and attaching to it are
    /// separate operations, and only the second yields the `sessionId` that
    /// `Runtime.*` and `Page.*` commands have to carry.
    pub async fn open_page(&mut self, url: &str) -> Result<PageBinding, BrowserError> {
        let target_id = self
            .call("Target.createTarget", json!({ "url": url }))
            .await?
            .get("targetId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| BrowserError::Protocol {
                method: "Target.createTarget".into(),
                message: "no targetId in the answer".into(),
            })?;

        let session_id = self
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
            )
            .await?
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| BrowserError::Protocol {
                method: "Target.attachToTarget".into(),
                message: "no sessionId in the answer".into(),
            })?;

        let page = PageBinding {
            target_id,
            session_id,
        };
        // `Runtime.evaluate` needs the domain enabled before it will hand back a
        // value; without this the first evaluation on a fresh target can fail with
        // "Cannot find context with specified id".
        self.call_page(&page, "Runtime.enable", json!({})).await?;
        Ok(page)
    }

    /// Navigate an already-attached page.
    ///
    /// Used when the session is resumed: the same page is sent somewhere else, so
    /// the session and its cookies survive and only the document changes.
    pub async fn navigate(&mut self, page: &PageBinding, url: &str) -> Result<(), BrowserError> {
        self.call_page(page, "Page.enable", json!({})).await?;
        self.call_page(page, "Page.navigate", json!({ "url": url }))
            .await
            .map(|_| ())
    }

    /// Read what is currently on screen, for [`super::waf`] to classify.
    ///
    /// Evaluated in the page rather than fetched over HTTP, because the point is
    /// to see what the *browser* can see — including whether a cross-origin
    /// widget is present, which an HTTP fetch of the same URL would not show.
    pub async fn page_evidence(
        &mut self,
        page: &PageBinding,
    ) -> Result<super::waf::PageEvidence, BrowserError> {
        const PROBE: &str = r#"JSON.stringify({
            url: location.href,
            title: document.title,
            body: (document.body && document.body.innerText || '').slice(0, 4096),
            hosts: Array.from(document.querySelectorAll('iframe')).map(function (f) {
                try { return new URL(f.src, location.href).host } catch (e) { return '' }
            }).filter(Boolean),
            application: !!document.querySelector('#root, [data-reactroot], main, #app')
        })"#;
        let result = self
            .call_page(
                page,
                "Runtime.evaluate",
                json!({ "expression": PROBE, "returnByValue": true }),
            )
            .await?;

        let raw = result
            .pointer("/result/value")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let value: Value = serde_json::from_str(raw).unwrap_or(Value::Null);

        Ok(super::waf::PageEvidence {
            url: value
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            title: value
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            body_text: value
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            frame_hosts: value
                .get("hosts")
                .and_then(Value::as_array)
                .map(|hosts| {
                    hosts
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            application_dom_loaded: value
                .get("application")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            elapsed_secs: 0.0,
        })
    }

    /// Close the browser and wait for it to actually go.
    ///
    /// Explicit rather than done on drop, because a paused challenge is a state
    /// the operation must survive being dropped from.
    ///
    /// `Browser.close` is what does the work, and the wait is on the **port**
    /// rather than on the process — on Windows the process this session spawned
    /// has already exited while the browser it started is still serving. A browser
    /// killed hard leaves a profile lock behind, so the close request is preferred
    /// and the kill is a last resort.
    pub async fn shutdown(mut self) -> Result<(), BrowserError> {
        let _ = self.call("Browser.close", json!({})).await;
        for _ in 0..50 {
            if !port_is_open(self.port).await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = self.child.kill().await;
        Ok(())
    }

    /// Whether the browser is still serving its debugging port.
    ///
    /// Asks the port rather than the process, because on Windows the two disagree
    /// and only the port reflects the browser that is actually running.
    pub async fn is_running(&self) -> bool {
        port_is_open(self.port).await
    }

    /// The isolated profile this session uses.
    pub fn profile_dir(&self) -> &Path {
        &self.profile_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_directory_is_always_passed() {
        // Without this the browser uses the user's everyday profile and checks in
        // as whoever they are signed in as.
        let args = build_args(Path::new("/p/a"), "https://relay.example/console");
        let user_data = args
            .iter()
            .find(|a| a.starts_with("--user-data-dir="))
            .expect("the isolation argument must be present");
        assert_eq!(user_data, "--user-data-dir=/p/a");
    }

    #[test]
    fn the_debugging_port_is_left_to_the_browser() {
        // A fixed port is a race: Zroutery would bind it, release it, and hope the
        // browser got it. `0` makes the browser bind and report where it landed.
        let args = build_args(Path::new("/p/a"), "https://relay.example/console");
        assert!(args.iter().any(|a| a == "--remote-debugging-port=0"));
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("--remote-debugging-port=9")),
            "no fixed port may be chosen"
        );
    }

    #[test]
    fn first_run_prompts_are_suppressed() {
        let args = build_args(Path::new("/p/a"), "https://relay.example/console");
        assert!(args.iter().any(|a| a == "--no-first-run"));
        assert!(args.iter().any(|a| a == "--no-default-browser-check"));
    }

    #[test]
    fn the_start_url_is_the_last_argument() {
        // Chrome treats a trailing URL as the page to open; anything after it is
        // read as a switch.
        let args = build_args(Path::new("/p/a"), "https://relay.example/console/personal");
        assert_eq!(
            args.last().unwrap(),
            "https://relay.example/console/personal"
        );
    }

    #[test]
    fn no_argument_names_a_credential() {
        // A command line is world-readable on most platforms, so a password here
        // would be a published password.
        let args = build_args(Path::new("/p/a"), "https://relay.example/console");
        let joined = args.join(" ").to_lowercase();
        for forbidden in ["password", "token", "secret", "cookie", "authorization"] {
            assert!(
                !joined.contains(forbidden),
                "the browser command line must carry no credential material: {joined}"
            );
        }
    }

    #[tokio::test]
    async fn a_partially_written_port_file_yields_no_endpoint() {
        // The function the launcher actually calls. Every state a file can be in
        // during a browser's first moments must read as "not ready" rather than as
        // an endpoint to connect to and fail against.
        let dir = std::env::temp_dir().join("zroutery-portfile-endpoint-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(profile::DEVTOOLS_PORT_FILE);

        // Absent.
        assert_eq!(read_endpoint(&path).await, None);
        // Present but empty.
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_endpoint(&path).await, None);
        // Port written, path not yet: a port alone is not an endpoint.
        std::fs::write(&path, "9222\n").unwrap();
        assert_eq!(read_endpoint(&path).await, None);
        // Both written.
        std::fs::write(&path, "9222\n/devtools/browser/abc\n").unwrap();
        assert_eq!(
            read_endpoint(&path).await.as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/abc")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_port_is_validated_rather_than_trusted() {
        // The file lives inside a directory whose path derives from user-supplied
        // ids, so its contents are checked before they become an address.
        let dir = std::env::temp_dir().join("zroutery-portfile-validate");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(profile::DEVTOOLS_PORT_FILE);

        std::fs::write(&path, "not-a-port\n/devtools/browser/x\n").unwrap();
        assert_eq!(read_port(&path).await, None);
        assert_eq!(read_endpoint(&path).await, None);

        std::fs::write(&path, "99999999\n/devtools/browser/x\n").unwrap();
        assert_eq!(read_port(&path).await, None, "out of range for a port");

        std::fs::write(&path, "0\n/devtools/browser/x\n").unwrap();
        assert_eq!(read_port(&path).await, None, "port 0 is not connectable");

        // A file that would point somewhere other than this loopback browser is refused.
        std::fs::write(&path, "9222\nws://example.com/devtools/browser/x\n").unwrap();
        assert_eq!(read_endpoint(&path).await, None);

        // A path that is not a path.
        std::fs::write(&path, "9222\ndevtools\n").unwrap();
        assert_eq!(read_endpoint(&path).await, None);
        std::fs::write(&path, "9222\n/devtools/browser/a b\n").unwrap();
        assert_eq!(read_endpoint(&path).await, None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_websocket_path_comes_from_the_browser_not_from_us() {
        // The path carries a per-launch id and a fixed `/devtools/browser/unknown`
        // is rejected, so this has to be read rather than assumed.
        let endpoint = endpoint_from_port_file("9222\n/devtools/browser/7f3a\n").unwrap();
        assert_eq!(endpoint, "ws://127.0.0.1:9222/devtools/browser/7f3a");
        assert!(endpoint_from_port_file("9222\n").is_none());
        assert!(endpoint_from_port_file("abc\n/devtools/browser/x").is_none());
    }

    #[test]
    fn an_explicitly_configured_executable_is_used_as_given() {
        // If the operator named a browser that is not there, saying so beats
        // silently driving a different one.
        let configured = find_browser("  /nonexistent/browser  ").expect("returns the path");
        assert_eq!(configured, PathBuf::from("/nonexistent/browser"));
    }

    #[test]
    fn the_error_messages_say_what_to_do() {
        assert!(
            BrowserError::NotFound
                .to_string()
                .contains("browser_executable"),
            "the user needs to be told which setting to change"
        );
        assert!(BrowserError::Exited.to_string().contains("exited"));
    }
}
