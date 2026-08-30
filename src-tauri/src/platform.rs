//! Platform plumbing shared by the GUI and the headless binary.
//!
//! One implementation of "where does the configuration live" and "what does
//! shutdown look like", so the two entry points cannot drift apart — and so
//! neither hard-codes a macOS path the way the first versions did.

use std::path::PathBuf;

pub const APP_ID: &str = "app.zroutery.desktop";

/// Where the configuration and spend ledger live.
///
/// `ZROUTERY_CONFIG_DIR` wins over everything: it is the escape hatch for
/// tests, dotfile repos and portable installs. Otherwise each platform uses
/// its own convention for per-application settings.
pub fn default_config_dir() -> PathBuf {
    config_dir_from(std::env::var("ZROUTERY_CONFIG_DIR").ok().as_deref())
}

/// The decision logic of [`default_config_dir`], with the environment handed
/// in so tests never race each other over the real one.
fn config_dir_from(env_override: Option<&str>) -> PathBuf {
    if let Some(dir) = env_override.filter(|d| !d.trim().is_empty()) {
        return PathBuf::from(dir);
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(APP_ID)
    }

    #[cfg(target_os = "windows")]
    {
        // %APPDATA%\app.zroutery.desktop — the same location Tauri's
        // app_config_dir resolves to, so the GUI (which asks Tauri) and
        // headless (which asks us) agree on one directory.
        std::env::var("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join(APP_ID))
            .unwrap_or_else(|_| {
                std::env::var("USERPROFILE")
                    .map(|home| {
                        PathBuf::from(home)
                            .join("AppData")
                            .join("Roaming")
                            .join(APP_ID)
                    })
                    .unwrap_or_else(|_| PathBuf::from(".").join(APP_ID))
            })
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        // XDG says the config dir is ~/.config/<id>, matching Tauri's
        // app_config_dir on Linux.
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".config").join(APP_ID)
    }
}

/// Wait for the platform's shutdown signal.
///
/// Unix daemons are expected to survive and clean up after SIGTERM; Windows
/// services and console programs get Ctrl-C (and Ctrl-Break, delivered the
/// same way by tokio).
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_variable_wins_over_the_platform_default() {
        assert_eq!(
            config_dir_from(Some("E:\\Zroutery\\target\\test-cfg")),
            PathBuf::from("E:\\Zroutery\\target\\test-cfg")
        );
        // An empty override is no override at all.
        let fallback = config_dir_from(None);
        assert_eq!(config_dir_from(Some("   ")), fallback);
    }

    #[test]
    fn the_platform_default_lands_under_the_app_directory() {
        let dir = config_dir_from(None);
        assert!(
            dir.ends_with(APP_ID),
            "got {}",
            dir.display()
        );
        // And it is never just the app id on its own: there is always a
        // parent directory from the platform.
        assert!(dir.parent().is_some_and(|p| !p.as_os_str().is_empty()));
    }
}
