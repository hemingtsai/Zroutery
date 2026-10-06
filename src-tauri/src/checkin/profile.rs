//! Browser profile isolation and executable discovery.
//!
//! # Why isolation is per `(provider_id, account_id)` and load-bearing
//!
//! A persistent browser profile is a cookie jar. Sharing one across accounts
//! would mean the session that solves one account's challenge authenticates as
//! another, that a WAF clearance cookie granted to one relay is replayed at a
//! different one, and — worst of all — that a check-in performed for account A
//! is carried out as account B's identity while the report attributes the reward
//! to A. Every one of those is a credential leak that looks like a success.
//!
//! So the profile directory is a pure function of the pair, two different pairs
//! can never collide, and the derivation is testable without a browser.
//!
//! # Why the path is not just the two ids joined
//!
//! Provider and account ids are user-supplied, so they can contain path
//! separators, `..`, or characters Windows will not accept in a directory name.
//! [`profile_dir`] sanitises both and appends a short digest of the *original*
//! pair, which does three things: two ids that sanitise to the same text still
//! get different directories, a change to the sanitising rules cannot silently
//! merge two existing profiles, and an id that is mostly illegal characters is
//! still distinguishable.
//!
//! # The digest is written out rather than borrowed
//!
//! [`stable_digest`] is FNV-1a rather than `DefaultHasher`. `DefaultHasher` is
//! explicitly documented as unstable between Rust releases, so using it would
//! mean a toolchain change could re-point every account's profile directory —
//! silently orphaning every stored session. FNV-1a is four lines, is specified
//! rather than versioned, and does not change.

use std::path::{Path, PathBuf};

/// Where per-account browser profiles live inside the config directory.
pub const PROFILE_ROOT: &str = "browser-profiles";

/// Longest sanitised id segment kept before the digest is appended.
///
/// Bounded so a long user-supplied id cannot produce a path component that
/// exceeds what the filesystem accepts, on any platform.
const MAX_SEGMENT: usize = 48;

/// The profile directory for one account.
///
/// Pure, and therefore the whole isolation policy is in one testable function.
/// `config_dir` is the application's own directory; the profile is created under
/// it so removing the app's data removes the sessions with it.
pub fn profile_dir(config_dir: &Path, provider_id: &str, account_id: &str) -> PathBuf {
    let digest = stable_digest(&[provider_id, account_id]);
    config_dir.join(PROFILE_ROOT).join(format!(
        // `{:016x}` rather than `{:08x}`: the width in a hex format is a
        // *minimum*, so a digest above 32 bits renders more characters than
        // intended and the component's length then varies with the value. Sixteen
        // is the full width, so the name is exactly as long as it looks and no
        // distinctness is traded away.
        "{}--{}--{:016x}",
        sanitize(provider_id),
        sanitize(account_id),
        digest
    ))
}

/// The file Chrome and Edge write their DevTools port into.
///
/// Both use this name when `--remote-debugging-port=0` is given, which is how a
/// launched browser is told where its own debugging endpoint ended up without
/// Zroutery having to pick a free port and race for it.
pub const DEVTOOLS_PORT_FILE: &str = "DevToolsActivePort";

/// Where the launched browser reports its debugging endpoint.
pub fn devtools_port_file(config_dir: &Path, provider_id: &str, account_id: &str) -> PathBuf {
    profile_dir(config_dir, provider_id, account_id).join(DEVTOOLS_PORT_FILE)
}

/// Reduce an id to characters that are safe in a directory name on every
/// platform Zroutery ships.
///
/// Anything outside `[A-Za-z0-9_-]` becomes `_`, and an id that reduces to
/// nothing becomes `account` rather than an empty path segment. The digest in
/// [`profile_dir`] is what actually guarantees distinctness; this only has to
/// make the name usable and legible.
pub fn sanitize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_SEGMENT));
    for ch in raw.chars().take(MAX_SEGMENT) {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        // An all-illegal id would otherwise collapse to an empty segment, and an
        // empty segment is not a directory name.
        return "account".to_string();
    }
    trimmed.to_string()
}

/// A stable 64-bit FNV-1a digest of several strings.
///
/// Separated by a NUL byte so `("ab", "c")` and `("a", "bc")` cannot digest to
/// the same value — without that separator a two-id key is really a four-id key
/// with ambiguous boundaries.
pub fn stable_digest(parts: &[&str]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET;
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hash ^= 0;
            hash = hash.wrapping_mul(PRIME);
        }
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// Literal browser locations, for the platforms whose paths are fixed.
///
/// Windows is **not** in this list and cannot be: its install directories come
/// from environment variables (`ProgramFiles`, `LOCALAPPDATA`) and differ per
/// machine, so they are built at runtime by [`browser_candidates`]. Listing only
/// a bare `chrome.exe` here was a real bug — Windows does not put Chrome on
/// `PATH`, so a resolution that relied on it found nothing on the machine it was
/// written on.
pub const BROWSER_CANDIDATES: &[&str] = &[
    // macOS
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    // Linux
    "google-chrome",
    "google-chrome-stable",
    "microsoft-edge",
    "chromium",
    "chromium-browser",
    "brave-browser",
];

/// Every place a Chromium browser might be, in the order it will be tried.
///
/// Windows entries come first among the absolutes and per-user installs are
/// preferred: a machine with both a per-user and a machine-wide Chrome should use
/// the one the person using the app actually owns, because that is the profile
/// their own sessions live in.
pub fn browser_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = BROWSER_CANDIDATES.iter().map(PathBuf::from).collect();
    let mut windows: Vec<PathBuf> = Vec::new();

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        windows.push(local.join("Google/Chrome/Application/chrome.exe"));
        windows.push(local.join("Microsoft/Edge/Application/msedge.exe"));
    }
    // `ProgramFiles` and `ProgramFiles(x86)` both exist on a 64-bit Windows; a
    // 32-bit install has only the latter, and a missing one is simply skipped.
    for (key, dir) in [
        ("PROGRAMFILES", "Google/Chrome/Application/chrome.exe"),
        ("PROGRAMFILES(X86)", "Google/Chrome/Application/chrome.exe"),
    ] {
        if let Some(base) = std::env::var_os(key) {
            windows.push(PathBuf::from(base).join(dir));
        }
    }
    for (key, dir) in [
        ("PROGRAMFILES", "Microsoft/Edge/Application/msedge.exe"),
        ("PROGRAMFILES(X86)", "Microsoft/Edge/Application/msedge.exe"),
    ] {
        if let Some(base) = std::env::var_os(key) {
            windows.push(PathBuf::from(base).join(dir));
        }
    }

    // Windows paths first on Windows, and only on Windows: on macOS and Linux
    // these resolve to nothing and would just be checked in vain.
    if cfg!(windows) {
        let mut ordered = windows;
        ordered.append(&mut out);
        return ordered;
    }
    out
}

/// Whether a candidate is an absolute path rather than a bare command name.
///
/// Absolute candidates are checked directly so a browser in a non-standard
/// install location is found without a `PATH` search; bare names are resolved
/// through `PATH` by the caller.
pub fn is_absolute_candidate(candidate: &str) -> bool {
    candidate.contains('/') || candidate.contains('\\') || Path::new(candidate).is_absolute()
}

/// A Windows path can be absolute without either separator being `/`.
///
/// `Path::is_absolute` on Windows accepts a drive prefix, but the check here runs
/// the same way on every platform, so the drive letter is recognised explicitly
/// rather than relying on the host's own rules.
pub fn is_windows_style_absolute(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(provider: &str, account: &str) -> PathBuf {
        profile_dir(Path::new("/cfg"), provider, account)
    }

    #[test]
    fn a_profile_lives_under_the_config_directory() {
        let path = dir("relay", "main");
        assert!(path.starts_with("/cfg"));
        assert!(path.to_string_lossy().contains(PROFILE_ROOT));
    }

    #[test]
    fn two_accounts_on_one_provider_get_different_profiles() {
        assert_ne!(dir("relay", "a"), dir("relay", "b"));
    }

    #[test]
    fn the_same_account_id_on_two_providers_gets_different_profiles() {
        // This is the case that matters most: a relay named `main` and a different
        // relay also named `main` must not share a session.
        assert_ne!(dir("relay-a", "main"), dir("relay-b", "main"));
    }

    #[test]
    fn derivation_is_deterministic() {
        // A resume has to find the same directory the launch created.
        assert_eq!(dir("relay", "main"), dir("relay", "main"));
    }

    #[test]
    fn ids_that_sanitise_alike_still_get_different_profiles() {
        // `a/b` and `a_b` reduce to the same text; the digest is what keeps them
        // apart. Merging them would sign two providers in as each other.
        assert_eq!(sanitize("a/b"), sanitize("a_b"));
        assert_ne!(dir("p", "a/b"), dir("p", "a_b"));
    }

    #[test]
    fn a_path_traversal_cannot_escape_the_config_directory() {
        let escaped = dir("p", "../../../etc");
        assert!(escaped.starts_with("/cfg"));
        assert!(
            !escaped.to_string_lossy().contains(".."),
            "no parent-directory component may survive: {escaped:?}"
        );
    }

    #[test]
    fn a_separator_cannot_escape_the_config_directory() {
        let escaped = dir("a\\..\\..\\Windows", "main");
        assert!(escaped.starts_with("/cfg"));
    }

    #[test]
    fn an_id_with_no_usable_characters_still_yields_a_directory_name() {
        assert_eq!(sanitize("///"), "account");
        assert_eq!(sanitize(""), "account");
        assert_eq!(sanitize("   "), "account");
        // And the pair is still distinct.
        assert_ne!(dir("p", "///"), dir("p", "???"));
    }

    #[test]
    fn a_long_id_produces_a_bounded_path_component() {
        let long = "x".repeat(5_000);
        let path = dir("p", &long);
        let leaf = path.file_name().expect("a leaf").to_string_lossy();
        // `p`, two separators, a 48-character account segment, two separators, a
        // 16-digit digest. The point is that it is exactly bounded.
        assert_eq!(leaf.len(), 1 + 2 + MAX_SEGMENT + 2 + 16);
        assert!(leaf.ends_with(&format!("{:016x}", stable_digest(&["p", &long]))));
    }

    #[test]
    fn unicode_ids_are_sanitised_not_rejected() {
        // A non-ASCII id is legal configuration; it must still get a profile.
        let path = dir("relay", "主账户");
        assert!(path.starts_with("/cfg"));
        // The invariant is about the *name*, not the whole path: on Windows the
        // path separator is a backslash, so asserting on the joined string would
        // only be testing the platform.
        let leaf = path.file_name().expect("a leaf").to_string_lossy();
        assert!(!leaf.contains('\\') && !leaf.contains('/'), "leaf {leaf:?}");
        assert!(leaf.starts_with("relay--"));
    }

    #[test]
    fn the_digest_is_stable_across_calls() {
        // The whole point of not using `DefaultHasher`: this value pins a
        // directory on disk, so it must not move when the toolchain does.
        assert_eq!(
            stable_digest(&["relay", "main"]),
            stable_digest(&["relay", "main"])
        );
        assert_ne!(
            stable_digest(&["relay", "main"]),
            stable_digest(&["relay", "other"])
        );
    }

    #[test]
    fn the_digest_separates_its_parts() {
        // Without a separator `("ab","c")` and `("a","bc")` would be one key.
        assert_ne!(stable_digest(&["ab", "c"]), stable_digest(&["a", "bc"]));
    }

    #[test]
    fn the_digest_handles_an_empty_pair_without_collapsing() {
        assert_ne!(stable_digest(&["", ""]), stable_digest(&["", "x"]));
        assert_ne!(stable_digest(&["p", ""]), stable_digest(&["", "p"]));
    }

    #[test]
    fn the_devtools_port_file_lives_inside_the_profile() {
        let port_file = devtools_port_file(Path::new("/cfg"), "relay", "main");
        assert!(port_file.starts_with(dir("relay", "main")));
        assert_eq!(port_file.file_name().unwrap(), DEVTOOLS_PORT_FILE);
    }

    #[test]
    fn absolute_candidates_are_recognised() {
        assert!(is_absolute_candidate("/Applications/Google Chrome.app/..."));
        assert!(is_absolute_candidate(
            "C:\\Program Files\\Chrome\\chrome.exe"
        ));
        assert!(!is_absolute_candidate("chrome.exe"));
        assert!(!is_absolute_candidate("google-chrome"));
    }

    #[test]
    fn the_candidate_list_is_chromium_only() {
        for candidate in BROWSER_CANDIDATES {
            let leaf = candidate.rsplit('/').next().unwrap_or(candidate);
            assert!(
                leaf.contains("Chrome")
                    || leaf.contains("chrome")
                    || leaf.contains("Edge")
                    || leaf.contains("edge")
                    || leaf.contains("Chromium")
                    || leaf.contains("chromium")
                    || leaf.contains("Brave")
                    || leaf.contains("brave"),
                "{candidate} is not a Chromium browser; the DevTools endpoint this relies on \
                 is a Chromium facility"
            );
        }
    }

    #[test]
    fn the_runtime_candidates_include_the_platform_install_locations() {
        // The bug this pins: a list of bare `chrome.exe`/`msedge.exe` entries
        // resolves through `PATH`, and Windows does not put Chrome on `PATH`. So a
        // machine with Chrome installed would find nothing.
        let candidates: Vec<String> = browser_candidates()
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect();
        let joined = candidates.join(";");
        if cfg!(windows) {
            assert!(
                joined.contains("Chrome") && joined.contains("chrome.exe"),
                "the Windows install locations must be present: {candidates:?}"
            );
            assert!(
                joined.contains("Microsoft/Edge") || joined.contains(r"Microsoft\Edge"),
                "Edge's install location must be present too: {candidates:?}"
            );
        } else {
            assert!(
                !joined.contains("chrome.exe"),
                "Windows paths are meaningless elsewhere: {candidates:?}"
            );
        }
    }

    #[test]
    fn a_windows_style_path_is_recognised_on_every_platform() {
        // The check has to behave the same everywhere, so the drive prefix is
        // matched explicitly rather than deferred to the host's own rules.
        assert!(is_windows_style_absolute(
            r"C:\Program Files\Google\Chrome\chrome.exe"
        ));
        assert!(is_windows_style_absolute("D:/chrome.exe"));
        assert!(!is_windows_style_absolute("chrome.exe"));
        assert!(!is_windows_style_absolute("C:chrome.exe"));
        assert!(!is_windows_style_absolute(""));
    }
}
