//! Native system accent for theme skins. Missing native data stays absent so
//! the page can keep its own fallback instead of claiming a guessed OS color.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SystemAccent {
    pub accent: String,
    pub accent_soft: String,
    pub accent_fg: String,
}

impl SystemAccent {
    fn from_rgb([r, g, b]: [u8; 3]) -> Self {
        // Use the stronger black/white contrast. YIQ's fixed threshold picks
        // white on some saturated colors where it fails readable contrast.
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let luminance = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
        let black_contrast = (luminance + 0.05) / 0.05;
        let white_contrast = 1.05 / (luminance + 0.05);
        Self {
            accent: format!("#{r:02x}{g:02x}{b:02x}"),
            accent_soft: format!("rgba({r}, {g}, {b}, 0.16)"),
            accent_fg: if black_contrast > white_contrast {
                "#000000"
            } else {
                "#ffffff"
            }
            .into(),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn current() -> Option<SystemAccent> {
    let mut argb = 0;
    let mut opaque = 0;
    // DWM returns AARRGGBB (not COLORREF's BGR). Both pointers are owned and
    // valid for the duration of the read-only system call.
    let result = unsafe {
        windows_sys::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut argb, &mut opaque)
    };
    (result >= 0)
        .then(|| SystemAccent::from_rgb([(argb >> 16) as u8, (argb >> 8) as u8, argb as u8]))
}

#[cfg(target_os = "macos")]
pub fn current() -> Option<SystemAccent> {
    use objc2_app_kit::{NSColor, NSColorSpace};
    let color =
        NSColor::controlAccentColor().colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let channels = [
        color.redComponent(),
        color.greenComponent(),
        color.blueComponent(),
    ];
    if channels.iter().any(|channel| !channel.is_finite()) {
        return None;
    }
    Some(SystemAccent::from_rgb(channels.map(|channel| {
        (channel.clamp(0.0, 1.0) * 255.0).round() as u8
    })))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn current() -> Option<SystemAccent> {
    linux::current().map(SystemAccent::from_rgb)
}

#[cfg(any(test, not(any(target_os = "windows", target_os = "macos"))))]
mod linux {
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub(super) fn current() -> Option<[u8; 3]> {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".config"))
            });
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if let Some(path) = config
            .filter(|_| desktop.contains("kde") || desktop.is_empty())
            .map(|root| root.join("kdeglobals"))
        {
            if let Ok(file) = std::fs::File::open(path) {
                let mut text = String::new();
                if file.take(1024 * 1024).read_to_string(&mut text).is_ok() {
                    if let Some(color) = kde_accent(&text) {
                        return Some(color);
                    }
                }
            }
        }
        let mut child = Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "accent-color"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                if !status.success() {
                    return None;
                }
                let mut text = String::new();
                child
                    .stdout
                    .take()?
                    .take(128)
                    .read_to_string(&mut text)
                    .ok()?;
                return gnome_accent(&text);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn kde_accent(text: &str) -> Option<[u8; 3]> {
        let mut section = "";
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line;
                continue;
            }
            if section != "[General]" {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() != "AccentColor" {
                continue;
            }
            let values: Vec<_> = value
                .trim()
                .split(',')
                .map(|value| value.trim().parse::<u8>())
                .collect::<Result<_, _>>()
                .ok()?;
            return values.try_into().ok();
        }
        None
    }

    pub(super) fn gnome_accent(text: &str) -> Option<[u8; 3]> {
        // GNOME 47+ named accent palette, not arbitrary CSS from a shell.
        Some(match text.trim().trim_matches('\'') {
            "blue" => [53, 132, 228],
            "teal" => [33, 144, 164],
            "green" => [58, 148, 74],
            "yellow" => [200, 136, 0],
            "orange" => [237, 91, 0],
            "red" => [230, 45, 66],
            "pink" => [213, 97, 153],
            "purple" => [145, 65, 172],
            "slate" => [111, 131, 150],
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_accent_uses_the_readable_black_or_white() {
        assert_eq!(SystemAccent::from_rgb([0, 0, 0]).accent_fg, "#ffffff");
        assert_eq!(SystemAccent::from_rgb([255, 255, 255]).accent_fg, "#000000");
        assert_eq!(SystemAccent::from_rgb([0, 120, 212]).accent, "#0078d4");
        assert_eq!(SystemAccent::from_rgb([255, 0, 0]).accent_fg, "#000000");
        assert_eq!(
            SystemAccent::from_rgb([0, 120, 212]).accent_soft,
            "rgba(0, 120, 212, 0.16)"
        );
    }

    #[test]
    fn linux_sources_validate_values_and_missing_remains_missing() {
        assert_eq!(
            linux::kde_accent("[General]\nAccentColor=12,34,56\n"),
            Some([12, 34, 56])
        );
        assert_eq!(linux::kde_accent("[Other]\nAccentColor=12,34,56\n"), None);
        assert_eq!(linux::kde_accent("[General]\nAccentColor=256,2,3\n"), None);
        assert_eq!(linux::gnome_accent("'blue'\n"), Some([53, 132, 228]));
        assert_eq!(linux::gnome_accent("unknown"), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn native_windows_accent_is_valid_when_dwm_is_available() {
        if let Some(accent) = current() {
            assert_eq!(accent.accent.len(), 7);
            assert!(u32::from_str_radix(&accent.accent[1..], 16).is_ok());
            assert!(matches!(accent.accent_fg.as_str(), "#000000" | "#ffffff"));
        }
    }
}
