//! Headless proxy: the same wiring as the desktop app without the GUI.
//!
//! Useful on machines without a session (CI, remote boxes) and for verifying a
//! configuration quickly:
//!
//! ```sh
//! ZROUTERY_CONFIG_DIR=/tmp/zr zroutery-headless
//! ```
//!
//! API keys come from the keychain when available, otherwise from
//! `ZROUTERY_KEY_PROVIDER_<ID>` environment variables.

use std::path::PathBuf;
use std::sync::Arc;

use zroutery_lib::secrets::KeychainSecrets;
use zroutery_lib::state::Desktop;
use zroutery_lib::store;

const KEYCHAIN_SERVICE: &str = "app.zroutery.desktop";

fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ZROUTERY_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library/Application Support")
        .join(KEYCHAIN_SERVICE)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("ZROUTERY_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let (config, warning) = store::load(&dir);
    if let Some(w) = &warning {
        tracing::warn!("{w}");
    }
    store::save(&dir, &config)?;

    for issue in config.validate() {
        tracing::warn!("{}: {}", issue.code, issue.message);
    }

    let desktop = Arc::new(Desktop::new(
        dir.clone(),
        config,
        // Headless runs may have no keychain access, so this is the one place
        // where ZROUTERY_KEY_* variables are honoured.
        Arc::new(KeychainSecrets::with_env_fallback(KEYCHAIN_SERVICE)),
    ));
    desktop.start().await.map_err(|e| e.to_string())?;

    let snapshot = desktop.snapshot().await;
    println!(
        "Zroutery {} listening on {}",
        snapshot.version,
        snapshot.server.base_url.as_deref().unwrap_or("(not bound)")
    );
    println!("config:  {}", snapshot.config_path);
    println!(
        "models:  {}",
        snapshot
            .config
            .models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.exposed_id())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if snapshot.server.require_auth {
        // Headless has no clipboard and no dashboard, so the token is printed
        // here; the GUI only ever shows the hint.
        println!("token:   {}", desktop.auth_token());
    } else {
        println!("token:   authentication disabled");
    }

    tokio::signal::ctrl_c().await?;
    println!("\nshutting down");
    desktop.stop().await;
    Ok(())
}
