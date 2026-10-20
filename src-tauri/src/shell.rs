//! The desktop-native shell.
//!
//! Four things a desktop application is expected to do that a web page
//! cannot:
//!
//! * be the only copy of itself — a second launch must bring the first
//!   one's window forward rather than start a second gateway on the same
//!   port;
//! * be summoned from another application, which is the whole point of an
//!   application that keeps its window hidden;
//! * answer a `zroutery://` URL;
//! * and say something out loud when it failed, if nobody is looking at the
//!   window.
//!
//! Everything here is desktop-only: the plugins behind it are desktop
//! plugins, and an accessory that cannot be summoned from another
//! application is not the thing this module describes.

use tauri::{AppHandle, Emitter, Manager, Wry};
use tauri_plugin_notification::NotificationExt;

use crate::tray;

/// The URL scheme this application answers to.
///
/// Declared in `tauri.conf.json` under `plugins.deep-link.desktop.schemes`.
/// That declaration is what the macOS bundle reads to write
/// `CFBundleURLTypes`, and what the Windows and Linux installers register
/// the handler with; without it the plugin knows no scheme and refuses
/// every URL.
pub const DEEP_LINK_SCHEME: &str = "zroutery";

/// The event the dashboard listens for when a link arrives.
///
/// The payload is the URL as a string. The name mirrors the
/// `zroutery://gateway-state-changed` convention the tray already uses, so
/// the two are recognisable as a pair.
pub const DEEP_LINK_EVENT: &str = "zroutery://deep-link";

/// The last link this process received, if any.
///
/// A link that *started* the application is announced before the dashboard has
/// finished loading, so there is no listener for it yet and the event goes to
/// nobody. The link is kept here instead, and the dashboard asks for it when
/// it mounts — so "this link started the application" and "the application was
/// already running and the link arrived" both land on the same page.
static LAST_DEEP_LINK: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The last link this process received, for the dashboard to ask about.
#[tauri::command]
pub fn deep_link_current() -> Option<String> {
    LAST_DEEP_LINK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

/// Remember a link, so a dashboard that was not listening can still find it.
fn remember_deep_link(url: &str) {
    let mut slot = LAST_DEEP_LINK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    *slot = Some(url.to_string());
}

/// The chord that summons the window from another application.
///
/// `CommandOrControl` is Ctrl on Windows and Linux and Command on macOS,
/// so this is Ctrl+Shift+Alt+Z there and Command+Shift+Alt+Z here.
///
/// Four keys, and the fourth one is Alt. That is the whole argument: a
/// three-key chord in this neighbourhood is taken — Ctrl+Shift+Z is redo in
/// VS Code, IntelliJ and most editors, Ctrl+Alt+Z and Ctrl+Alt+Shift+Z
/// are taken by terminal emulators and by GNOME and KDE shortcuts — and
/// adding Alt is what moves the binding out of the way of the keys people
/// press without thinking. The letter is Z because it is the one nobody
/// binds with Alt held down.
///
/// A chord the operating system refuses — another application already owns
/// it, or the desktop will not hand out global shortcuts at all — is a
/// warning, never a reason to refuse to start.
pub const SUMMON_SHORTCUT: &str = "CommandOrControl+Shift+Alt+Z";

/// Bring the dashboard forward.
///
/// "Forward" means shown, un-minimised *and* focused: a window that
/// becomes visible behind whatever the operator is doing is not a summons.
/// The work is posted to the main thread because the callers are not all on
/// it — the global shortcut's handler runs on the hotkey thread — and a
/// window shown from the wrong thread does not come to the front on every
/// platform.
pub fn summon(app: &AppHandle) {
    let handle = app.clone();
    if app
        .run_on_main_thread(move || tray::show_dashboard(&handle))
        .is_err()
    {
        // The main thread refused the task; showing the window here is
        // better than not showing it at all.
        tray::show_dashboard(app);
    }
}

/// What a second launch of the application means.
///
/// The second process never reaches the run loop: the single-instance plugin
/// hands its arguments over and exits, so there is no second gateway
/// fighting the first one for the port.
///
/// The arguments still matter. On Windows and Linux the desktop opens a
/// `zroutery://` link by *starting the application with the URL as its only
/// argument*, so a second launch is how a link arrives at an application
/// that is already running. Handing those arguments to the deep-link plugin
/// gives the link the same path as a link opened into a fresh process.
pub fn on_second_launch(app: &AppHandle, argv: &[String]) {
    if let Some(deep_link) = app.try_state::<tauri_plugin_deep_link::DeepLink<Wry>>() {
        deep_link.handle_cli_arguments(argv.iter());
    }
    summon(app);
}

/// Install the parts of the shell that are not plugins: the shortcut and
/// the URL scheme.
///
/// Called from the application's setup, after the window exists and after
/// the plugins are registered.
pub fn install(app: &AppHandle) {
    install_shortcut(app);
    install_deep_links(app);
}

/// Register the chord that summons the window.
///
/// The plugin is registered here rather than with the rest of them because
/// a desktop that will not give out global shortcuts at all must not stop
/// the application from starting: the failure is reported and the rest of
/// the shell carries on without the chord.
fn install_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    if let Err(e) = app.plugin(tauri_plugin_global_shortcut::Builder::new().build()) {
        tracing::warn!("cannot start the global shortcut service: {e}");
        return;
    }

    let registered = app
        .global_shortcut()
        .on_shortcut(SUMMON_SHORTCUT, |app, _shortcut, event| {
            // The plugin reports both edges of the key. Only the press
            // opens the window, or it would open and close again.
            if event.state == ShortcutState::Pressed {
                summon(app);
            }
        });
    match registered {
        Ok(()) => tracing::info!("the window can be summoned with {SUMMON_SHORTCUT}"),
        Err(e) => tracing::warn!("cannot claim {SUMMON_SHORTCUT}: {e}"),
    }
}

/// Listen for `zroutery://` links and bring the window forward when one
/// arrives.
fn install_deep_links(app: &AppHandle) {
    use tauri_plugin_deep_link::DeepLinkExt;

    tracing::info!("this application answers the {DEEP_LINK_SCHEME}:// scheme");

    // Where the handler is registered is the operating system's business:
    // macOS gets it from the bundle's Info.plist, and Windows and Linux
    // from the installer. Whether it took is worth saying out loud, because
    // a link that opens nothing is otherwise impossible to explain.
    match app.deep_link().is_registered(DEEP_LINK_SCHEME) {
        Ok(true) => tracing::info!("the {DEEP_LINK_SCHEME}:// scheme is registered with this system"),
        Ok(false) => tracing::warn!(
            "the {DEEP_LINK_SCHEME}:// scheme is not registered with this system; a link will open nothing"
        ),
        Err(e) => tracing::debug!("cannot tell whether the {DEEP_LINK_SCHEME}:// scheme is registered: {e}"),
    }

    let handle = app.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            announce_deep_link(&handle, url.as_ref());
        }
    });

    // A link that *started* this process was announced before the
    // application's own setup ran, so nobody could have been listening for
    // it. It is read back rather than waited for.
    match app.deep_link().get_current() {
        Ok(Some(urls)) => {
            for url in urls {
                announce_deep_link(app, url.as_ref());
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("cannot read the link that opened the application: {e}"),
    }
}

/// Announce one link to the dashboard and bring the window forward.
///
/// The plugin only emits the schemes its configuration declares, and that
/// configuration declares one — this application's own — so every URL that
/// arrives here is already ours to announce.
fn announce_deep_link(app: &AppHandle, url: &str) {
    tracing::info!("deep link opened: {url}");
    remember_deep_link(url);
    let _ = app.emit(DEEP_LINK_EVENT, url);
    summon(app);
}

/// Tell the operator that the gateway did not start.
///
/// Nothing on screen says this by itself: the tray menu has nowhere to
/// write an error down, and the window may well be closed. The
/// notification is the report.
///
/// A failure that is *already* a banner in the dashboard must not come
/// through here — the same message twice is worse than one — which is what
/// [`notify_startup_failure`] is for.
pub fn notify_gateway_failure(app: &AppHandle, cause: &str) {
    let (title, body) = if tray::system_language_is_chinese() {
        ("Zroutery — 网关未能启动", format!("网关没有启动: {cause}"))
    } else {
        (
            "Zroutery — the gateway did not start",
            format!("The proxy could not start: {cause}"),
        )
    };
    tracing::error!("{title}: {body}");
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        tracing::warn!("cannot show the notification: {e}");
    }
}

/// Tell the operator that the gateway did not start at launch, unless the
/// dashboard is already saying so.
///
/// The startup failure is a banner in the dashboard. With that window on
/// screen the banner is the report, and a notification would repeat it;
/// with the window closed the notification is the only report there is —
/// and on macOS the window does not open by itself at all.
pub fn notify_startup_failure(app: &AppHandle, cause: &str) {
    if window_is_visible(app) {
        tracing::debug!("the dashboard is showing the startup failure: {cause}");
        return;
    }
    notify_gateway_failure(app, cause);
}

/// Whether the dashboard window is on screen.
fn window_is_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

// TODO(deep-link routing): a link opens the application and brings the
// window forward, and the URL is handed to the dashboard as an event
// (`DEEP_LINK_EVENT`, payload: the URL as a string). Turning that URL into
// a *page* is a frontend change, and `ui/` is not this crate's to edit, so
// it is deliberately not made here.
//
// A follow-up has to change exactly two places:
//
//   1. `ui/src/api.ts` — add a listener helper next to
//      `onGatewayStateChanged`, which is the file's existing pattern for
//      an event that arrives from Rust:
//
//          export const DEEP_LINK_EVENT = "zroutery://deep-link";
//
//          export function onDeepLink(
//            handler: (url: string) => void,
//          ): Promise<UnlistenFn> {
//            return listen(DEEP_LINK_EVENT, (event) => handler(event.payload));
//          }
//
//      The event name has to match `DEEP_LINK_EVENT` in
//      `src-tauri/src/shell.rs`; the two are a pair, the same way
//      `GATEWAY_STATE_EVENT` is.
//
//   2. `ui/src/App.tsx` — the `Shell` component keeps the current page in
//      `const [page, setPage] = useState<Page>("overview")` (the `Page`
//      union is declared at the top of the file), and nothing outside the
//      component can change it today. The deep-link handler has to be
//      wired into `Shell` — next to the `onGatewayStateChanged` effect —
//      and call `setPage` with the page the URL names, e.g.
//      `zroutery://providers` becomes `setPage("providers")`.
//
// Until that lands, a link opens the application and stops there.

#[cfg(test)]
mod tests {
    use super::DEEP_LINK_SCHEME;

    /// The scheme this code names and the scheme the configuration
    /// registers have to be the same string.
    ///
    /// They are written in two places on purpose — a constant in Rust and
    /// the manifest the bundler reads — and the plugin matches a link
    /// against the manifest only. If the two drift, every link is refused
    /// while the code believes it handled them, and nothing says so.
    #[test]
    fn the_configured_scheme_is_the_one_the_code_names() {
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .expect("the configuration parses");
        let schemes = config["plugins"]["deep-link"]["desktop"]["schemes"]
            .as_array()
            .expect("the deep-link schemes are declared in tauri.conf.json");
        assert!(
            schemes.iter().any(|scheme| scheme == DEEP_LINK_SCHEME),
            "the configuration does not register {DEEP_LINK_SCHEME}: {schemes:?}"
        );
    }
}
