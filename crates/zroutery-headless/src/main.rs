//! Headless proxy: the same wiring as the desktop app without the GUI.
//!
//! Useful on machines without a session (CI, remote boxes) and for verifying a
//! configuration quickly:
//!
//! ```sh
//! ZROUTERY_CONFIG_DIR=/tmp/zr zroutery-headless
//! ```
//!
//! API keys come from the OS credential store when available, otherwise from
//! `ZROUTERY_KEY_PROVIDER_<ID>` environment variables.

use std::path::PathBuf;
use std::sync::Arc;

use zroutery_lib::platform::{self, APP_ID};
use zroutery_lib::secrets::KeychainSecrets;
use zroutery_lib::state::Desktop;
use zroutery_lib::store;

mod experiment;

/// Arguments after the program name, or an error naming what was expected.
fn subcommand_args(name: &str) -> Result<Vec<String>, String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some(found) if found == name => Ok(args.collect()),
        Some(other) => Err(format!("expected `{name}`, got `{other}`")),
        None => Err(format!("expected `{name}`")),
    }
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

    // `--experiment` is handled before any of the proxy wiring below. It builds its
    // own `AppState` over a fake provider environment and must not inherit a
    // developer's real configuration, real providers or real credentials, so it runs
    // first and exits.
    if std::env::args().any(|a| a == "--experiment") {
        let args = subcommand_args("--experiment")?;
        let mut state_dir = platform::default_config_dir().join("experiment");
        let mut requests_per_phase = 120usize;
        let mut exploration = 0.0f64;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--state-dir" => {
                    state_dir = PathBuf::from(
                        iter.next()
                            .ok_or("--state-dir needs a path")?
                            .clone(),
                    )
                }
                "--requests" => {
                    requests_per_phase = iter
                        .next()
                        .ok_or("--requests needs a count")?
                        .parse()
                        .map_err(|e| format!("--requests: {e}"))?
                }
                "--exploration" => {
                    exploration = iter
                        .next()
                        .ok_or("--exploration needs a probability")?
                        .parse()
                        .map_err(|e| format!("--exploration: {e}"))?
                }
                other => {
                    return Err(format!("unknown `--experiment` argument `{other}`").into())
                }
            }
        }
        let report = experiment::run(&state_dir, requests_per_phase, exploration).await?;
        print!("{}", report.render());
        return Ok(());
    }

    let dir = platform::default_config_dir();
    std::fs::create_dir_all(&dir)?;
    // The document is written back only when startup read it whole or created
    // it: a corrupt file whose backup could not be made stays where it is.
    let (config, warning) = match store::startup_load(&dir) {
        Ok(startup) => {
            if startup.token_to_write.is_some() {
                store::save(&dir, &startup.config)?;
            }
            (startup.config, startup.warning)
        }
        Err(e) => {
            tracing::error!("{e}");
            (
                store::with_defaults(zroutery_core::config::AppConfig::default()),
                Some(e),
            )
        }
    };
    if let Some(w) = &warning {
        tracing::warn!("{w}");
    }

    for issue in config.validate() {
        tracing::warn!("{}: {}", issue.code, issue.message);
    }

    let desktop = Arc::new(Desktop::new(
        dir.clone(),
        config,
        // Headless runs may have no credential-store access, so this is the
        // one place where ZROUTERY_KEY_* variables are honoured.
        Arc::new(KeychainSecrets::with_env_fallback(APP_ID)),
    ));

    // `--elect` is the same idea for routing: probe every tier member, print the
    // order it decided, and exit. Handy from a shell, and what the smoke test drives.
    if std::env::args().any(|a| a == "--elect") {
        let election = desktop.hold_election().await;
        if election.tiers.is_empty() {
            println!("no tier has an enabled model to measure");
        }
        for (tier, outcome) in &election.tiers {
            println!("{}:", tier.virtual_id());
            for ranked in &outcome.ranked {
                println!(
                    "  {:<34} {}",
                    ranked.model_id,
                    ranked.note.clone().unwrap_or_default()
                );
            }
            if let Some(note) = &outcome.note {
                println!("  ({note})");
            }
        }
        return Ok(());
    }

    // `--balances` is a diagnostic: ask every provider that publishes a balance,
    // print what came back, and exit without serving.
    if std::env::args().any(|a| a == "--balances") {
        let problems = desktop.refresh_all_balances().await;
        let balances = desktop.balances();
        if balances.is_empty() {
            println!("no provider is configured with a balance endpoint");
        }
        for (provider_id, status) in balances {
            match (status.balance, status.error) {
                (Some(b), _) => println!(
                    "{provider_id}: {} {} remaining",
                    b.remaining
                        .or(b.total)
                        .map(|v| format!("{v:.2}"))
                        .unwrap_or_else(|| "?".into()),
                    b.currency
                ),
                (None, Some(e)) => println!("{provider_id}: check failed: {e}"),
                (None, None) => println!("{provider_id}: no answer"),
            }
        }
        return if problems.is_empty() {
            Ok(())
        } else {
            Err(format!("{} provider(s) failed", problems.len()).into())
        };
    }

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

    // Spend is flushed periodically as well as at shutdown, so a kill costs seconds
    // of history rather than the whole run.
    let keeper = Arc::clone(&desktop);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            tick.tick().await;
            keeper.flush_ledger();
        }
    });

    // Ctrl-C everywhere; SIGTERM as well where it exists. A supervisor or a
    // plain `kill` should still get the clean shutdown, because that is when
    // the ledger is written.
    platform::shutdown_signal().await;
    println!("\nshutting down");
    desktop.stop().await;
    Ok(())
}
