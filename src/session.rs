//! A capture session owns one open source on a background thread. Each new
//! frame is processed (on the GPU on Windows) as soon as it arrives and the UI
//! is woken immediately; stale frames are skipped, never queued.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use futures::channel::mpsc::UnboundedSender;

#[cfg(not(windows))]
use crate::screen::{ScreenCapture, Target};
#[cfg(windows)]
use crate::wgc::{Target, WgcCapture};
use crate::{
    encoder::{Capabilities, PixelOrder, RecordSettings},
    processor::Processor,
    recorder::{RawFrame, Recorder, RecordingInfo},
    scaling::{Method, Scaling},
    source::{self, DeviceFrame, DeviceMode, DeviceSource, SourceId},
};

/// How long a capture thread waits for a frame before checking for shutdown.
const WAIT: Duration = Duration::from_millis(100);

/// Preview pixels in BGRA order (what GPUI's `RenderImage` expects).
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub struct Preview {
    pub frame: PreviewFrame,
    /// When the source produced this frame (Windows' composition timestamp for
    /// desktops/windows; arrival from ffmpeg for devices).
    pub captured: Instant,
    pub method: Method,
}

#[derive(Clone, Debug)]
pub enum SourceState {
    Opening,
    Live { width: u32, height: u32 },
    Failed(String),
}

/// Which device mode to open with.
#[derive(Clone, Debug)]
pub enum ModeRequest {
    Auto,
    Mode(DeviceMode),
    /// A mode remembered from a previous run, matched by its label.
    Label(String),
}

struct Shared {
    state: Mutex<SourceState>,
    preview: Mutex<Option<Preview>>,
    modes: Mutex<Vec<DeviceMode>>,
    /// The mode in use (what the request or "Auto" resolved to).
    active_mode: Mutex<Option<DeviceMode>>,
    /// The mode the user explicitly chose; `None` means Auto.
    chosen_mode: Mutex<Option<DeviceMode>>,
    scaling: Mutex<Scaling>,
    captured: AtomicU64,
    /// Frames written to the current recording.
    recorded: AtomicU64,
    /// Set when a recording ends on its own (source lost, encoder failed).
    recording_error: Mutex<Option<String>>,
}

enum Command {
    Screenshot {
        path: PathBuf,
        reply: mpsc::Sender<Result<PathBuf>>,
    },
    StartRecording {
        settings: RecordSettings,
        caps: Capabilities,
        path: PathBuf,
        reply: mpsc::Sender<Result<RecordingInfo>>,
    },
    StopRecording {
        reply: mpsc::Sender<Result<PathBuf>>,
    },
}

pub struct Session {
    pub id: SourceId,
    shared: Arc<Shared>,
    /// Dropping this tells the capture thread to stop.
    commands: mpsc::Sender<Command>,
}

impl Session {
    pub fn open(id: SourceId, mode: ModeRequest, scaling: Scaling, wake: UnboundedSender<()>) -> Self {
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(SourceState::Opening),
            preview: Mutex::new(None),
            modes: Mutex::default(),
            active_mode: Mutex::default(),
            chosen_mode: Mutex::new(match &mode {
                ModeRequest::Mode(m) => Some(m.clone()),
                _ => None,
            }),
            scaling: Mutex::new(scaling),
            captured: AtomicU64::new(0),
            recorded: AtomicU64::new(0),
            recording_error: Mutex::new(None),
        });
        {
            let (id, shared) = (id.clone(), shared.clone());
            thread::Builder::new()
                .name("capture-session".into())
                .spawn(move || run(id, mode, shared, receiver, wake))
                .expect("failed to spawn capture thread");
        }
        Self { id, shared, commands }
    }

    pub fn state(&self) -> SourceState {
        self.shared.state.lock().unwrap().clone()
    }

    /// Modes the device supports (empty for desktops and windows).
    pub fn modes(&self) -> Vec<DeviceMode> {
        self.shared.modes.lock().unwrap().clone()
    }

    pub fn active_mode(&self) -> Option<DeviceMode> {
        self.shared.active_mode.lock().unwrap().clone()
    }

    /// The explicitly chosen mode, or `None` for Auto.
    pub fn chosen_mode(&self) -> Option<DeviceMode> {
        self.shared.chosen_mode.lock().unwrap().clone()
    }

    pub fn take_preview(&self) -> Option<Preview> {
        self.shared.preview.lock().unwrap().take()
    }

    pub fn captured_frames(&self) -> u64 {
        self.shared.captured.load(Ordering::Relaxed)
    }

    pub fn set_scaling(&self, scaling: Scaling) {
        *self.shared.scaling.lock().unwrap() = scaling;
    }

    /// Saves the newest frame as a full-resolution PNG. The channel yields the
    /// path once the file is written.
    pub fn screenshot(&self, path: PathBuf) -> mpsc::Receiver<Result<PathBuf>> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::Screenshot { path, reply: reply.clone() }, reply);
        rx
    }

    pub fn start_recording(
        &self,
        settings: RecordSettings,
        caps: Capabilities,
        path: PathBuf,
    ) -> Result<RecordingInfo> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::StartRecording { settings, caps, path, reply: reply.clone() }, reply);
        rx.recv().map_err(|_| anyhow!("the source stopped"))?
    }

    /// Stops recording; the channel yields the file once it is finalized.
    pub fn stop_recording(&self) -> mpsc::Receiver<Result<PathBuf>> {
        let (reply, rx) = mpsc::channel();
        self.send(Command::StopRecording { reply: reply.clone() }, reply);
        rx
    }

    pub fn recorded_frames(&self) -> u64 {
        self.shared.recorded.load(Ordering::Relaxed)
    }

    pub fn take_recording_error(&self) -> Option<String> {
        self.shared.recording_error.lock().unwrap().take()
    }

    fn send<T>(&self, command: Command, reply: mpsc::Sender<Result<T>>) {
        if self.commands.send(command).is_err() {
            let _ = reply.send(Err(anyhow!("the source stopped")));
        }
    }
}

enum Capture {
    #[cfg(windows)]
    Wgc(WgcCapture),
    #[cfg(not(windows))]
    Screen(ScreenCapture),
    Device(DeviceSource),
}

fn run(
    id: SourceId,
    request: ModeRequest,
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
    wake: UnboundedSender<()>,
) {
    let fail = |e: anyhow::Error| {
        *shared.state.lock().unwrap() = SourceState::Failed(format!("{e:#}"));
        let _ = wake.unbounded_send(());
    };
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }

    let mut gpu = match Processor::new() {
        Ok(gpu) => gpu,
        Err(e) => return fail(e),
    };

    let capture = match &id {
        #[cfg(windows)]
        SourceId::Display(h) => WgcCapture::new(Target::Monitor(*h), &gpu.device).map(Capture::Wgc),
        #[cfg(windows)]
        SourceId::Window(h) => WgcCapture::new(Target::Window(*h), &gpu.device).map(Capture::Wgc),
        #[cfg(not(windows))]
        SourceId::Display(h) => ScreenCapture::new(Target::Monitor(*h)).map(Capture::Screen),
        #[cfg(not(windows))]
        SourceId::Window(h) => ScreenCapture::new(Target::Window(*h)).map(Capture::Screen),
        SourceId::Device { path } => {
            // Discover the device's modes and resolve what to open with.
            let modes = source::probe_device_modes(path);
            let chosen = match request {
                ModeRequest::Auto => None,
                ModeRequest::Mode(m) => Some(m),
                ModeRequest::Label(label) => modes.iter().find(|m| m.label() == label).cloned(),
            }
            .filter(|m| modes.contains(m));
            let active = chosen.clone().or_else(|| source::best_mode(&modes).cloned());
            *shared.modes.lock().unwrap() = modes;
            *shared.chosen_mode.lock().unwrap() = chosen;
            *shared.active_mode.lock().unwrap() = active.clone();
            DeviceSource::open(path, active.as_ref()).map(Capture::Device)
        }
    };
    let mut capture = match capture {
        Ok(c) => c,
        Err(e) => return fail(e),
    };

    let mut size = None;
    let mut recorder: Option<Recorder> = None;
    loop {
        // Commands from the UI (and shutdown when the UI drops the session).
        loop {
            match commands.try_recv() {
                Ok(command) => handle(command, &mut gpu, &mut recorder, &shared),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(r) = recorder.take() {
                        let _ = r.finish(); // save what we have
                    }
                    return;
                }
            }
        }
        if recorder.as_ref().is_some_and(|r| r.has_failed()) {
            let error = recorder.take().unwrap().finish().err();
            *shared.recording_error.lock().unwrap() =
                Some(error.map_or("Recording stopped".into(), |e| format!("Recording stopped: {e:#}")));
            let _ = wake.unbounded_send(());
        }
        let scaling = *shared.scaling.lock().unwrap();

        // Frames that arrive as CPU pixels (devices, and screens outside Windows).
        let mut cpu_frame = |frame: Option<DeviceFrame>| -> Result<_> {
            frame
                .map(|f| {
                    let (preview, method) = gpu.process_rgba(&f.rgba, f.size, scaling)?;
                    if let Some(r) = &recorder {
                        r.push(Arc::new(RawFrame { data: f.rgba, size: f.size, order: PixelOrder::Rgba }));
                    }
                    Ok((preview, method, f.received, f.size))
                })
                .transpose()
        };
        let result = match &mut capture {
            #[cfg(not(windows))]
            Capture::Screen(c) => c.next_frame(WAIT).and_then(&mut cpu_frame),
            Capture::Device(d) => d.next_frame(WAIT).and_then(&mut cpu_frame),
            #[cfg(windows)]
            Capture::Wgc(c) => c.next_frame(WAIT).and_then(|frame| {
                frame
                    .map(|f| {
                        let captured = Instant::now().checked_sub(f.age).unwrap_or_else(Instant::now);
                        let (preview, method) = gpu.process_texture(&f.texture, f.size, scaling)?;
                        if let Some(r) = &recorder {
                            r.push(Arc::new(gpu.read_full()?));
                        }
                        Ok((preview, method, captured, f.size))
                    })
                    .transpose()
            }),
        };
        if let Some(r) = &recorder {
            shared.recorded.store(r.frames_written(), Ordering::Relaxed);
        }

        match result {
            Ok(Some((frame, method, captured, frame_size))) => {
                shared.captured.fetch_add(1, Ordering::Relaxed);
                if size != Some(frame_size) {
                    size = Some(frame_size);
                    *shared.state.lock().unwrap() = SourceState::Live { width: frame_size.0, height: frame_size.1 };
                }
                *shared.preview.lock().unwrap() = Some(Preview { frame, captured, method });
                let _ = wake.unbounded_send(());
            }
            Ok(None) => {}
            Err(e) => {
                if let Some(r) = recorder.take() {
                    let saved = match r.finish() {
                        Ok(path) => format!("Partial recording saved to {}", path.display()),
                        Err(e) => format!("The recording could not be saved: {e:#}"),
                    };
                    *shared.recording_error.lock().unwrap() = Some(format!("Source lost. {saved}"));
                }
                return fail(e);
            }
        }
    }
}

fn handle(command: Command, gpu: &mut Processor, recorder: &mut Option<Recorder>, shared: &Shared) {
    match command {
        Command::Screenshot { path, reply } => match gpu.read_full() {
            // Encoding a large PNG takes a moment; keep the preview running.
            Ok(frame) => {
                thread::spawn(move || {
                    let _ = reply.send(save_png(frame, path));
                });
            }
            Err(e) => {
                let _ = reply.send(Err(e));
            }
        },
        Command::StartRecording { settings, caps, path, reply } => {
            let result = if recorder.is_some() {
                Err(anyhow!("already recording"))
            } else {
                gpu.read_full().and_then(|first| {
                    shared.recorded.store(0, Ordering::Relaxed);
                    let r = Recorder::start(Arc::new(first), settings, &caps, path)?;
                    let info = r.info;
                    *recorder = Some(r);
                    Ok(info)
                })
            };
            let _ = reply.send(result);
        }
        Command::StopRecording { reply } => match recorder.take() {
            // Finalizing waits for ffmpeg; keep the preview running meanwhile.
            Some(r) => {
                thread::spawn(move || {
                    let _ = reply.send(r.finish());
                });
            }
            None => {
                let _ = reply.send(Err(anyhow!("not recording")));
            }
        },
    }
}

fn save_png(mut frame: RawFrame, path: PathBuf) -> Result<PathBuf> {
    if frame.order == PixelOrder::Bgra {
        for px in frame.data.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
    }
    let image = xcap::image::RgbaImage::from_raw(frame.size.0, frame.size.1, frame.data)
        .ok_or_else(|| anyhow!("bad frame size"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    image.save(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::mpsc::unbounded;

    fn wait_live(session: &Session, secs: u64) -> SourceState {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let state = session.state();
            if !matches!(state, SourceState::Opening) || Instant::now() > deadline {
                return state;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn fit(width: u32, height: u32) -> Scaling {
        Scaling::Fit { width, height, fsr: true, sharpness: Some(0.2) }
    }

    /// Needs a real display: `cargo test --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn display_preview_streams() {
        let (sources, _) = source::list_sources();
        let display = sources.iter().find(|s| matches!(s.id, SourceId::Display(_))).unwrap();
        let (wake, _rx) = unbounded();
        let session = Session::open(display.id.clone(), ModeRequest::Auto, fit(800, 400), wake);
        let SourceState::Live { width, height } = wait_live(&session, 5) else {
            panic!("display never went live: {:?}", session.state());
        };
        thread::sleep(Duration::from_millis(500));
        // The desktop only produces frames when something changes; the
        // first frame always arrives.
        let start = session.captured_frames();
        let mut latencies = vec![];
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut fitted = None;
        while Instant::now() < deadline {
            if let Some(p) = session.take_preview() {
                latencies.push(p.captured.elapsed());
                fitted = Some((p.frame.width, p.frame.height, p.method));
            }
            thread::sleep(Duration::from_millis(2));
        }
        let frames = session.captured_frames() - start;
        println!(
            "{width}×{height} → {fitted:?}; {} fps; latency samples (ms): {:?}",
            frames / 2,
            latencies.iter().map(|d| d.as_secs_f32() * 1000.).map(|v| (v * 10.).round() / 10.).collect::<Vec<_>>()
        );
        session.set_scaling(Scaling::Native);
        thread::sleep(Duration::from_millis(300));
    }

    /// Opens every capture card / camera and checks frames arrive (nothing is saved).
    #[test]
    #[ignore]
    fn device_sources_stream() {
        let (sources, errors) = source::list_sources();
        assert!(errors.is_empty(), "{errors:?}");
        for s in sources.iter().filter(|s| matches!(s.id, SourceId::Device { .. })) {
            let (wake, _rx) = unbounded();
            let session = Session::open(s.id.clone(), ModeRequest::Auto, fit(2560, 1440), wake);
            let state = wait_live(&session, 20);
            thread::sleep(Duration::from_millis(1000));
            let start = session.captured_frames();
            let mut latency = Duration::ZERO;
            let mut method = None;
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(1) {
                if let Some(p) = session.take_preview() {
                    latency = latency.max(p.captured.elapsed());
                    method = Some(p.method);
                }
                thread::sleep(Duration::from_millis(1));
            }
            println!(
                "{:?} {:?}: auto = {:?} -> {:?}; {} fps; {:?}; worst pipeline latency {:.1} ms",
                s.kind,
                s.name,
                session.active_mode().map(|m| m.label()),
                state,
                session.captured_frames() - start,
                method,
                latency.as_secs_f32() * 1000.
            );
            drop(session);
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// Screenshot + 2 s recording from the desktop and the first capture card.
    /// Files go to %TEMP%. `cargo test --release record_and_screenshot -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn record_and_screenshot() {
        let caps = Capabilities::detect();
        let (sources, _) = source::list_sources();
        let picks = [
            sources.iter().find(|s| matches!(s.id, SourceId::Display(_))),
            sources.iter().find(|s| s.kind == source::SourceKind::CaptureCard),
        ];
        for (n, s) in picks.into_iter().flatten().enumerate() {
            let (wake, _rx) = unbounded();
            let session = Session::open(s.id.clone(), ModeRequest::Auto, fit(1280, 720), wake);
            assert!(matches!(wait_live(&session, 20), SourceState::Live { .. }), "{:?}", session.state());
            thread::sleep(Duration::from_millis(300));
            let dir = std::env::temp_dir();
            let shot = session.screenshot(dir.join(format!("spectra-test-{n}.png"))).recv().unwrap().unwrap();
            let settings = RecordSettings::default();
            let info =
                session.start_recording(settings, caps.clone(), dir.join(format!("spectra-test-{n}.mp4"))).unwrap();
            thread::sleep(Duration::from_secs(2));
            let video = session.stop_recording().recv().unwrap().unwrap();
            println!(
                "{:?}: {} {:?} via {} -> {} / {}",
                s.kind,
                s.name,
                info.size,
                info.encoder,
                shot.display(),
                video.display()
            );
        }
    }
}
