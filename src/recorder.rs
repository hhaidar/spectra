//! Recording: a writer thread with its own clock feeds the newest frame to the
//! encoder at a constant frame rate. When the source sends no new frame (an
//! idle desktop), the last one is repeated, so the video's timing matches real
//! time unless the encoder can't keep up.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};

use crate::encoder::{Capabilities, Encoder, PixelOrder, RecordSettings};

/// A full-resolution frame in CPU memory.
pub struct RawFrame {
    pub data: Vec<u8>,
    pub size: (u32, u32),
    pub order: PixelOrder,
}

#[derive(Clone, Copy)]
pub struct RecordingInfo {
    pub started: Instant,
    pub fps: u32,
    pub size: (u32, u32),
    pub encoder: &'static str,
}

pub struct Recorder {
    latest: Arc<Mutex<Option<Arc<RawFrame>>>>,
    stop: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
    handle: Option<JoinHandle<Result<PathBuf>>>,
    pub info: RecordingInfo,
}

impl Recorder {
    pub fn start(first: Arc<RawFrame>, settings: RecordSettings, caps: &Capabilities, path: PathBuf) -> Result<Self> {
        let size = first.size;
        let mut encoder = Encoder::spawn(size, first.order, &settings, caps, &path)?;
        let info = RecordingInfo { started: Instant::now(), fps: settings.fps, size, encoder: encoder.name };

        let latest = Arc::new(Mutex::new(None::<Arc<RawFrame>>));
        let stop = Arc::new(AtomicBool::new(false));
        let written = Arc::new(AtomicU64::new(0));
        let handle = {
            let (latest, stop, written) = (latest.clone(), stop.clone(), written.clone());
            let started = info.started;
            let frame_time = Duration::from_secs_f64(1.0 / settings.fps as f64);
            let fps = u64::from(settings.fps);
            thread::Builder::new().name("recorder".into()).spawn(move || -> Result<PathBuf> {
                let mut current = first;
                // Frame slots the clock has passed, and frames actually written.
                let mut count: u64 = 0;
                let mut frames: u64 = 0;
                loop {
                    if let Some(frame) = latest.lock().unwrap().take() {
                        current = frame;
                    }
                    // Write as many frames as are due (more than one only if
                    // encoding fell behind), keeping wall-clock timing.
                    let due = (started.elapsed().as_secs_f64() / frame_time.as_secs_f64()) as u64 + 1;
                    let n = due.saturating_sub(count).max(1);
                    let fitted;
                    let data = if current.size == size {
                        &current.data
                    } else {
                        // e.g. a window was resized: letterbox into the original size.
                        fitted = fit_into(&current, size);
                        &fitted
                    };
                    // An encoder slower than real time would get a bigger batch
                    // each tick, and stopping would wait for all of it. Catch up
                    // by a second at most and skip the rest: the video jumps
                    // ahead, and the UI already says the encoder can't keep up.
                    for _ in 0..n.min(fps) {
                        encoder.write_frame(data)?;
                        frames += 1;
                        written.store(frames, Ordering::Relaxed);
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    count += n;

                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let next = started + frame_time.mul_f64(count as f64);
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        thread::sleep(wait);
                    }
                }
                encoder.finish()?;
                Ok(path)
            })?
        };

        Ok(Self { latest, stop, written, handle: Some(handle), info })
    }

    /// Offers the newest frame; the writer picks it up at its next tick.
    pub fn push(&self, frame: Arc<RawFrame>) {
        *self.latest.lock().unwrap() = Some(frame);
    }

    pub fn frames_written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// True if the writer stopped on its own (the encoder failed).
    pub fn has_failed(&self) -> bool {
        !self.stop.load(Ordering::Relaxed) && self.handle.as_ref().is_some_and(|h| h.is_finished())
    }

    /// Stops and waits for the file to be finalized. Blocking.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().map_err(|_| anyhow!("recording thread panicked"))?
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Scales a frame to fit inside `size` (nearest neighbour), centred on black.
fn fit_into(frame: &RawFrame, size: (u32, u32)) -> Vec<u8> {
    let (sw, sh) = frame.size;
    let (dw, dh) = size;
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for px in out.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    if sw == 0 || sh == 0 {
        return out;
    }
    let scale = (dw as f64 / sw as f64).min(dh as f64 / sh as f64);
    let (fw, fh) = (((sw as f64 * scale) as u32).clamp(1, dw), ((sh as f64 * scale) as u32).clamp(1, dh));
    let (ox, oy) = ((dw - fw) / 2, (dh - fh) / 2);
    for y in 0..fh {
        let sy = (y as u64 * sh as u64 / fh as u64) as usize;
        for x in 0..fw {
            let sx = (x as u64 * sw as u64 / fw as u64) as usize;
            let s = (sy * sw as usize + sx) * 4;
            let d = (((oy + y) * dw + ox + x) * 4) as usize;
            out[d..d + 4].copy_from_slice(&frame.data[s..s + 4]);
        }
    }
    out
}
