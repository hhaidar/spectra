//! Settings remembered between runs, stored in `%APPDATA%\Spectra\settings.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{encoder::RecordSettings, source::SourceKind};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The last source shown.
    pub source: Option<SavedSource>,
    /// Show the preview 1:1 instead of fitted.
    pub native_zoom: bool,
    /// Upscale with FSR when the preview area is larger than the source.
    pub fsr: bool,
    pub sharpening: Sharpening,
    pub record: RecordSettings,
    /// Where recordings and screenshots go; `None` = Videos\Spectra.
    pub output_dir: Option<PathBuf>,
    /// Sidebar sections the user has expanded (all start collapsed).
    pub expanded: Vec<String>,
    /// Where the window was when Spectra last closed.
    pub window: Option<SavedWindow>,
}

/// The window's position and size (logical pixels), and whether it was maximised.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub maximized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            source: None,
            native_zoom: false,
            fsr: true,
            sharpening: Sharpening::Medium,
            record: RecordSettings::default(),
            output_dir: None,
            expanded: Vec::new(),
            window: None,
        }
    }
}

/// Sources are matched by stable properties (handles change between runs).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSource {
    pub kind: SourceKind,
    pub name: String,
    /// DirectShow path for capture cards and cameras.
    pub device_path: Option<String>,
    /// Device mode label; `None` means Auto.
    pub mode: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sharpening {
    Off,
    Low,
    Medium,
    High,
}

impl Sharpening {
    pub const ALL: [Sharpening; 4] = [Sharpening::Off, Sharpening::Low, Sharpening::Medium, Sharpening::High];

    pub fn label(self) -> &'static str {
        match self {
            Sharpening::Off => "Off",
            Sharpening::Low => "Low",
            Sharpening::Medium => "Medium",
            Sharpening::High => "High",
        }
    }

    /// RCAS strength in stops (0 = strongest); `None` skips RCAS.
    pub fn stops(self) -> Option<f32> {
        match self {
            Sharpening::Off => None,
            Sharpening::Low => Some(1.0),
            Sharpening::Medium => Some(0.25),
            Sharpening::High => Some(0.0),
        }
    }
}

impl Settings {
    pub fn is_expanded(&self, section: &str) -> bool {
        self.expanded.iter().any(|s| s == section)
    }

    pub fn toggle_expanded(&mut self, section: &str) {
        if self.is_expanded(section) {
            self.expanded.retain(|s| s != section);
        } else {
            self.expanded.push(section.to_string());
        }
    }

    pub fn output_dir(&self) -> PathBuf {
        self.output_dir.clone().unwrap_or_else(default_output_dir)
    }
}

fn default_output_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|home| PathBuf::from(home).join("Videos").join("Spectra"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("Spectra").join("settings.json"))
}

pub fn load() -> Settings {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let Some(path) = path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        // Write then rename so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}
