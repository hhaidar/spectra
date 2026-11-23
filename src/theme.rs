//! Black / gray palette with a purple accent.

use gpui::{Hsla, rgb};

pub const BG: u32 = 0x09090b;
pub const SURFACE: u32 = 0x111113;
pub const ELEVATED: u32 = 0x19191c;
pub const HOVER: u32 = 0x232327;
pub const BORDER: u32 = 0x232327;
pub const BORDER_STRONG: u32 = 0x34343a;
pub const TEXT: u32 = 0xededef;
pub const MUTED: u32 = 0x8b8b94;
pub const SUBTLE: u32 = 0x5d5d66;
pub const ERROR: u32 = 0xe5484d;
pub const RECORD: u32 = 0xe5383b;
pub const SUCCESS: u32 = 0x46a758;
pub const WARNING: u32 = 0xe5a83b;
pub const ACCENT: u32 = 0xa855f7; // purple

/// Font for icon glyphs: Windows 11's Fluent icon font, or the system font
/// (with Unicode symbols) elsewhere.
#[cfg(windows)]
pub const ICONS: &str = "Segoe Fluent Icons";
#[cfg(not(windows))]
pub const ICONS: &str = ".SystemUIFont";

/// Monospaced font for timers.
#[cfg(windows)]
pub const MONO: &str = "Consolas";
#[cfg(target_os = "macos")]
pub const MONO: &str = "Menlo";
#[cfg(all(not(windows), not(target_os = "macos")))]
pub const MONO: &str = "DejaVu Sans Mono";

#[cfg(not(windows))]
#[allow(dead_code)] // the window buttons are native outside Windows
pub mod icon {
    pub const CHEVRON_DOWN: &str = "▾";
    pub const CHEVRON_RIGHT: &str = "▸";
    pub const CAMERA: &str = "📷";
    pub const FOLDER: &str = "📁";
    pub const OPEN: &str = "↗";
    pub const CHECK: &str = "✓";
    pub const REFRESH: &str = "↻";
    pub const VIDEO: &str = "▶";
    pub const WARNING: &str = "⚠";
    pub const MINIMIZE: &str = "—";
    pub const MAXIMIZE: &str = "□";
    pub const RESTORE: &str = "❐";
    pub const CLOSE: &str = "✕";
}

#[cfg(windows)]
pub mod icon {
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CAMERA: &str = "\u{E722}";
    pub const FOLDER: &str = "\u{E8B7}";
    pub const OPEN: &str = "\u{E8A7}";
    pub const CHECK: &str = "\u{E73E}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const VIDEO: &str = "\u{E714}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const MINIMIZE: &str = "\u{E921}";
    pub const MAXIMIZE: &str = "\u{E922}";
    pub const RESTORE: &str = "\u{E923}";
    pub const CLOSE: &str = "\u{E8BB}";
}

pub fn accent() -> Hsla {
    rgb(ACCENT).into()
}

/// Translucent accent for selected backgrounds.
pub fn accent_soft() -> Hsla {
    accent().opacity(0.16)
}

pub fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}
