//! Recording settings and the `ffmpeg` encoder process.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, Stdio},
    thread::{self, JoinHandle},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::source::ffmpeg_command;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 3] = [Codec::H264, Codec::Hevc, Codec::Av1];

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC",
            Codec::Av1 => "AV1",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Codec::H264 => "Plays everywhere",
            Codec::Hevc => "Smaller files",
            Codec::Av1 => "Smallest files, newest",
        }
    }

    fn software_encoder(self) -> &'static str {
        match self {
            Codec::H264 => "libx264",
            Codec::Hevc => "libx265",
            Codec::Av1 => "libsvtav1",
        }
    }

    fn hardware_encoders(self) -> [&'static str; 3] {
        match self {
            Codec::H264 => ["h264_nvenc", "h264_amf", "h264_qsv"],
            Codec::Hevc => ["hevc_nvenc", "hevc_amf", "hevc_qsv"],
            Codec::Av1 => ["av1_nvenc", "av1_amf", "av1_qsv"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    High,
    Balanced,
    Small,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Balanced, Quality::Small];

    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High",
            Quality::Balanced => "Balanced",
            Quality::Small => "Small file",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Quality::High => "Best quality, larger files",
            Quality::Balanced => "Recommended",
            Quality::Small => "Smaller files, softer image",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordSettings {
    pub fps: u32,
    pub codec: Codec,
    pub quality: Quality,
    /// Prefer the GPU encoder when one is available.
    pub hardware: bool,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self { fps: 60, codec: Codec::Hevc, quality: Quality::Balanced, hardware: true }
    }
}

/// What the local ffmpeg + GPU can actually do.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub encoders: Vec<&'static str>,
}

impl Capabilities {
    /// Probes by encoding a single tiny frame with each candidate (an encoder
    /// can be compiled into ffmpeg but have no matching GPU).
    pub fn detect() -> Self {
        let handles: Vec<_> = Codec::ALL
            .iter()
            .flat_map(|c| c.hardware_encoders().into_iter().chain([c.software_encoder()]))
            .map(|enc| {
                thread::spawn(move || {
                    ffmpeg_command()
                        .args(["-hide_banner", "-loglevel", "error"])
                        .args(["-f", "lavfi", "-i", "color=s=256x256", "-frames:v", "1"])
                        .args(["-c:v", enc, "-f", "null", "-"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
                        .then_some(enc)
                })
            })
            .collect();
        Self { encoders: handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect() }
    }

    pub fn hardware_encoder(&self, codec: Codec) -> Option<&'static str> {
        codec.hardware_encoders().into_iter().find(|e| self.encoders.contains(e))
    }

    pub fn has_codec(&self, codec: Codec) -> bool {
        self.hardware_encoder(codec).is_some() || self.encoders.contains(&codec.software_encoder())
    }

    /// Picks the concrete ffmpeg encoder for these settings and frame size.
    pub fn encoder_for(&self, settings: &RecordSettings, size: (u32, u32)) -> Option<&'static str> {
        let hw = self.hardware_encoder(settings.codec).filter(|enc| {
            // GPU H.264 encoders top out at 4096 px per side.
            !enc.starts_with("h264") || (size.0 <= 4096 && size.1 <= 4096)
        });
        let sw = Some(settings.codec.software_encoder()).filter(|e| self.encoders.contains(e));
        if settings.hardware { hw.or(sw) } else { sw.or(hw) }
    }
}

fn quality_args(encoder: &str, quality: Quality) -> Vec<String> {
    let q = |high: u32, balanced: u32, small: u32| {
        match quality {
            Quality::High => high,
            Quality::Balanced => balanced,
            Quality::Small => small,
        }
        .to_string()
    };
    let args: &[&str] = match encoder {
        "libx264" => &["-preset", "veryfast", "-crf"],
        "libx265" => &["-preset", "fast", "-crf"],
        "libsvtav1" => &["-preset", "8", "-crf"],
        e if e.ends_with("_nvenc") => &["-preset", "p4", "-tune", "hq", "-rc", "vbr", "-b:v", "0", "-cq"],
        e if e.ends_with("_amf") => &["-rc", "cqp", "-qp_p"],
        e if e.ends_with("_qsv") => &["-global_quality"],
        _ => &[],
    };
    let value = match encoder {
        "libx264" => q(18, 23, 28),
        "libx265" => q(20, 26, 31),
        "libsvtav1" => q(26, 34, 42),
        e if e.ends_with("_nvenc") => q(19, 25, 31),
        e if e.ends_with("_amf") => q(20, 26, 32),
        _ => q(20, 25, 31),
    };
    args.iter().map(|s| s.to_string()).chain([value]).collect()
}

/// Byte order of raw frames fed to the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder {
    Rgba,
    /// What the Windows GPU path reads back.
    #[cfg_attr(not(windows), allow(dead_code))]
    Bgra,
}

pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    stderr: Option<JoinHandle<String>>,
    pub name: &'static str,
}

impl Encoder {
    pub fn spawn(
        size: (u32, u32),
        order: PixelOrder,
        settings: &RecordSettings,
        caps: &Capabilities,
        path: &Path,
    ) -> Result<Self> {
        let Some(encoder) = caps.encoder_for(settings, size) else {
            bail!("no {} encoder is available in ffmpeg", settings.codec.label());
        };

        let mut cmd = ffmpeg_command();
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", if order == PixelOrder::Bgra { "bgra" } else { "rgba" }])
            .args(["-s", &format!("{}x{}", size.0, size.1)])
            .args(["-framerate", &settings.fps.to_string()])
            .args(["-i", "-"])
            // sRGB → BT.709 limited-range YUV 4:2:0 (even size), tagged so players
            // show the colours exactly as captured.
            .args([
                "-vf",
                "scale=trunc(iw/2)*2:trunc(ih/2)*2:out_color_matrix=bt709:out_range=tv\
                 :flags=bicubic+accurate_rnd+full_chroma_int,format=yuv420p,\
                 setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv",
            ])
            .args(["-c:v", encoder])
            .args(quality_args(encoder, settings.quality))
            .args(["-g", &(settings.fps * 2).to_string()]);
        if settings.codec == Codec::Hevc {
            cmd.args(["-tag:v", "hvc1"]); // plays in QuickTime / Apple apps
        }
        cmd.args(["-movflags", "+faststart"])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().context("failed to start ffmpeg")?;
        let stdin = child.stdin.take();
        let mut stderr_pipe = child.stderr.take().unwrap();
        let stderr = thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr_pipe.read_to_string(&mut s);
            s
        });
        Ok(Self { child, stdin, stderr: Some(stderr), name: encoder })
    }

    pub fn write_frame(&mut self, data: &[u8]) -> Result<()> {
        let Some(stdin) = self.stdin.as_mut() else { bail!("encoder stopped") };
        if let Err(e) = stdin.write_all(data) {
            // ffmpeg died: report its own error message if it left one.
            drop(self.stdin.take());
            let _ = self.child.wait();
            let log = self.stderr.take().map(|h| h.join().unwrap_or_default()).unwrap_or_default();
            bail!("encoder stopped: {}", if log.trim().is_empty() { e.to_string() } else { log.trim().to_string() });
        }
        Ok(())
    }

    /// Closes the input and waits for ffmpeg to finalize the file.
    pub fn finish(mut self) -> Result<()> {
        drop(self.stdin.take());
        let status = self.child.wait()?;
        let log = self.stderr.take().map(|h| h.join().unwrap_or_default());
        if !status.success() {
            bail!("ffmpeg exited with {status}: {}", log.unwrap_or_default().trim());
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            let _ = self.child.kill();
        }
    }
}
