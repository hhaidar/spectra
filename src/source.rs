//! Video sources: desktops (monitors), windows/tabs, capture cards and cameras.
//!
//! Desktops and windows are listed with `xcap` and captured with Windows Graphics
//! Capture (see `wgc.rs`). Capture cards and cameras are read through an
//! `ffmpeg` DirectShow child process that streams raw RGBA frames.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    process::{Child, Command, Stdio},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use xcap::{Monitor, Window};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Display,
    Window,
    CaptureCard,
    Camera,
}

impl SourceKind {
    pub const ALL: [SourceKind; 4] =
        [SourceKind::Display, SourceKind::Window, SourceKind::CaptureCard, SourceKind::Camera];

    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Display => "Desktop",
            SourceKind::Window => "Window / Tab",
            SourceKind::CaptureCard => "Capture card",
            SourceKind::Camera => "Camera",
        }
    }

    pub fn empty_text(self) -> &'static str {
        match self {
            SourceKind::Display => "No displays found",
            SourceKind::Window => "No capturable windows. Restore minimized windows and refresh.",
            SourceKind::CaptureCard => "No capture cards detected",
            SourceKind::Camera => "No cameras detected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceId {
    Display(u32),
    Window(u32),
    /// A DirectShow video device; `path` is what's passed to `-i video=…`.
    Device {
        path: String,
    },
}

#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub id: SourceId,
    pub name: String,
    pub detail: String,
    pub primary: bool,
}

/// Enumerates every source. Failures for one category don't hide the others.
pub fn list_sources() -> (Vec<SourceInfo>, Vec<String>) {
    let mut sources = Vec::new();
    let mut errors = Vec::new();
    type Lister = fn() -> Result<Vec<SourceInfo>>;
    let lists: [(&str, Lister); 3] =
        [("displays", list_displays), ("windows", list_windows), ("video devices", list_devices)];
    for (what, list) in lists {
        match list() {
            Ok(list) => sources.extend(list),
            Err(e) => errors.push(format!("Couldn't list {what}: {e:#}")),
        }
    }
    (sources, errors)
}

fn list_displays() -> Result<Vec<SourceInfo>> {
    let mut out = Monitor::all()?
        .iter()
        .filter_map(|m| {
            Some(SourceInfo {
                kind: SourceKind::Display,
                id: SourceId::Display(m.id().ok()?),
                name: m.friendly_name().or_else(|_| m.name()).ok()?,
                detail: format!("{}×{}", m.width().ok()?, m.height().ok()?),
                primary: m.is_primary().unwrap_or(false),
            })
        })
        .collect::<Vec<_>>();
    out.sort_by_key(|s| !s.primary);
    Ok(out)
}

fn list_windows() -> Result<Vec<SourceInfo>> {
    let own_pid = std::process::id();
    let mut out = Vec::new();
    for w in Window::all()? {
        let (Ok(id), Ok(title)) = (w.id(), w.title()) else {
            continue;
        };
        let (width, height) = (w.width().unwrap_or(0), w.height().unwrap_or(0));
        if title.trim().is_empty()
            || title == "Program Manager"
            || w.pid().ok() == Some(own_pid)
            || w.is_minimized().unwrap_or(true)
            || width < 100
            || height < 60
        {
            continue;
        }
        let app = w.app_name().unwrap_or_default();
        out.push(SourceInfo {
            kind: SourceKind::Window,
            id: SourceId::Window(id),
            name: title,
            detail: format!("{app} · {width}×{height}"),
            primary: false,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// DirectShow devices (capture cards & cameras) via ffmpeg
// ---------------------------------------------------------------------------

pub(crate) fn ffmpeg_command() -> Command {
    let mut cmd = Command::new("ffmpeg");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.stdin(Stdio::null());
    cmd
}

pub fn ffmpeg_available() -> bool {
    ffmpeg_command().arg("-version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Strips ffmpeg's `[dshow @ 0x…] ` log prefix.
fn log_body(line: &str) -> &str {
    match line.trim_start().strip_prefix('[') {
        Some(rest) => rest.split_once("] ").map_or(line, |(_, body)| body),
        None => line,
    }
}

fn list_devices() -> Result<Vec<SourceInfo>> {
    if !cfg!(windows) {
        return Ok(vec![]);
    }
    let output = ffmpeg_command()
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .output()
        .context("failed to run ffmpeg")?;
    let devices = parse_device_list(&String::from_utf8_lossy(&output.stderr));
    Ok(devices
        .into_iter()
        .map(|(name, path)| {
            let kind = classify_device(&name, &path);
            let detail = if path.starts_with("@device_sw_") { "Virtual device" } else { "USB device" };
            SourceInfo { kind, id: SourceId::Device { path }, name, detail: detail.into(), primary: false }
        })
        .collect())
}

/// Returns `(friendly name, path)` for each video device in `-list_devices` output.
fn parse_device_list(text: &str) -> Vec<(String, String)> {
    let mut devices = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        let body = log_body(line).trim();
        if let Some(alt) = body.strip_prefix("Alternative name ") {
            if let Some(name) = pending.take() {
                devices.push((name, alt.trim().trim_matches('"').to_string()));
            }
            continue;
        }
        // Any other line ends the previous device entry.
        if let Some(name) = pending.take() {
            devices.push((name.clone(), name));
        }
        if let Some(quoted) = body.strip_suffix("(video)") {
            pending = Some(quoted.trim().trim_matches('"').to_string());
        }
    }
    if let Some(name) = pending {
        devices.push((name.clone(), name));
    }
    devices
}

/// DirectShow doesn't tell cameras and capture cards apart, so use the USB
/// vendor id and well-known product names.
fn classify_device(name: &str, path: &str) -> SourceKind {
    const CAPTURE_VENDORS: [&str; 5] = [
        "vid_534d", // MacroSilicon (most HDMI→USB dongles)
        "vid_345f", // MacroSilicon MS2130 variants
        "vid_07ca", // AVerMedia
        "vid_2935", // Magewell
        "vid_1edb", // Blackmagic
    ];
    const CAPTURE_NAMES: [&str; 14] = [
        "capture card",
        "game capture",
        "video capture",
        "cam link",
        "hd60",
        "4k60",
        "live gamer",
        "avermedia",
        "magewell",
        "decklink",
        "blackmagic",
        "hdmi",
        "usb video",
        "usb3 video",
    ];
    let (name, path) = (name.to_lowercase(), path.to_lowercase());
    let vendor_match = path.starts_with("@device_pnp_") && CAPTURE_VENDORS.iter().any(|vid| path.contains(vid));
    if vendor_match || CAPTURE_NAMES.iter().any(|k| name.contains(k)) {
        SourceKind::CaptureCard
    } else {
        SourceKind::Camera
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceMode {
    pub width: u32,
    pub height: u32,
    /// Frame rate exactly as ffmpeg printed it (DirectShow wants an exact match).
    fps: String,
    pub fps_value: f64,
    /// `("-vcodec", "mjpeg")` or `("-pixel_format", "yuyv422")`.
    format: (&'static str, String),
}

impl DeviceMode {
    pub fn label(&self) -> String {
        let format = if self.format.0 == "-vcodec" { self.format.1.to_uppercase() } else { "RAW".into() };
        format!("{}×{} @ {} · {format}", self.width, self.height, self.fps_value.round())
    }

    /// Ranking used for "Auto": ≥30 fps first, then the largest size up to
    /// 1080p (bigger frames are costly to pipe around), then higher fps, then
    /// uncompressed formats.
    fn auto_rank(&self) -> (bool, bool, u32, u32, bool) {
        let area = self.width * self.height;
        (
            self.fps_value >= 29.0,
            area <= 1920 * 1080,
            area,
            (self.fps_value.min(60.0) * 100.0) as u32,
            self.format.0 == "-pixel_format",
        )
    }
}

/// Parses `-list_options` output into distinct modes, largest/fastest first.
fn parse_modes(text: &str) -> Vec<DeviceMode> {
    let mut modes: Vec<DeviceMode> = Vec::new();
    for line in text.lines() {
        let mut tokens = log_body(line).split_whitespace();
        let Some(first) = tokens.next() else { continue };
        let format = if let Some(codec) = first.strip_prefix("vcodec=") {
            if codec != "mjpeg" {
                continue; // ffmpeg can't decode e.g. h264 dshow streams reliably
            }
            ("-vcodec", codec.to_string())
        } else if let Some(pix) = first.strip_prefix("pixel_format=") {
            ("-pixel_format", pix.to_string())
        } else {
            continue;
        };
        // Use the "max s=WxH fps=N" part.
        let mut after_max = tokens.skip_while(|t| *t != "max").skip(1);
        let (Some(size), Some(fps)) = (after_max.next(), after_max.next()) else {
            continue;
        };
        let (Some(size), Some(fps)) = (size.strip_prefix("s="), fps.strip_prefix("fps=")) else {
            continue;
        };
        let Some((width, height)) = parse_size(size) else { continue };
        let Ok(fps_value) = fps.parse::<f64>() else { continue };
        let mode = DeviceMode { width, height, fps: fps.to_string(), fps_value, format };
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    modes.sort_by(|a, b| {
        (b.width * b.height, b.fps_value as u32, b.format.0 == "-pixel_format").cmp(&(
            a.width * a.height,
            a.fps_value as u32,
            a.format.0 == "-pixel_format",
        ))
    });
    modes
}

pub fn best_mode(modes: &[DeviceMode]) -> Option<&DeviceMode> {
    modes.iter().max_by_key(|m| m.auto_rank())
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// Finds the `WxH` in an ffmpeg `Stream #0:0: Video: …` line.
fn parse_stream_size(line: &str) -> Option<(u32, u32)> {
    line.split([',', ' ']).filter(|t| !t.starts_with("0x")).find_map(parse_size)
}

pub fn probe_device_modes(path: &str) -> Vec<DeviceMode> {
    ffmpeg_command()
        .args(["-hide_banner", "-f", "dshow", "-list_options", "true", "-i"])
        .arg(format!("video={path}"))
        .output()
        .map(|o| parse_modes(&String::from_utf8_lossy(&o.stderr)))
        .unwrap_or_default()
}

type Tail = Arc<Mutex<VecDeque<String>>>;

fn tail_text(tail: &Tail) -> String {
    let tail = tail.lock().unwrap();
    if tail.iter().any(|l| l.contains("already in use") || l.contains("Could not run graph")) {
        return "the device is busy. Close other apps using it (Camera, OBS, Teams, a browser tab…) and select it again"
            .into();
    }
    // Prefer ffmpeg's most specific complaint over its generic final error.
    tail.iter()
        .find(|l| l.contains("Could not") || l.contains("not supported"))
        .or_else(|| tail.iter().rev().find(|l| l.to_lowercase().contains("error")))
        .or(tail.back())
        .map(|l| log_body(l).trim().to_string())
        .unwrap_or_default()
}

/// A frame from a capture card / camera.
pub struct DeviceFrame {
    pub rgba: Vec<u8>,
    pub size: (u32, u32),
    /// When the last byte of the frame arrived from ffmpeg.
    pub received: Instant,
}

type Latest = Arc<(Mutex<Option<DeviceFrame>>, Condvar)>;

/// A capture card or camera, decoded by an ffmpeg child process.
pub struct DeviceSource {
    child: Child,
    latest: Latest,
    tail: Tail,
}

impl DeviceSource {
    /// Opens the device. A device that was just released (e.g. by switching
    /// modes) can take a moment to become available again, so retry briefly.
    pub fn open(path: &str, mode: Option<&DeviceMode>) -> Result<Self> {
        let mut attempt = 0;
        loop {
            match Self::try_open(path, mode) {
                Ok(device) => return Ok(device),
                Err(e) if attempt >= 2 => return Err(e),
                Err(_) => {
                    attempt += 1;
                    thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    fn try_open(path: &str, mode: Option<&DeviceMode>) -> Result<Self> {
        let mut cmd = ffmpeg_command();
        cmd.args(["-hide_banner", "-nostats", "-loglevel", "info"])
            // Low latency: don't buffer or analyse input before starting, and
            // hand over every frame as soon as it is decoded.
            .args(["-fflags", "nobuffer", "-flags", "low_delay"])
            .args(["-probesize", "32", "-analyzeduration", "0"])
            .args(["-f", "dshow", "-rtbufsize", "64M"]);
        if let Some(mode) = mode {
            cmd.args(["-video_size", &format!("{}x{}", mode.width, mode.height)])
                .args(["-framerate", &mode.fps])
                .args([mode.format.0, &mode.format.1]);
        }
        cmd.arg("-i")
            .arg(format!("video={path}"))
            // Accurate YUV → RGB: honour the device's signalled matrix and range,
            // with full-precision chroma and rounding.
            .args([
                "-vf",
                "scale=in_color_matrix=auto:in_range=auto:out_range=full\
                 :flags=accurate_rnd+full_chroma_int+full_chroma_inp,format=rgba",
            ])
            .args(["-an", "-fps_mode", "passthrough", "-flush_packets", "1"])
            .args(["-f", "rawvideo", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().context("failed to start ffmpeg")?;

        // Read ffmpeg's log to learn the output frame size and keep recent lines
        // around for error messages.
        let tail: Tail = Default::default();
        let (size_tx, size_rx) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        {
            let tail = tail.clone();
            thread::spawn(move || {
                let mut in_output = false;
                let mut size_tx = Some(size_tx);
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if line.starts_with("Output #0") {
                        in_output = true;
                    } else if in_output
                        && line.contains("Video:")
                        && let (Some(tx), Some(size)) = (size_tx.take(), parse_stream_size(&line))
                    {
                        let _ = tx.send(size);
                    }
                    let mut tail = tail.lock().unwrap();
                    if tail.len() >= 30 {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            });
        }

        let size = match size_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(size) => size,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let reason = tail_text(&tail);
                bail!("couldn't open device{}", if reason.is_empty() { String::new() } else { format!(": {reason}") });
            }
        };

        let latest: Latest = Default::default();
        let mut stdout = child.stdout.take().unwrap();
        {
            let latest = latest.clone();
            thread::spawn(move || {
                let len = (size.0 * size.1 * 4) as usize;
                loop {
                    let mut rgba = vec![0u8; len];
                    if stdout.read_exact(&mut rgba).is_err() {
                        break;
                    }
                    // Replace (never queue) so the consumer always gets the newest frame.
                    *latest.0.lock().unwrap() = Some(DeviceFrame { rgba, size, received: Instant::now() });
                    latest.1.notify_one();
                }
                latest.1.notify_one();
            });
        }

        Ok(Self { child, latest, tail })
    }

    /// Waits up to `timeout` for the next frame.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<DeviceFrame>> {
        {
            let (lock, cvar) = &*self.latest;
            let mut latest = lock.lock().unwrap();
            if latest.is_none() {
                latest = cvar.wait_timeout(latest, timeout).unwrap().0;
            }
            if let Some(frame) = latest.take() {
                return Ok(Some(frame));
            }
        }
        if let Some(status) = self.child.try_wait()? {
            let reason = tail_text(&self.tail);
            bail!("device stopped ({status}){}", if reason.is_empty() { String::new() } else { format!(": {reason}") });
        }
        Ok(None)
    }
}

impl Drop for DeviceSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = r#"[in#0 @ 0000] "USB Video" (video)
[in#0 @ 0000]   Alternative name "@device_pnp_\\?\usb#vid_534d&pid_2109&mi_00#9&3433&1&0000#{65e8}\global"
[in#0 @ 0000] "Logitech BRIO" (video)
[in#0 @ 0000]   Alternative name "@device_pnp_\\?\usb#vid_046d&pid_085e&mi_00#8&729c&0&0000#{65e8}\global"
I2026-11-22 18:58:10.953043 (64040) [INFO] [VCAMDS] ffmpeg.exe
[in#0 @ 0000] "Logi Capture" (video)
[in#0 @ 0000]   Alternative name "@device_sw_{860BB310}\{4A2FEA90}"
[in#0 @ 0000] "Microphone (Realtek Audio USB)" (audio)
[in#0 @ 0000]   Alternative name "@device_cm_{33D9A762}\wave_{7263C0C7}"
Error opening input file dummy."#;

    #[test]
    fn parses_and_classifies_devices() {
        let devices = parse_device_list(DEVICES);
        let names: Vec<_> = devices.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["USB Video", "Logitech BRIO", "Logi Capture"]);
        assert!(devices[0].1.starts_with("@device_pnp_"));
        let kinds: Vec<_> = devices.iter().map(|(n, p)| classify_device(n, p)).collect();
        assert_eq!(kinds, [SourceKind::CaptureCard, SourceKind::Camera, SourceKind::Camera]);
    }

    #[test]
    fn picks_best_mode() {
        let options = r#"[in#0 @ 0000]   vcodec=mjpeg  min s=1920x1080 fps=10 max s=1920x1080 fps=60.0002
[in#0 @ 0000]   vcodec=mjpeg  min s=3840x2160 fps=10 max s=3840x2160 fps=30
[in#0 @ 0000]   vcodec=mjpeg  min s=1280x720 fps=10 max s=1280x720 fps=60.0002 (pc, bt470bg/bt709/unknown, center)
[in#0 @ 0000]   pixel_format=yuyv422  min s=1920x1080 fps=5 max s=1920x1080 fps=5
[in#0 @ 0000]   pixel_format=yuyv422  min s=640x480 fps=5 max s=640x480 fps=30"#;
        let modes = parse_modes(options);
        assert_eq!(modes.len(), 5);
        assert_eq!((modes[0].width, modes[0].height), (3840, 2160)); // sorted largest first
        let mode = best_mode(&modes).unwrap();
        assert_eq!((mode.width, mode.height), (1920, 1080));
        assert_eq!(mode.fps, "60.0002");
        assert_eq!(mode.format, ("-vcodec", "mjpeg".to_string()));
    }

    #[test]
    fn parses_stream_size() {
        let line = "  Stream #0:0: Video: rawvideo (RGBA / 0x41424752), rgba(pc, gbr/unknown/unknown, progressive), 1920x1080 [SAR 1:1 DAR 16:9], q=2-31, 30 fps";
        assert_eq!(parse_stream_size(line), Some((1920, 1080)));
    }
}
