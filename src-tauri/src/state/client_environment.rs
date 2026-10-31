//! Explain higher-precedence Gemini configuration without exposing its values.

use std::path::{Path, PathBuf};

pub(super) fn gemini_notes(settings: &Path) -> Vec<String> {
    let mut notes = vec![
        "Gemini CLI must be reopened after synchronization; --model and already-running sessions can override the selected default".into(),
    ];
    let global_env = settings.with_file_name(".env");
    if let Ok(cwd) = std::env::current_dir() {
        notes.extend(project_notes(&cwd, settings, &global_env));
    }
    for variable in [
        "GOOGLE_GEMINI_BASE_URL",
        "GEMINI_API_KEY",
        "GEMINI_MODEL",
        "GOOGLE_GENAI_API_VERSION",
        "GEMINI_API_KEY_AUTH_MECHANISM",
    ] {
        if std::env::var_os(variable).is_some() {
            notes.push(format!("the Zroutery launch environment contains {variable}; inherited process variables can override Gemini's managed .env"));
        }
    }
    let system = std::env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(target_os = "windows")]
            {
                PathBuf::from(
                    std::env::var_os("PROGRAMDATA").unwrap_or_else(|| "C:\\ProgramData".into()),
                )
                .join("gemini-cli/settings.json")
            }
            #[cfg(target_os = "macos")]
            {
                PathBuf::from("/Library/Application Support/GeminiCli/settings.json")
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                PathBuf::from("/etc/gemini-cli/settings.json")
            }
        });
    if system.is_file() {
        notes.push(format!(
            "Gemini system settings at {} may enforce authentication or override the user default",
            system.display()
        ));
    }
    notes
}

fn project_notes(cwd: &Path, settings: &Path, global_env: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    for directory in cwd.ancestors() {
        if let Some(env) = [directory.join(".gemini/.env"), directory.join(".env")]
            .into_iter()
            .find(|path| path.is_file())
        {
            if env != global_env {
                notes.push(format!("the launch directory selects {}; Gemini loads the nearest .env instead of merging the managed global file", env.display()));
            }
            break;
        }
    }
    let workspace = cwd.join(".gemini/settings.json");
    if workspace != settings && workspace.is_file() {
        notes.push(format!("trusted workspace settings at {} can override the user authentication and model; other launch directories need their own verification", workspace.display()));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearer_env_shadows_global_even_without_any_managed_key() {
        let dir = std::env::temp_dir().join(format!("zr-gemini-notes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("project/.gemini")).unwrap();
        std::fs::write(dir.join("project/.env"), "OTHER=private-value\n").unwrap();
        std::fs::write(dir.join("project/.gemini/settings.json"), "{}").unwrap();
        let notes = project_notes(
            &dir.join("project"),
            &dir.join("home/.gemini/settings.json"),
            &dir.join("home/.gemini/.env"),
        );
        assert_eq!(notes.len(), 2);
        assert!(notes[0].contains("instead of merging"));
        assert!(!notes.join(" ").contains("private-value"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
