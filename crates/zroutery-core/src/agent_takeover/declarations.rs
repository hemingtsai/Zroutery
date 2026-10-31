//! Discovery and file locations are data. Protocol-specific fields remain in
//! gateway.rs and every JSON/TOML declaration shares the same field editor.

use std::path::PathBuf;

use super::clients::AgentType;
use super::fs::{config_dir_override, home_dir};

#[cfg(test)]
thread_local! {
    /// Deterministic executable search roots for adapter fixtures. An isolated
    /// config home must never fall through to the developer's real PATH.
    static TEST_BINARY_SEARCH: std::cell::RefCell<Option<(Vec<PathBuf>, Vec<String>)>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct BinarySearchIsolation(Option<(Vec<PathBuf>, Vec<String>)>);

#[cfg(test)]
impl Drop for BinarySearchIsolation {
    fn drop(&mut self) {
        let previous = self.0.take();
        TEST_BINARY_SEARCH.with(|slot| *slot.borrow_mut() = previous);
    }
}

#[cfg(test)]
pub(crate) fn isolate_binary_search(
    paths: Vec<PathBuf>,
    suffixes: Vec<String>,
) -> BinarySearchIsolation {
    let previous = TEST_BINARY_SEARCH.with(|slot| slot.replace(Some((paths, suffixes))));
    BinarySearchIsolation(previous)
}

pub(super) struct Declaration {
    pub binary: &'static str,
    environment: &'static str,
    default_directory: &'static str,
    relative: &'static str,
}

impl AgentType {
    pub(super) fn declaration(self) -> Declaration {
        match self {
            Self::Claude => Declaration {
                binary: "claude",
                environment: "CLAUDE_CONFIG_DIR",
                default_directory: ".claude",
                relative: "settings.json",
            },
            Self::Codex => Declaration {
                binary: "codex",
                environment: "CODEX_HOME",
                default_directory: ".codex",
                relative: "config.toml",
            },
            Self::Gemini => Declaration {
                binary: "gemini",
                environment: "GEMINI_CLI_HOME",
                default_directory: "",
                relative: ".gemini/settings.json",
            },
            Self::OpenCode => Declaration {
                binary: "opencode",
                environment: "XDG_CONFIG_HOME",
                default_directory: ".config",
                relative: "opencode/opencode.json",
            },
        }
    }
}

impl Declaration {
    pub(super) fn config_path(&self) -> Result<PathBuf, String> {
        let directory = match config_dir_override(self.environment) {
            Some(path) => path,
            None => home_dir()?.join(self.default_directory),
        };
        Ok(directory.join(self.relative))
    }

    pub(super) fn detected(&self) -> Result<bool, String> {
        if self.config_path()?.is_file() {
            return Ok(true);
        }
        self.binary_detected()
    }

    /// Detect only the declared executable. Gemini uses this rather than
    /// `detected`: Antigravity also writes `~/.gemini`, so that directory is not
    /// evidence that Gemini CLI itself is installed.
    pub(super) fn binary_detected(&self) -> Result<bool, String> {
        #[cfg(test)]
        {
            if let Some((paths, suffixes)) = TEST_BINARY_SEARCH.with(|slot| slot.borrow().clone()) {
                return Ok(find_binary(self.binary, paths.into_iter(), &suffixes));
            }
            if super::fs::isolated_agent_home().is_some() {
                // A fixture's empty home must not borrow the developer's PATH.
                return Ok(false);
            }
        }
        let paths = std::env::var_os("PATH").unwrap_or_default();
        let suffixes: Vec<String> = if cfg!(windows) {
            let mut suffixes = vec![String::new()];
            suffixes.extend(
                std::env::var("PATHEXT")
                    .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                    .split(';')
                    .filter(|suffix| suffix.starts_with('.') && !suffix.contains(['/', '\\']))
                    .map(str::to_owned),
            );
            suffixes
        } else {
            vec![String::new()]
        };
        Ok(find_binary(
            self.binary,
            std::env::split_paths(&paths),
            &suffixes,
        ))
    }
}

fn find_binary(binary: &str, paths: impl Iterator<Item = PathBuf>, suffixes: &[String]) -> bool {
    paths
        .filter(|path| !path.as_os_str().is_empty())
        .any(|directory| {
            suffixes.iter().any(|suffix| {
                let candidate = directory.join(format!("{binary}{suffix}"));
                let Ok(metadata) = std::fs::metadata(candidate) else {
                    return false;
                };
                if !metadata.is_file() {
                    return false;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                }
                #[cfg(not(unix))]
                {
                    true
                }
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_requires_a_file_and_never_executes_the_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let paths = || std::iter::once(temp.path().to_owned());
        assert!(!find_binary("gemini", paths(), &[".cmd".into()]));
        std::fs::create_dir(temp.path().join("gemini.cmd")).unwrap();
        assert!(!find_binary("gemini", paths(), &[".cmd".into()]));
        std::fs::remove_dir(temp.path().join("gemini.cmd")).unwrap();
        let file = temp.path().join("gemini.cmd");
        std::fs::write(&file, "should never run").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert!(find_binary("gemini", paths(), &[".cmd".into()]));
    }
}
