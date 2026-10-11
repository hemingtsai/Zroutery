//! Diagnostic probe: what does Chrome actually do when two launches race for one
//! profile directory?
//!
//! Run deliberately, and read the output rather than the assertions:
//!
//! ```sh
//! cargo test -p zroutery --features account-maint --test checkin_concurrency_probe \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! **Several visible windows will open.**
//!
//! # Why this is a probe and not a gate
//!
//! It leans on Chrome's own behaviour for its result and opens several windows, so
//! it is not something CI should assert on. It exists because the operator-facing
//! symptom — a "your preferences file is corrupted" dialog, several windows at
//! once, and profiles that do not reuse — has more than one candidate cause, and
//! the way to tell them apart is to watch the browser do it.
//!
//! The shapes it distinguishes:
//!
//! * the second launch is **refused** by Zroutery, because a live browser owns the
//!   profile — the intended behaviour;
//! * the second launch **succeeds**, which means two browsers really did share one
//!   directory and that is the corruption;
//! * the second launch **fails or hangs**, which is what contention looks like from
//!   the outside.
//!
//! It also reports whether the profile directory is reusable afterwards, because
//! "not reused" is the other half of the report.

#![cfg(feature = "account-maint")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use zroutery_lib::checkin::cdp;

fn browser() -> Option<PathBuf> {
    match cdp::find_browser("") {
        Some(path) if path.is_file() => Some(path),
        _ => {
            eprintln!("skipping: no Chromium browser found");
            None
        }
    }
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zroutery-probe-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Whether something is still listening on a debugging port.
///
/// Used instead of `BrowserSession::is_running` after a `shutdown`, because
/// `shutdown` consumes the session: asking the session afterwards is asking a
/// value that has been moved.
async fn port_open(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

/// The files Chrome writes into a profile directory, and whether they parse.
///
/// Read rather than assumed: the operator's dialog names `Preferences`, so the
/// question is whether that file is valid JSON, not whether it exists.
fn profile_report(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in [
        "Default/Preferences",
        "Default/Secure Preferences",
        "Local State",
    ] {
        let path = dir.join(name);
        match std::fs::read(&path) {
            Err(e) => out.push((name.to_string(), format!("absent ({:?})", e.kind()))),
            Ok(bytes) => {
                let verdict = match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(_) => "parses".to_string(),
                    Err(e) => format!("DOES NOT PARSE: {e}"),
                };
                out.push((
                    name.to_string(),
                    format!("{} bytes, {verdict}", bytes.len()),
                ));
            }
        }
    }
    out
}

fn show(tag: &str, dir: &Path) {
    for (name, verdict) in profile_report(dir) {
        eprintln!("    {tag}/{name}: {verdict}");
    }
}

#[tokio::test]
#[ignore = "diagnostic probe: opens several visible browser windows"]
async fn probe_two_launches_against_one_profile() {
    let Some(executable) = browser() else { return };
    let dir = scratch("race");

    eprintln!("\n=== PROBE 1: launch once, then launch again on the SAME directory");
    let first = cdp::launch(&executable, &dir, "about:blank")
        .await
        .expect("the first launch must succeed");
    let first_port = first.port();
    eprintln!(
        "  first browser on port {first_port}, running: {}",
        port_open(first_port).await
    );
    show("after first launch", &dir);

    // The second launch is the one that used to corrupt the profile.
    match cdp::launch(&executable, &dir, "about:blank").await {
        Ok(second) => {
            let second_port = second.port();
            eprintln!(
                "  !! SECOND LAUNCH SUCCEEDED on port {second_port} while {first_port} was still live:\
                 \n     two browsers shared one profile directory, which is the corruption"
            );
            let _ = second.shutdown().await;
        }
        Err(e) => eprintln!("  second launch REFUSED: {e}"),
    }
    show("after second launch", &dir);

    eprintln!("\n  closing the first browser");
    match first.shutdown().await {
        Ok(()) => eprintln!("    shutdown reported closed"),
        Err(e) => eprintln!("    !! shutdown REFUSED: {e}"),
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
    eprintln!(
        "    port {first_port} still accepting connections: {}",
        port_open(first_port).await
    );
    show("after closing", &dir);

    eprintln!("\n=== PROBE 2: relaunch on the same directory (is the profile reused?)");
    match cdp::launch(&executable, &dir, "about:blank").await {
        Ok(third) => {
            let p = third.port();
            eprintln!("  relaunch succeeded on port {p}: the profile is reusable");
            let _ = third.shutdown().await;
        }
        Err(e) => eprintln!("  !! relaunch REFUSED, which would break reuse: {e}"),
    }
    eprintln!("  files left in the profile root:");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("read profile root")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    names.sort();
    for n in &names {
        eprintln!("    {n}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "diagnostic probe: opens several visible browser windows"]
async fn probe_separate_directories_open_separately() {
    // The control for probe 1. Two directories must give two independent browsers,
    // which is what "one browser per account" means. If this interferes with itself
    // then the cause is not contention but something in the launch path.
    let Some(executable) = browser() else { return };
    let a = scratch("iso-a");
    let b = scratch("iso-b");

    eprintln!("\n=== PROBE 3: two DIFFERENT directories");
    let first = cdp::launch(&executable, &a, "about:blank")
        .await
        .expect("first directory");
    let second = cdp::launch(&executable, &b, "about:blank")
        .await
        .expect("second directory, which must not be refused as contended");
    let (pa, pb) = (first.port(), second.port());
    eprintln!(
        "  both live on distinct ports {pa} / {pb}: {} / {}",
        port_open(pa).await,
        port_open(pb).await
    );

    let _ = first.shutdown().await;
    let _ = second.shutdown().await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    eprintln!(
        "  after shutdown {pa} / {pb} still open: {} / {}",
        port_open(pa).await,
        port_open(pb).await
    );
    show("a", &a);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}
