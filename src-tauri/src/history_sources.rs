//! Which history sources this machine already has, and which were used before.
//!
//! A history import needs a path, and asking for one every time is a tax on a
//! source that never moves: CC Switch keeps its usage database in one known
//! directory, and Claude Code keeps its transcripts in another. This module
//! names those paths so the dashboard can offer them, and remembers paths a
//! person chose by hand so a copy in an unusual place does not have to be found
//! twice.
//!
//! Discovery only asks the filesystem whether a path exists; it never opens a
//! database or reads a transcript. Deciding that a source is usable stays the
//! preview's job, where its reason can be shown next to the counts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the remembered paths are kept, beside the application's own config.
const MEMORY_FILE: &str = "import-sources.json";

/// How many remembered paths survive. One per tool is the useful case; the
/// extra room covers a relocated copy without letting the list grow forever.
const REMEMBERED: usize = 5;

/// How many directory entries discovery will look at before it gives up on
/// finding a transcript. A folder of transcripts is noticed within its first
/// project directory in practice, so this is a safety valve, not a limit a real
/// installation reaches.
const SCAN_BUDGET: usize = 500;

/// One source the dashboard can offer without opening a file dialog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySourceOffer {
    /// The adapter family the path belongs to, as the preview will report it.
    pub source: String,
    /// The name a person recognises: "CC Switch", "Claude Code".
    pub label: String,
    pub path: String,
    /// True when this path was imported on this machine before.
    pub recent: bool,
}

/// A path a person chose by hand, kept so it can be offered again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RememberedSource {
    pub path: String,
    pub source: String,
}

/// The places discovery looks, with the environment handed in so tests never
/// touch the real home directory.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    home: Option<PathBuf>,
    appdata: Option<PathBuf>,
    /// `ZROUTERY_CC_SWITCH_DIR`, when the operator pointed the provider import
    /// at a specific installation.
    cc_switch: Option<PathBuf>,
}

/// The name a person recognises for an adapter family.
pub fn label(source: &str) -> &'static str {
    match source {
        "ccswitch" => "CC Switch",
        "claudecode" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        _ => "another tool",
    }
}

impl Discovery {
    /// The real machine's directories, from the environment.
    pub fn from_env() -> Self {
        let dir = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            home: dir("USERPROFILE").or_else(|| dir("HOME")),
            appdata: dir("APPDATA"),
            cc_switch: dir("ZROUTERY_CC_SWITCH_DIR"),
        }
    }

    /// Every path that exists and holds a source the importer understands.
    fn candidates(&self) -> Vec<(&'static str, PathBuf)> {
        let mut found: Vec<(&'static str, PathBuf)> = Vec::new();
        if let Some(dir) = &self.cc_switch {
            push_database(&mut found, dir);
        }
        if let Some(home) = &self.home {
            push_database(&mut found, &home.join(".cc-switch"));
            let projects = home.join(".claude").join("projects");
            if has_transcript(&projects) {
                found.push(("claudecode", projects));
            }
            let opencode = home.join(".local/share/opencode/opencode.db");
            if opencode.is_file() {
                found.push(("opencode", opencode));
            }
            push_database(
                &mut found,
                &home.join("Library/Application Support/com.ccswitch.desktop"),
            );
            push_database(&mut found, &home.join(".config/com.ccswitch.desktop"));
        }
        if let Some(appdata) = &self.appdata {
            push_database(&mut found, &appdata.join("com.ccswitch.desktop"));
        }
        found
    }
}

/// Add `dir/cc-switch.db` when it is a file discovery found the same database
/// through more than one root, so the first spelling wins.
fn push_database(found: &mut Vec<(&'static str, PathBuf)>, dir: &Path) {
    let db = dir.join("cc-switch.db");
    if db.is_file() && !found.iter().any(|(_, path)| same_path(path, &db)) {
        found.push(("ccswitch", db));
    }
}

/// Whether `dir` holds at least one transcript, giving up after `SCAN_BUDGET`
/// entries. Claude Code nests one file per conversation, so the answer arrives
/// from the first project that has one.
fn has_transcript(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let mut left = SCAN_BUDGET;
    walk_for_transcript(dir, 0, &mut left)
}

fn walk_for_transcript(dir: &Path, depth: usize, left: &mut usize) -> bool {
    // The adapter searches six levels below the chosen root; discovery only has
    // to find one file, so it stops sooner rather than walking a whole tree.
    if depth > 4 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        if *left == 0 {
            return false;
        }
        *left -= 1;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if walk_for_transcript(&path, depth + 1, left) {
                return true;
            }
        } else if kind.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
        {
            return true;
        }
    }
    false
}

/// Compare two paths as the filesystem spells them, which on Windows folds case
/// and on every platform resolves links. A path that cannot be resolved is
/// compared as written.
fn same_path(one: &Path, other: &Path) -> bool {
    match (one.canonicalize(), other.canonicalize()) {
        (Ok(one), Ok(other)) => one == other,
        _ => one == other,
    }
}

/// The offers to show: sources found on this machine first, then paths used
/// before that are no longer at a conventional location. Recently used sources
/// sort ahead of the rest, and each path appears once.
pub fn offers(discovery: &Discovery, remembered: &[RememberedSource]) -> Vec<HistorySourceOffer> {
    let mut offers: Vec<HistorySourceOffer> = Vec::new();
    for (source, path) in discovery.candidates() {
        offers.push(HistorySourceOffer {
            source: source.to_string(),
            label: label(source).to_string(),
            path: path.display().to_string(),
            recent: false,
        });
    }
    for entry in remembered {
        let path = Path::new(&entry.path);
        if !path.exists() {
            continue;
        }
        match offers
            .iter_mut()
            .find(|offer| same_path(Path::new(&offer.path), path))
        {
            Some(offer) => offer.recent = true,
            None => offers.push(HistorySourceOffer {
                source: entry.source.clone(),
                label: label(&entry.source).to_string(),
                path: entry.path.clone(),
                recent: true,
            }),
        }
    }
    // Stable sort: a recently used source moves to the front, the order within
    // each group stays as declared.
    offers.sort_by_key(|offer| !offer.recent);
    offers
}

/// The remembered paths, most recent first. A missing or unreadable file is an
/// empty list: a convenience that was lost must not stop an import.
pub fn load(config_dir: &Path) -> Vec<RememberedSource> {
    std::fs::read_to_string(config_dir.join(MEMORY_FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Record a source that was just read, and return the new list.
///
/// The write is atomic and a failure is only reported: remembering the path is
/// a convenience, and refusing a preview because the note could not be filed
/// would turn it into a requirement.
pub fn remember(
    config_dir: &Path,
    path: &str,
    source: &str,
) -> Result<Vec<RememberedSource>, String> {
    if path.trim().is_empty() || source.trim().is_empty() {
        return Ok(load(config_dir));
    }
    let mut list = load(config_dir);
    list.retain(|entry| entry.path != path);
    list.insert(
        0,
        RememberedSource {
            path: path.to_string(),
            source: source.to_string(),
        },
    );
    list.truncate(REMEMBERED);
    let text = serde_json::to_string_pretty(&list).map_err(|error| error.to_string())?;
    crate::store::write_atomically(config_dir, MEMORY_FILE, &text)?;
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zroutery-sources-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"x").unwrap();
    }

    fn discovery(home: &Path) -> Discovery {
        Discovery {
            home: Some(home.to_path_buf()),
            appdata: None,
            cc_switch: None,
        }
    }

    #[test]
    fn a_source_in_its_conventional_place_is_found_without_a_dialog() {
        let home = root();
        touch(&home.join(".cc-switch/cc-switch.db"));
        touch(&home.join(".claude/projects/alpha/session.jsonl"));
        let offers = offers(&discovery(&home), &[]);
        let found: Vec<&str> = offers.iter().map(|offer| offer.source.as_str()).collect();
        assert_eq!(found, ["ccswitch", "claudecode"]);
        assert_eq!(offers[0].label, "CC Switch");
        assert_eq!(offers[1].label, "Claude Code");
        assert!(offers[0].path.ends_with("cc-switch.db"));
        assert!(offers[1].path.ends_with("projects"));
        assert!(!offers[0].recent && !offers[1].recent);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_home_without_either_source_offers_nothing() {
        let home = root();
        // The projects folder exists but holds no transcript: a client that was
        // installed and never used must not be offered as importable.
        std::fs::create_dir_all(home.join(".claude/projects/empty")).unwrap();
        assert!(offers(&discovery(&home), &[]).is_empty());
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_database_found_through_two_roots_is_offered_once() {
        let home = root();
        touch(&home.join(".cc-switch/cc-switch.db"));
        let appdata = home.join("AppData/Roaming");
        touch(&appdata.join("com.ccswitch.desktop/cc-switch.db"));
        let discovery = Discovery {
            home: Some(home.clone()),
            appdata: Some(appdata),
            cc_switch: None,
        };
        assert_eq!(
            offers(&discovery, &[]).len(),
            2,
            "one per root, not one per row"
        );
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_path_chosen_by_hand_is_offered_again_and_marked_recent() {
        let home = root();
        let copy = home.join("backups/old-cc-switch.db");
        touch(&copy);
        let remembered = vec![RememberedSource {
            path: copy.display().to_string(),
            source: "ccswitch".into(),
        }];
        let offers = offers(&discovery(&home), &remembered);
        assert_eq!(offers.len(), 1);
        assert!(offers[0].recent);
        assert_eq!(offers[0].label, "CC Switch");
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_remembered_path_that_vanished_is_dropped() {
        let home = root();
        let remembered = vec![RememberedSource {
            path: home.join("gone/cc-switch.db").display().to_string(),
            source: "ccswitch".into(),
        }];
        assert!(offers(&discovery(&home), &remembered).is_empty());
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn a_detected_source_used_before_is_first_and_marked_once() {
        let home = root();
        touch(&home.join(".cc-switch/cc-switch.db"));
        touch(&home.join(".claude/projects/alpha/session.jsonl"));
        let remembered = vec![RememberedSource {
            path: home.join(".claude/projects").display().to_string(),
            source: "claudecode".into(),
        }];
        let offers = offers(&discovery(&home), &remembered);
        assert_eq!(offers.len(), 2, "the remembered path is the detected one");
        assert_eq!(offers[0].source, "claudecode");
        assert!(offers[0].recent);
        assert_eq!(offers[1].source, "ccswitch");
        assert!(!offers[1].recent);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn the_newest_paths_are_kept_once_and_capped() {
        let dir = root();
        let mut paths = Vec::new();
        for index in 0..REMEMBERED + 2 {
            let path = dir.join(format!("copy-{index}.db"));
            touch(&path);
            paths.push(path.display().to_string());
            remember(&dir, &paths[index], "ccswitch").unwrap();
        }
        let list = load(&dir);
        assert_eq!(list.len(), REMEMBERED, "older paths were dropped");
        assert_eq!(
            list[0].path,
            paths[REMEMBERED + 1],
            "the newest path is first"
        );
        assert!(
            list.iter().all(|entry| entry.path != paths[0]),
            "the oldest path was dropped: {list:?}"
        );

        // Choosing a path that is still remembered moves it to the front
        // instead of adding a second copy of it.
        let list = remember(&dir, &paths[3], "ccswitch").unwrap();
        assert_eq!(list.len(), REMEMBERED, "a reused path is not duplicated");
        assert_eq!(list[0].path, paths[3]);
        assert_eq!(list, load(&dir));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_corrupt_memory_file_is_an_empty_list_not_an_error() {
        let dir = root();
        std::fs::write(dir.join(MEMORY_FILE), b"not json").unwrap();
        assert!(load(&dir).is_empty());
        let list = remember(&dir, "C:/fixture/cc-switch.db", "ccswitch").unwrap();
        assert_eq!(list.len(), 1, "a corrupt file is replaced, not appended to");
        std::fs::remove_dir_all(dir).ok();
    }
}
