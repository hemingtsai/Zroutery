//! A read-only preview server for the dashboard, over real application state.
//!
//! ```sh
//! cargo run -p zroutery --example preview
//! pnpm --dir ui dev
//! # open http://localhost:5174/preview.html
//! ```
//!
//! The server copies the running application's configuration directory (the
//! document, the durable usage ledger and the ML/takeover/cutover records) into
//! `target/preview-data`, builds the same [`Desktop`] the desktop shell builds,
//! and answers the dashboard's Tauri commands over `POST /invoke`.
//!
//! # Why a copy
//!
//! The desktop process owns this directory. A preview that opened the live
//! `runtime.sqlite3` would compete with its writer, and one that saved a
//! configuration would edit the file the application is using. The copy makes
//! the preview a reader: every command that mutates is refused, and the running
//! window is untouched.
//!
//! # Why the listener is not started
//!
//! The preview would have to bind the configured port, which the running
//! application already owns. The snapshot therefore reads the live gateway's
//! port directly: when 127.0.0.1:8787 answers, the page is told the gateway is
//! running, because it is — in the process next to this one.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};

use zroutery_lib::platform::{self, APP_ID};
use zroutery_lib::secrets::KeychainSecrets;
use zroutery_lib::state::Desktop;
use zroutery_lib::store;

#[derive(Debug)]
struct Options {
    from: PathBuf,
    dir: PathBuf,
    port: u16,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Self {
            from: platform::default_config_dir(),
            dir: PathBuf::from("target/preview-data"),
            port: 8786,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--from" => options.from = PathBuf::from(args.next().ok_or("--from needs a path")?),
                "--dir" => options.dir = PathBuf::from(args.next().ok_or("--dir needs a path")?),
                "--port" => {
                    options.port = args
                        .next()
                        .ok_or("--port needs a number")?
                        .parse()
                        .map_err(|error| format!("--port: {error}"))?
                }
                "--help" | "-h" => {
                    println!(
                        "usage: preview [--from <live config dir>] [--dir <preview dir>] [--port <port>]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(options)
    }
}

struct Preview {
    desktop: Arc<Desktop>,
    source: PathBuf,
    copied: PathBuf,
    gateway_port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse().map_err(|error| format!("{error}\n\nTry --help."))?;
    if !options.from.join(store::FILE_NAME).is_file() {
        return Err(format!(
            "no configuration at {}; pass --from <dir>",
            options.from.display()
        )
        .into());
    }

    copy_live_state(&options.from, &options.dir)?;

    let (config, warning) = store::load(&options.dir);
    if let Some(warning) = &warning {
        eprintln!("[preview] {warning}");
    }
    let live_port = config.server.port;
    let desktop = Arc::new(Desktop::new(
        options.dir.clone(),
        config,
        Arc::new(KeychainSecrets::new(APP_ID)),
    ));

    let preview = Arc::new(Preview {
        desktop,
        source: options.from.clone(),
        copied: options.dir.clone(),
        gateway_port: live_port,
    });

    let app = Router::new()
        .route("/invoke", post(invoke).options(preflight))
        .route("/status", get(status))
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
        .with_state(Arc::clone(&preview));

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", options.port)).await?;
    println!(
        "[preview] listening on http://127.0.0.1:{} · data {} · live config {} (gateway port {})",
        options.port,
        options.dir.display(),
        options.from.display(),
        live_port
    );
    println!("[preview] open http://localhost:5174/preview.html (read-only)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Replace the preview copy with a fresh reading of the live directory.
///
/// `browser-profiles` is deliberately not copied: it is hundreds of megabytes
/// of Chromium state, and nothing the preview renders reads it.
fn copy_live_state(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;

    // The database is copied with its write-ahead log so a running application's
    // latest settlements are visible; the `-shm` index is rebuilt by SQLite, and
    // a stale copy of it would be worse than none.
    for name in ["runtime.sqlite3", "runtime.sqlite3-wal"] {
        let source = from.join(name);
        let target = to.join(name);
        if target.exists() {
            std::fs::remove_file(&target)
                .map_err(|error| format!("cannot clear {}: {error}", target.display()))?;
        }
        if source.exists() {
            std::fs::copy(&source, &target)
                .map_err(|error| format!("cannot copy {}: {error}", source.display()))?;
        }
    }

    for name in ["config.json", "spend.json", "checkin-schedule.json"] {
        let source = from.join(name);
        if source.exists() {
            std::fs::copy(&source, to.join(name))
                .map_err(|error| format!("cannot copy {}: {error}", source.display()))?;
        }
    }

    for name in ["ml", "takeover", "migration"] {
        let source = from.join(name);
        if source.is_dir() {
            copy_dir(&source, &to.join(name))?;
        }
    }

    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|error| format!("{}: {error}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|error| format!("{}: {error}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", from.display()))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            // A lock file held by the live process is not needed for a reading;
            // skipping it is better than failing the whole preview.
            if let Err(error) = std::fs::copy(&source, &target) {
                eprintln!("[preview] skipped {}: {error}", source.display());
            }
        }
    }
    Ok(())
}

async fn preflight() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, GET, OPTIONS"),
    );
    response
}

async fn status(State(preview): State<Arc<Preview>>) -> Response {
    with_cors(
        Json(json!({
            "mode": "live-preview",
            "read_only": true,
            "source": preview.source.display().to_string(),
            "copy": preview.copied.display().to_string(),
            "gateway": preview.gateway_port,
        }))
        .into_response(),
    )
}

async fn invoke(State(preview): State<Arc<Preview>>, Json(request): Json<Value>) -> Response {
    let command = request
        .get("cmd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
    let reply = match dispatch(&preview, &command, &args).await {
        Ok(data) => json!({ "ok": true, "data": data }),
        Err(error) => json!({ "ok": false, "error": error }),
    };
    with_cors(Json(reply).into_response())
}

fn with_cors(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, GET, OPTIONS"),
    );
    response
}

async fn dispatch(preview: &Arc<Preview>, command: &str, args: &Value) -> Result<Value, String> {
    let desktop = &preview.desktop;
    match command {
        "get_snapshot" => {
            let mut value = to_value(desktop.snapshot().await)?;
            let port = value
                .get("config")
                .and_then(|config| config.get("server"))
                .and_then(|server| server.get("port"))
                .and_then(Value::as_u64)
                .unwrap_or(0) as u16;
            patch_live_gateway(&mut value, port);
            Ok(value)
        }
        "get_activity" => to_value(desktop.activity()),
        "get_usage_days" => {
            let days = args.get("days").and_then(Value::as_u64).unwrap_or(30) as u32;
            let desktop = Arc::clone(desktop);
            let rows = tokio::task::spawn_blocking(move || desktop.usage_days(days))
                .await
                .map_err(|error| format!("usage days task failed: {error}"))??;
            to_value(rows)
        }
        "query_usage_history" => {
            let query: zroutery_core::stats::UsageHistoryQuery =
                serde_json::from_value(args.get("query").cloned().unwrap_or_else(|| json!({})))
                    .map_err(|error| format!("invalid usage history query: {error}"))?;
            let desktop = Arc::clone(desktop);
            let page = tokio::task::spawn_blocking(move || desktop.query_usage_history(&query))
                .await
                .map_err(|error| format!("usage history task failed: {error}"))??;
            to_value(page)
        }
        "get_usage_sources" => {
            let days = args.get("days").and_then(Value::as_u64).unwrap_or(30) as u32;
            let desktop = Arc::clone(desktop);
            let local = tokio::task::spawn_blocking(move || {
                let activity = desktop.activity();
                Ok::<_, String>(json!({
                    "source_name": "This device",
                    "revision": 0,
                    "summary": activity.summary,
                    "recent": activity.recent,
                    "days": desktop.usage_days(days)?,
                }))
            })
            .await
            .map_err(|error| format!("usage sources task failed: {error}"))??;
            Ok(json!({ "local": local, "standby": null }))
        }
        "get_logs" => Ok(json!([])),
        "get_system_accent" => Ok(Value::Null),
        "deep_link_current" => Ok(Value::Null),
        // The guards mirror the ones on the `Desktop` methods in `state/ml.rs`:
        // a build without a capability has no command for it either, and the
        // dashboard is expected to ask `ml_available` / `account_available` on
        // the snapshot before calling. These arms are the fact that absence
        // stands for, stated the way `commands.rs` states it.
        #[cfg(feature = "ml")]
        "get_ml_status" => to_value(desktop.ml_status()),
        #[cfg(not(feature = "ml"))]
        "get_ml_status" => Err("this build has no learning stack".into()),
        #[cfg(feature = "ml")]
        "get_ml_shadow" => {
            let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(5_000) as usize;
            to_value(desktop.ml_shadow_analysis(limit))
        }
        #[cfg(not(feature = "ml"))]
        "get_ml_shadow" => Err("this build has no learning stack".into()),
        #[cfg(feature = "ml")]
        "get_ml_traces" => to_value(desktop.ml_traces()),
        #[cfg(not(feature = "ml"))]
        "get_ml_traces" => Err("this build has no learning stack".into()),
        "takeover_status" => {
            let views = desktop
                .takeover_views()
                .into_iter()
                .map(|(agent, status)| json!({ "agent": agent, "status": status }))
                .collect::<Vec<_>>();
            Ok(Value::Array(views))
        }
        "migration_status" => to_value(desktop.migration_status()),
        "migration_previews" => to_value(desktop.migration_previews()),
        #[cfg(feature = "account")]
        "reconcile_accounts" => to_value(desktop.reconcile_accounts().await),
        #[cfg(not(feature = "account"))]
        "reconcile_accounts" => Err("this build has no account subsystem".into()),
        "refresh_balances" => {
            desktop.refresh_all_balances().await;
            to_value(desktop.snapshot().await)
        }
        "refresh_balance" => {
            let provider = arg_str(args, "providerId")?;
            desktop.refresh_balance(provider).await?;
            to_value(desktop.snapshot().await)
        }
        "copy_text" | "get_tray_snapshot" => Ok(Value::Null),
        other => Err(format!(
            "实时预览是只读的：`{other}` 未在预览中开放，配置与真实运行中的应用均不会被修改。"
        )),
    }
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing `{name}`"))
}

fn to_value<T: serde::Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

/// Tell the page that the real gateway is running when it is.
///
/// The preview deliberately starts no listener, so its own snapshot says
/// `running: false` while the application next to it is serving on 8787. The
/// dashboard's status, its gateway controls and the takeover/cutover readiness
/// all read this field, and a preview that showed the live gateway as stopped
/// would be reviewing a different application.
fn patch_live_gateway(value: &mut Value, port: u16) {
    if port == 0 {
        return;
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let reachable = TcpStream::connect_timeout(&addr, Duration::from_millis(120)).is_ok();
    if !reachable {
        return;
    }
    let Some(server) = value.get_mut("server").and_then(Value::as_object_mut) else {
        return;
    };
    server.insert("running".into(), json!(true));
    server.insert("address".into(), json!(addr.to_string()));
    server.insert("base_url".into(), json!(format!("http://{addr}")));
}
