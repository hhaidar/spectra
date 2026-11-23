#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod encoder;
// The preview pipeline: Direct3D 11 compute (with FSR) on Windows, CPU elsewhere.
#[cfg(windows)]
#[path = "gpu.rs"]
mod processor;
#[cfg(not(windows))]
#[path = "cpu.rs"]
mod processor;
mod recorder;
mod scaling;
#[cfg(not(windows))]
mod screen;
mod session;
mod settings;
mod source;
mod theme;
#[cfg(windows)]
mod wgc;
mod widgets;

use std::{
    cell::Cell,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use encoder::{Capabilities, Codec, Quality};
use futures::{StreamExt, channel::mpsc::UnboundedSender};
use gpui::{
    Animation, AnimationExt, App, Application, Bounds, ClickEvent, Context, CursorStyle, FocusHandle, FontWeight,
    KeyBinding, MouseButton, ObjectFit, PathPromptOptions, Pixels, Point, RenderImage, ScrollHandle, SharedString,
    Size, Task, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowOptions, actions, canvas, div, img,
    point, prelude::*, pulsating_between, px, relative, size,
};
use recorder::RecordingInfo;
use scaling::{Method, Scaling};
use session::{ModeRequest, Session, SourceState};
use settings::{SavedSource, SavedWindow, Settings, Sharpening};
use source::{DeviceMode, SourceId, SourceInfo, SourceKind};
use theme::{color, icon};
use widgets::{
    Dropdown, DropdownItem, Tooltip, brand_mark, ghost_button, icon_button, note, section_title, setting_row,
};
use xcap::image::{Frame, RgbaImage};

const SIDEBAR_WIDTH: f32 = 320.;
const SIDEBAR_INNER: f32 = SIDEBAR_WIDTH - 40.;
const CONTROL_WIDTH: f32 = 160.;
const TITLEBAR_HEIGHT: f32 = 36.;
/// Windows and macOS get Spectra's own dark title bar; Linux keeps its native one.
const CUSTOM_TITLEBAR: bool = cfg!(any(windows, target_os = "macos"));
const FPS_CHOICES: [u32; 4] = [24, 30, 60, 120];

actions!(spectra, [ToggleRecording, TakeScreenshot, ToggleFullscreen, ExitFullscreen, ToggleZoom]);

/// A shortcut as the platform writes it: ⌘ on macOS, Ctrl elsewhere.
fn shortcut(key: &str) -> String {
    if cfg!(target_os = "macos") { format!("⌘{key}") } else { format!("Ctrl+{key}") }
}

enum Recording {
    Idle,
    Active(RecordingInfo),
    Saving,
}

/// Result of the last recording / screenshot, shown under the preview.
struct Status {
    text: SharedString,
    is_error: bool,
    path: Option<PathBuf>,
}

struct Spectra {
    sources: Vec<SourceInfo>,
    loading_sources: bool,
    /// Which source category the Source dropdown lists.
    kind: SourceKind,
    session: Option<Session>,
    /// Name/details of the source the session was opened for.
    current: Option<SourceInfo>,
    settings: Settings,
    /// Encoders ffmpeg can use here; `None` while still probing.
    caps: Option<Capabilities>,
    recording: Recording,
    /// Capture threads ping this when a new frame is ready.
    wake: UnboundedSender<()>,

    preview_image: Option<Arc<RenderImage>>,
    /// Size of the preview area, measured during layout.
    preview_bounds: Rc<Cell<Size<Pixels>>>,
    scroll: ScrollHandle,
    /// Mouse position and scroll offset when a pan drag started.
    drag: Option<(Point<Pixels>, Point<Pixels>)>,

    // Live stats.
    fps: f32,
    fps_sample: (Instant, u64),
    /// Smoothed capture → on-screen-image latency, in milliseconds.
    latency_ms: Option<f32>,
    /// How the last frame was scaled, and to what size.
    method: Option<(Method, u32, u32)>,

    status: Option<Status>,
    /// Persistent problem (e.g. ffmpeg missing), shown in the sidebar.
    warning: Option<SharedString>,
    open_dropdown: Option<&'static str>,
    /// Keyboard shortcuts are handled while this has focus.
    focus: FocusHandle,
    /// Bumped by each screenshot, to play the shutter flash.
    flash: Option<usize>,
    flashes: usize,
    _tasks: Vec<Task<()>>,
}

impl Spectra {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Compile the GPU shaders while sources are being listed.
        cx.background_executor().spawn(async { processor::warm_up() }).detach();

        // Frames: render as soon as a capture thread says one is ready.
        let (wake, mut frames) = futures::channel::mpsc::unbounded::<()>();
        let on_frame = cx.spawn_in(window, async move |this, cx| {
            while frames.next().await.is_some() {
                while frames.try_recv().is_ok() {} // coalesce bursts
                if this.update_in(cx, |this, window, cx| this.on_frame(window, cx)).is_err() {
                    break;
                }
            }
        });
        // Slow tick for stats, the timer and the "Opening…" state.
        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                if this.update_in(cx, |this, window, cx| this.on_tick(window, cx)).is_err() {
                    break;
                }
            }
        });

        // On close: finish a recording instead of leaving it broken, and
        // remember where the window was.
        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            if let Some(this) = this.upgrade() {
                let (bounds, maximized) = match window.window_bounds() {
                    WindowBounds::Windowed(b) => (b, false),
                    WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
                };
                this.update(cx, |this, _| {
                    if let (Recording::Active(_), Some(session)) = (&this.recording, &this.session) {
                        let _ = session.stop_recording().recv();
                    }
                    this.settings.window = Some(SavedWindow {
                        x: f32::from(bounds.origin.x),
                        y: f32::from(bounds.origin.y),
                        width: f32::from(bounds.size.width),
                        height: f32::from(bounds.size.height),
                        maximized,
                    });
                    settings::save(&this.settings);
                });
            }
            true
        });

        let focus = cx.focus_handle();
        window.focus(&focus);

        let ffmpeg = source::ffmpeg_available();
        if ffmpeg {
            let caps = cx.background_executor().spawn(async { Capabilities::detect() });
            cx.spawn(async move |this, cx| {
                let caps = caps.await;
                this.update(cx, |this, cx| {
                    this.caps = Some(caps);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }

        let mut this = Self {
            sources: vec![],
            loading_sources: false,
            kind: SourceKind::CaptureCard,
            session: None,
            current: None,
            settings: settings::load(),
            caps: None,
            recording: Recording::Idle,
            wake,
            preview_image: None,
            preview_bounds: Rc::default(),
            scroll: ScrollHandle::new(),
            drag: None,
            fps: 0.,
            fps_sample: (Instant::now(), 0),
            latency_ms: None,
            method: None,
            status: None,
            warning: (!ffmpeg)
                .then(|| "ffmpeg was not found on PATH. Recording, capture cards and cameras need it.".into()),
            open_dropdown: None,
            focus,
            flash: None,
            flashes: 0,
            _tasks: vec![on_frame, ticker],
        };
        this.refresh_sources(cx);
        this
    }

    // -- Frames and stats -----------------------------------------------------

    /// How the capture thread should scale frames for the current view.
    fn scaling(&self, window: &Window) -> Scaling {
        if self.settings.native_zoom {
            return Scaling::Native;
        }
        let bounds = self.preview_bounds.get();
        let scale = window.scale_factor();
        Scaling::Fit {
            width: (f32::from(bounds.width) * scale).round().max(1.) as u32,
            height: (f32::from(bounds.height) * scale).round().max(1.) as u32,
            fsr: self.settings.fsr,
            sharpness: self.settings.sharpening.stops(),
        }
    }

    fn on_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        session.set_scaling(self.scaling(window));
        self.check_recording_error();
        let Some(preview) = self.session.as_ref().and_then(|s| s.take_preview()) else {
            cx.notify(); // e.g. the source failed
            return;
        };
        let (w, h) = (preview.frame.width, preview.frame.height);
        if let Some(buffer) = RgbaImage::from_raw(w, h, preview.frame.bgra) {
            let image = Arc::new(RenderImage::new(vec![Frame::new(buffer)]));
            self.set_preview(Some(image), window);
        }
        let sample = preview.captured.elapsed().as_secs_f32() * 1000.;
        self.latency_ms = Some(match self.latency_ms {
            Some(avg) => avg * 0.9 + sample * 0.1,
            None => sample,
        });
        self.method = Some((preview.method, w, h));
        cx.notify();
    }

    fn on_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        session.set_scaling(self.scaling(window));
        let (since, frames) = self.fps_sample;
        if since.elapsed() >= Duration::from_secs(1) {
            let now = session.captured_frames();
            self.fps = now.saturating_sub(frames) as f32 / since.elapsed().as_secs_f32();
            self.fps_sample = (Instant::now(), now);
        }
        self.check_recording_error();
        cx.notify();
    }

    fn check_recording_error(&mut self) {
        if let (Recording::Active(_), Some(session)) = (&self.recording, &self.session)
            && let Some(error) = session.take_recording_error()
        {
            self.recording = Recording::Idle;
            self.status = Some(Status { text: error.into(), is_error: true, path: None });
        }
    }

    fn set_preview(&mut self, image: Option<Arc<RenderImage>>, window: &mut Window) {
        if let Some(old) = std::mem::replace(&mut self.preview_image, image) {
            let _ = window.drop_image(old); // free the GPU texture
        }
    }

    // -- Sources --------------------------------------------------------------

    fn refresh_sources(&mut self, cx: &mut Context<Self>) {
        if self.loading_sources {
            return;
        }
        self.loading_sources = true;
        let list = cx.background_executor().spawn(async { source::list_sources() });
        cx.spawn(async move |this, cx| {
            let (sources, errors) = list.await;
            this.update(cx, |this, cx| {
                this.loading_sources = false;
                this.sources = sources;
                if !errors.is_empty() {
                    this.warning = Some(errors.join("; ").into());
                }
                if this.session.is_none() {
                    this.open_initial_source(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// The source from last time if it's still there, otherwise the first
    /// capture card, otherwise the primary display.
    fn open_initial_source(&mut self, cx: &mut Context<Self>) {
        let saved = self.settings.source.clone();
        let remembered = saved.as_ref().and_then(|saved| {
            self.sources
                .iter()
                .find(|s| match (&s.id, &saved.device_path) {
                    (SourceId::Device { path }, Some(saved_path)) => path == saved_path,
                    _ => s.kind == saved.kind && s.name == saved.name,
                })
                .cloned()
        });
        let (info, mode) = match remembered {
            Some(info) => (info, saved.and_then(|s| s.mode).map_or(ModeRequest::Auto, ModeRequest::Label)),
            None => {
                let fallback = self
                    .sources
                    .iter()
                    .find(|s| s.kind == SourceKind::CaptureCard)
                    .or_else(|| self.sources.iter().find(|s| s.kind == SourceKind::Display))
                    .cloned();
                let Some(info) = fallback else { return };
                (info, ModeRequest::Auto)
            }
        };
        self.kind = info.kind;
        self.start_session(info, mode, cx);
    }

    fn is_recording(&self) -> bool {
        !matches!(self.recording, Recording::Idle)
    }

    fn select_source(&mut self, info: SourceInfo, window: &mut Window, cx: &mut Context<Self>) {
        // Re-selecting a failed source retries it.
        let same_and_working =
            self.session.as_ref().is_some_and(|s| s.id == info.id && !matches!(s.state(), SourceState::Failed(_)));
        if same_and_working || self.is_recording() {
            return;
        }
        let mode = self
            .session
            .as_ref()
            .filter(|s| s.id == info.id)
            .and_then(|s| s.chosen_mode())
            .map_or(ModeRequest::Auto, ModeRequest::Mode);
        self.set_preview(None, window);
        self.start_session(info, mode, cx);
    }

    fn select_mode(&mut self, mode: Option<DeviceMode>, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(session), Some(info)) = (&self.session, self.current.clone()) else {
            return;
        };
        if session.chosen_mode() == mode || self.is_recording() {
            return;
        }
        self.set_preview(None, window);
        self.start_session(info, mode.map_or(ModeRequest::Auto, ModeRequest::Mode), cx);
    }

    fn start_session(&mut self, info: SourceInfo, mode: ModeRequest, cx: &mut Context<Self>) {
        self.session = None; // release a device before reopening it
        // Use the last measured preview size; it's re-applied on every frame.
        let bounds = self.preview_bounds.get();
        let scaling = if self.settings.native_zoom {
            Scaling::Native
        } else {
            Scaling::Fit {
                width: (f32::from(bounds.width) as u32).max(640),
                height: (f32::from(bounds.height) as u32).max(360),
                fsr: self.settings.fsr,
                sharpness: self.settings.sharpening.stops(),
            }
        };
        let mode_label = match &mode {
            ModeRequest::Auto => None,
            ModeRequest::Mode(m) => Some(m.label()),
            ModeRequest::Label(l) => Some(l.clone()),
        };
        self.session = Some(Session::open(info.id.clone(), mode, scaling, self.wake.clone()));
        self.settings.source = Some(SavedSource {
            kind: info.kind,
            name: info.name.clone(),
            device_path: match &info.id {
                SourceId::Device { path } => Some(path.clone()),
                _ => None,
            },
            mode: mode_label,
        });
        settings::save(&self.settings);
        self.current = Some(info);
        self.fps = 0.;
        self.fps_sample = (Instant::now(), 0);
        self.latency_ms = None;
        self.method = None;
        cx.notify();
    }

    fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings);
        settings::save(&self.settings);
        cx.notify();
    }

    fn set_native_zoom(&mut self, native: bool, cx: &mut Context<Self>) {
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.update_settings(cx, |s| s.native_zoom = native);
    }

    fn live_size(&self) -> Option<(u32, u32)> {
        match self.session.as_ref()?.state() {
            SourceState::Live { width, height } => Some((width, height)),
            _ => None,
        }
    }

    // -- Recording and screenshots --------------------------------------------

    fn output_path(&self, extension: &str) -> PathBuf {
        let name = chrono::Local::now().format("spectra-%Y-%m-%d_%H-%M-%S%.3f").to_string();
        self.settings.output_dir().join(format!("{name}.{extension}"))
    }

    fn can_record(&self) -> bool {
        self.caps.as_ref().is_some_and(|c| c.has_codec(self.settings.record.codec)) && self.live_size().is_some()
    }

    fn toggle_recording(&mut self, cx: &mut Context<Self>) {
        match self.recording {
            Recording::Idle => self.start_recording(cx),
            Recording::Active(_) => self.stop_recording(cx),
            Recording::Saving => {}
        }
    }

    fn start_recording(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(caps)) = (&self.session, &self.caps) else { return };
        let path = self.output_path("mp4");
        if let Err(e) = std::fs::create_dir_all(self.settings.output_dir()) {
            self.status = Some(Status {
                text: format!("Can't create the output folder: {e}").into(),
                is_error: true,
                path: None,
            });
            return cx.notify();
        }
        match session.start_recording(self.settings.record, caps.clone(), path) {
            Ok(info) => {
                self.status = None;
                self.recording = Recording::Active(info);
            }
            Err(e) => {
                self.status =
                    Some(Status { text: format!("Couldn't start recording: {e:#}").into(), is_error: true, path: None })
            }
        }
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        self.recording = Recording::Saving;
        let done = session.stop_recording();
        cx.notify();
        let result = cx
            .background_executor()
            .spawn(async move { done.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("the recording was lost"))) });
        cx.spawn(async move |this, cx| {
            let result = result.await;
            this.update(cx, |this, cx| {
                this.recording = Recording::Idle;
                this.status = Some(saved_status(result, "Recording"));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn take_screenshot(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let done = session.screenshot(self.output_path("png"));
        // A shutter flash over the preview, gone after a moment.
        self.flashes += 1;
        self.flash = Some(self.flashes);
        let flash = self.flashes;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(400)).await;
            this.update(cx, |this, cx| {
                if this.flash == Some(flash) {
                    this.flash = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        let result = cx
            .background_executor()
            .spawn(async move { done.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("the screenshot was lost"))) });
        cx.spawn(async move |this, cx| {
            let result = result.await;
            this.update(cx, |this, cx| {
                this.status = Some(saved_status(result, "Screenshot"));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn choose_output_dir(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Save recordings and screenshots here".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(dir) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| this.update_settings(cx, |s| s.output_dir = Some(dir))).ok();
            }
        })
        .detach();
    }

    /// A clickable sidebar section title with a chevron; toggles (and
    /// remembers) whether the section is expanded.
    fn section_header(
        &self,
        key: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.settings.is_expanded(key);
        div()
            .id(key)
            .flex()
            .items_center()
            .gap_1p5()
            .cursor_pointer()
            .child(
                div()
                    .w(px(10.))
                    .font_family(theme::ICONS)
                    .text_size(px(9.))
                    .text_color(color(theme::SUBTLE))
                    .child(if expanded { icon::CHEVRON_DOWN } else { icon::CHEVRON_RIGHT }),
            )
            .child(section_title(label))
            .hover(|s| s.opacity(0.8))
            .on_click(cx.listener(move |this, _, _, cx| this.update_settings(cx, |s| s.toggle_expanded(key))))
    }

    /// Wraps a [`Dropdown`] with this view's open/close bookkeeping.
    fn dropdown(
        &self,
        dropdown: Dropdown,
        id: &'static str,
        viewport: Size<Pixels>,
        on_select: impl Fn(&mut Self, usize, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        dropdown.render(
            self.open_dropdown == Some(id),
            viewport,
            |this: &mut Self, open, cx| {
                // Opening the source menus re-lists sources, so a device
                // plugged in since shows up.
                if matches!(open, Some("source" | "kind")) && this.open_dropdown != open {
                    this.refresh_sources(cx);
                }
                this.open_dropdown = open;
                cx.notify();
            },
            on_select,
            cx,
        )
    }

    // -- Rendering ----------------------------------------------------------

    fn render_titlebar(&self, window: &Window) -> impl IntoElement {
        let caption = |id: &'static str, glyph: &'static str, area: WindowControlArea, close: bool| {
            div()
                .id(id)
                .w(px(46.))
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .font_family(theme::ICONS)
                .text_size(px(10.))
                .text_color(color(theme::MUTED))
                .window_control_area(area)
                .map(|this| {
                    if close {
                        this.hover(|s| s.bg(color(0xc42b1c)).text_color(color(0xffffff)))
                    } else {
                        this.hover(|s| s.bg(color(theme::HOVER)).text_color(color(theme::TEXT)))
                    }
                })
                .child(glyph)
        };

        div()
            .h(px(TITLEBAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .bg(color(theme::BG))
            .border_b_1()
            .border_color(color(theme::BORDER))
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .map(|this| if cfg!(target_os = "macos") { this.pl(px(80.)) } else { this.pl_3() })
                    .window_control_area(WindowControlArea::Drag)
                    .child(brand_mark(16.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(color(theme::TEXT))
                            .child("Spectra"),
                    )
                    .when(matches!(self.recording, Recording::Active(_)), |this| {
                        this.child(
                            div()
                                .ml_2()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .bg(color(theme::RECORD).opacity(0.15))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(color(theme::RECORD))
                                .child(div().size_1p5().rounded_full().bg(color(theme::RECORD)))
                                .child("Recording"),
                        )
                    }),
            )
            .when(cfg!(windows), |this| {
                this.child(caption("min", icon::MINIMIZE, WindowControlArea::Min, false))
                    .child(caption(
                        "max",
                        if window.is_maximized() { icon::RESTORE } else { icon::MAXIMIZE },
                        WindowControlArea::Max,
                        false,
                    ))
                    .child(caption("close", icon::CLOSE, WindowControlArea::Close, true))
            })
    }

    /// The FSR tag's state and the tooltip explaining it.
    fn fsr_status(&self) -> (bool, String) {
        if !cfg!(windows) {
            return (false, "FSR off: Windows only".into());
        }
        if !self.settings.fsr {
            return (false, "FSR off: disabled in settings".into());
        }
        if self.settings.native_zoom {
            return (false, "FSR off: 1:1 view".into());
        }
        match self.method {
            Some((Method::Fsr, w, h)) => (true, format!("FSR on: upscaling to {w}×{h}")),
            Some((Method::Downscale, ..)) => (false, "FSR off: source is larger than the preview".into()),
            Some((Method::Copy, ..)) => (false, "FSR off: preview matches source size".into()),
            None => (false, "FSR off".into()),
        }
    }

    fn render_preview(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.session.as_ref().map(|s| s.state());
        let recording = matches!(self.recording, Recording::Active(_));
        let recording_time = match &self.recording {
            Recording::Active(info) => {
                let secs = info.started.elapsed().as_secs();
                Some(format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60))
            }
            _ => None,
        };
        let placeholder = |icon: gpui::AnyElement, text: String, hex: u32| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .child(icon)
                .child(div().max_w(px(420.)).text_center().text_sm().text_color(color(hex)).child(text))
        };
        // The brand mark, breathing while a source opens or waits for video.
        let mark = |busy: bool| {
            if busy {
                brand_mark(44.)
                    .with_animation(
                        "opening",
                        Animation::new(Duration::from_millis(1600)).repeat().with_easing(pulsating_between(0.3, 1.)),
                        |mark, delta| mark.opacity(delta),
                    )
                    .into_any_element()
            } else {
                brand_mark(44.).opacity(0.5).into_any_element()
            }
        };
        let name = self.current.as_ref().map_or("source".to_string(), |c| c.name.clone());
        let badge = || {
            div()
                .absolute()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(color(0x000000).opacity(0.65))
                .text_xs()
                .text_color(color(0xffffff))
        };

        let content = match (&state, &self.preview_image) {
            (Some(SourceState::Failed(e)), _) => placeholder(
                div()
                    .font_family(theme::ICONS)
                    .text_size(px(28.))
                    .text_color(color(theme::ERROR))
                    .child(icon::WARNING)
                    .into_any_element(),
                format!("Couldn't capture {name}: {e}."),
                theme::MUTED,
            )
            .child(ghost_button("retry", Some(icon::REFRESH), "Try again", false).on_click(cx.listener(
                |this, _, window, cx| {
                    if let Some(info) = this.current.clone() {
                        this.select_source(info, window, cx);
                    }
                },
            )))
            .into_any_element(),
            (Some(_), Some(image)) if !self.settings.native_zoom => {
                img(image.clone()).size_full().object_fit(ObjectFit::Contain).into_any_element()
            }
            (Some(_), Some(image)) => {
                // Size the image so one source pixel = one screen pixel.
                let scale = window.scale_factor();
                let native = image.size(0);
                let (w, h) = (native.width.0 as f32 / scale, native.height.0 as f32 / scale);
                div()
                    .id("pan")
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&self.scroll)
                    .cursor(if self.drag.is_some() { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                            this.drag = Some((e.position, this.scroll.offset()));
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, e: &gpui::MouseMoveEvent, _, cx| {
                        if let Some((start, offset)) = this.drag {
                            if e.pressed_button != Some(MouseButton::Left) {
                                this.drag = None;
                            } else {
                                this.scroll.set_offset(offset + (e.position - start));
                            }
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.drag = None;
                            cx.notify();
                        }),
                    )
                    .child(
                        // Centres the image when it's smaller than the view.
                        div()
                            .min_w(relative(1.))
                            .min_h(relative(1.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(img(image.clone()).w(px(w)).h(px(h)).flex_none()),
                    )
                    .into_any_element()
            }
            (Some(SourceState::Opening), None) => {
                placeholder(mark(true), format!("Opening {name}…"), theme::MUTED).into_any_element()
            }
            (Some(_), None) => {
                placeholder(mark(true), format!("Waiting for video from {name}…"), theme::MUTED).into_any_element()
            }
            (None, _) => placeholder(mark(false), "Choose a source".into(), theme::MUTED).into_any_element(),
        };

        let fullscreen = window.is_fullscreen();
        let fullscreen_button = div()
            .id("fullscreen")
            .px_1p5()
            .py_0p5()
            .rounded_sm()
            .font_family(theme::ICONS)
            .text_xs()
            .text_color(color(theme::MUTED))
            .cursor_pointer()
            .hover(|s| s.text_color(color(0xffffff)))
            .child(if fullscreen { icon::EXIT_FULLSCREEN } else { icon::FULLSCREEN })
            .tooltip(Tooltip::text(if fullscreen {
                "Exit full screen (Esc)"
            } else {
                "Full screen (F11, or double-click)"
            }))
            .on_click(|_, window, _| window.toggle_fullscreen());
        let zoom_tip = format!("Fit or 1:1 ({})", shortcut("1"));
        let zoom_toggle = div()
            .absolute()
            .bottom_3()
            .left_3()
            .flex()
            .items_center()
            .p_0p5()
            .gap_0p5()
            .rounded_md()
            .bg(color(0x000000).opacity(0.65))
            .child(fullscreen_button)
            .children([(false, "Fit"), (true, "1:1")].map(|(native, label)| {
                let selected = self.settings.native_zoom == native;
                div()
                    .id(label)
                    .tooltip(Tooltip::text(zoom_tip.clone()))
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .text_xs()
                    .text_color(color(if selected { 0xffffff } else { theme::MUTED }))
                    .when(selected, |this| this.bg(theme::accent()))
                    .when(!selected, |this| {
                        this.cursor_pointer()
                            .hover(|s| s.text_color(color(0xffffff)))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_native_zoom(native, cx)))
                    })
                    .child(label)
            }));

        let stats = self.live_size().map(|(w, h)| {
            let mut rest = format!("· {:.0} fps", self.fps);
            if let Some(ms) = self.latency_ms {
                rest.push_str(&format!(" · {ms:.0} ms"));
            }
            let (fsr_on, explanation) = self.fsr_status();
            let fsr_tag = div()
                .id("fsr-tag")
                .px_1p5()
                .rounded_sm()
                .border_1()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .map(|this| {
                    if fsr_on {
                        this.bg(theme::accent()).border_color(theme::accent()).text_color(color(0xffffff))
                    } else {
                        this.border_color(color(theme::SUBTLE)).text_color(color(theme::MUTED))
                    }
                })
                .child("FSR")
                .tooltip(Tooltip::text(explanation));
            badge().bottom_3().right_3().child(format!("{w}×{h}")).child(fsr_tag).child(rest)
        });

        let bounds = self.preview_bounds.clone();
        div()
            .id("preview")
            .relative()
            .w_full()
            .flex_1()
            .min_h(px(200.))
            .overflow_hidden()
            .bg(color(0x000000))
            .on_click(|e: &ClickEvent, window, _| {
                if e.click_count() == 2 {
                    window.toggle_fullscreen();
                }
            })
            // Measure the area so frames can be rendered at its exact pixel size.
            .child(canvas(move |b: Bounds<Pixels>, _, _| bounds.set(b.size), |_, _, _, _| {}).absolute().size_full())
            .child(content)
            .when_some(self.current.as_ref(), |this, info| {
                this.child(
                    badge()
                        .top_3()
                        .left_3()
                        .max_w(px(360.))
                        .when(recording, |this| {
                            this.child(div().size_2().flex_none().rounded_full().bg(color(theme::RECORD)))
                                .child(div().font_weight(FontWeight::BOLD).child("REC"))
                                .when_some(recording_time, |this, t| {
                                    this.child(div().font_family(theme::MONO).child(t))
                                })
                        })
                        .child(div().truncate().child(info.name.clone())),
                )
            })
            .when_some(stats, |this, stats| this.child(stats).child(zoom_toggle))
            .when_some(self.flash, |this, flash| {
                this.child(div().absolute().top_0().left_0().size_full().bg(color(0xffffff)).with_animation(
                    ("flash", flash),
                    Animation::new(Duration::from_millis(350)),
                    |flash, delta| flash.opacity(0.35 * (1. - delta)),
                ))
            })
    }

    /// Record / Screenshot buttons plus the recording summary and last result,
    /// shown at the top of the sidebar's Recording section.
    fn render_record_actions(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let saving = matches!(self.recording, Recording::Saving);
        let enabled = match self.recording {
            Recording::Idle => self.can_record(),
            Recording::Active(_) => true,
            Recording::Saving => false,
        };

        let (detail, warn) = match &self.recording {
            Recording::Active(info) => {
                let frames = self.session.as_ref().map_or(0, |s| s.recorded_frames());
                let expected = info.started.elapsed().as_secs_f64() * info.fps as f64;
                if expected - frames as f64 > info.fps as f64 {
                    // More than a second behind.
                    ("Encoder can't keep up: try a lower frame rate or the GPU encoder".to_string(), true)
                } else {
                    let (w, h) = info.size;
                    (format!("{w}×{h} · {} fps · {}", info.fps, info.encoder), false)
                }
            }
            Recording::Saving => ("Finishing the file…".to_string(), false),
            Recording::Idle => {
                let r = &self.settings.record;
                let encoder = match (&self.caps, self.live_size()) {
                    (None, _) => "detecting encoders…".to_string(),
                    (Some(caps), size) => {
                        caps.encoder_for(r, size.unwrap_or((1920, 1080))).unwrap_or("no encoder").to_string()
                    }
                };
                (format!("{} · {} fps · {encoder}", r.codec.label(), r.fps), false)
            }
        };

        let record_button = div()
            .id("record")
            .tooltip(Tooltip::text(format!("Record or stop ({})", shortcut("R"))))
            .h(px(32.))
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded_md()
            .border_1()
            .text_sm()
            .map(|this| {
                if let Recording::Active(info) = &self.recording {
                    let secs = info.started.elapsed().as_secs();
                    this.bg(color(theme::RECORD).opacity(0.12))
                        .border_color(color(theme::RECORD).opacity(0.6))
                        .text_color(color(theme::TEXT))
                        .child(div().size(px(8.)).rounded_sm().bg(color(theme::RECORD)))
                        .child("Stop")
                        .child(div().font_family(theme::MONO).text_color(color(theme::MUTED)).child(format!(
                            "{:02}:{:02}:{:02}",
                            secs / 3600,
                            secs / 60 % 60,
                            secs % 60
                        )))
                } else {
                    this.bg(color(theme::ELEVATED))
                        .border_color(color(theme::BORDER_STRONG))
                        .text_color(color(theme::TEXT))
                        .child(div().size(px(8.)).rounded_full().bg(color(theme::RECORD)))
                        .child(if saving { "Saving…" } else { "Record" })
                }
            })
            .map(|this| {
                if enabled {
                    this.cursor_pointer()
                        .hover(|s| s.bg(color(theme::HOVER)))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx)))
                } else {
                    this.opacity(0.45)
                }
            });

        let screenshot = icon_button("screenshot", icon::CAMERA, self.live_size().is_none())
            .tooltip(Tooltip::text(format!("Screenshot ({})", shortcut("S"))))
            .when(self.live_size().is_some(), |this| {
                this.on_click(cx.listener(|this, _, _, cx| this.take_screenshot(cx)))
            });

        let status = self.status.as_ref().map(|status| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(color(if status.is_error { theme::ERROR } else { theme::SUCCESS }))
                        .when(!status.is_error, |this| this.truncate())
                        .child(status.text.clone()),
                )
                .when_some(status.path.clone(), |this, path| {
                    let reveal = path.clone();
                    this.child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                ghost_button("open", Some(icon::OPEN), "Open", false)
                                    .flex_1()
                                    .justify_center()
                                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
                            )
                            .child(
                                ghost_button("reveal", Some(icon::FOLDER), "Show in folder", false)
                                    .flex_1()
                                    .justify_center()
                                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                            ),
                    )
                })
        });

        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().gap_2().child(record_button).child(screenshot))
            .child(
                div()
                    .text_xs()
                    .truncate()
                    .text_color(color(if warn { theme::WARNING } else { theme::SUBTLE }))
                    .child(detail),
            )
            .children(status)
    }

    fn render_sidebar(&self, viewport: Size<Pixels>, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.is_recording();

        // --- Source -------------------------------------------------------------
        let kind_items = SourceKind::ALL
            .iter()
            .map(|k| {
                let count = self.sources.iter().filter(|s| s.kind == *k).count();
                DropdownItem::new(k.label(), *k == self.kind).detail(if self.loading_sources {
                    "Searching…".to_string()
                } else {
                    format!("{count} available")
                })
            })
            .collect();
        let kind_dropdown = self.dropdown(
            Dropdown::new("kind", self.kind.label(), kind_items).menu_width(px(SIDEBAR_INNER - 40.)).disabled(busy),
            "kind",
            viewport,
            |this, i, window, cx| {
                this.kind = SourceKind::ALL[i];
                match this.kind {
                    // Window titles change constantly; keep the list fresh.
                    SourceKind::Window => this.refresh_sources(cx),
                    // Go straight to the first (primary) display.
                    SourceKind::Display => {
                        let first = this.sources.iter().find(|s| s.kind == SourceKind::Display).cloned();
                        if let Some(display) = first {
                            this.select_source(display, window, cx);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            },
            cx,
        );

        let listed: Vec<SourceInfo> = self.sources.iter().filter(|s| s.kind == self.kind).cloned().collect();
        let current_id = self.current.as_ref().map(|c| &c.id);
        let selected_here = self.current.as_ref().filter(|c| c.kind == self.kind);
        let source_items = listed
            .iter()
            .map(|info| {
                DropdownItem::new(
                    format!("{}{}", if info.primary { "★ " } else { "" }, info.name),
                    Some(&info.id) == current_id,
                )
                .detail(info.detail.clone())
            })
            .collect();
        let prompt = match self.kind {
            SourceKind::Display => "Choose a display",
            SourceKind::Window => "Choose a window or tab",
            SourceKind::CaptureCard => "Choose a capture card",
            SourceKind::Camera => "Choose a camera",
        };
        let source_dropdown = self.dropdown(
            Dropdown::new("source", selected_here.map_or(prompt.to_string(), |c| c.name.clone()), source_items)
                .placeholder(selected_here.is_none())
                .menu_width(px(SIDEBAR_INNER))
                .empty_text(self.kind.empty_text())
                .disabled(busy),
            "source",
            viewport,
            move |this, i, window, cx| {
                if let Some(info) = listed.get(i).cloned() {
                    this.select_source(info, window, cx);
                }
            },
            cx,
        );

        let device = self.session.as_ref().filter(|s| matches!(s.id, SourceId::Device { .. }));
        let mode_dropdown = device.map(|session| {
            let modes = session.modes();
            let chosen = session.chosen_mode();
            let active = session.active_mode().map(|m| m.label());
            let value = match (&chosen, &active) {
                (Some(m), _) => m.label(),
                (None, Some(a)) => format!("Auto · {a}"),
                (None, None) if modes.is_empty() => "Detecting modes…".to_string(),
                (None, None) => "Auto".to_string(),
            };
            let mut items = vec![
                DropdownItem::new("Auto", chosen.is_none())
                    .detail(active.map_or("Best available".into(), |a| format!("Best available · {a}"))),
            ];
            items.extend(modes.iter().map(|m| DropdownItem::new(m.label(), chosen.as_ref() == Some(m))));
            self.dropdown(
                Dropdown::new("mode", value, items).menu_width(px(SIDEBAR_INNER)).disabled(modes.is_empty() || busy),
                "mode",
                viewport,
                move |this, i, window, cx| {
                    let mode = if i == 0 { None } else { modes.get(i - 1).cloned() };
                    this.select_mode(mode, window, cx);
                },
                cx,
            )
        });

        let refresh = icon_button("refresh", icon::REFRESH, self.loading_sources)
            .when(!self.loading_sources, |this| this.on_click(cx.listener(|this, _, _, cx| this.refresh_sources(cx))));

        // --- Preview quality -----------------------------------------------------
        let s = &self.settings;
        let row_control = |el: gpui::AnyElement| div().w(px(CONTROL_WIDTH)).flex().child(el);
        let upscaling = self.dropdown(
            Dropdown::new(
                "upscaling",
                if s.fsr { "AMD FSR 1.0" } else { "Bilinear" },
                vec![
                    DropdownItem::new("AMD FSR 1.0", s.fsr).detail("Sharp edge-aware upscaling"),
                    DropdownItem::new("Bilinear", !s.fsr).detail("Simple, soft"),
                ],
            )
            .menu_width(px(220.)),
            "upscaling",
            viewport,
            |this, i, _, cx| this.update_settings(cx, |s| s.fsr = i == 0),
            cx,
        );
        let sharpening = s.fsr.then(|| {
            self.dropdown(
                Dropdown::new(
                    "sharpening",
                    s.sharpening.label(),
                    Sharpening::ALL
                        .iter()
                        .map(|level| DropdownItem::new(level.label(), s.sharpening == *level))
                        .collect(),
                )
                .menu_width(px(CONTROL_WIDTH)),
                "sharpening",
                viewport,
                |this, i, _, cx| this.update_settings(cx, |s| s.sharpening = Sharpening::ALL[i]),
                cx,
            )
        });

        // --- Recording -----------------------------------------------------------
        let r = s.record;
        let caps = self.caps.as_ref();
        let codec = self.dropdown(
            Dropdown::new(
                "codec",
                r.codec.label(),
                Codec::ALL
                    .iter()
                    .map(|c| {
                        let available = caps.is_none_or(|caps| caps.has_codec(*c));
                        DropdownItem::new(c.label(), r.codec == *c).detail(if available {
                            c.detail()
                        } else {
                            "Not available"
                        })
                    })
                    .collect(),
            )
            .menu_width(px(220.))
            .disabled(busy),
            "codec",
            viewport,
            |this, i, _, cx| this.update_settings(cx, |s| s.record.codec = Codec::ALL[i]),
            cx,
        );
        let quality = self.dropdown(
            Dropdown::new(
                "quality",
                r.quality.label(),
                Quality::ALL.iter().map(|q| DropdownItem::new(q.label(), r.quality == *q).detail(q.detail())).collect(),
            )
            .menu_width(px(220.))
            .disabled(busy),
            "quality",
            viewport,
            |this, i, _, cx| this.update_settings(cx, |s| s.record.quality = Quality::ALL[i]),
            cx,
        );
        let fps = self.dropdown(
            Dropdown::new(
                "fps",
                format!("{} fps", r.fps),
                FPS_CHOICES.iter().map(|f| DropdownItem::new(format!("{f} fps"), r.fps == *f)).collect(),
            )
            .menu_width(px(CONTROL_WIDTH))
            .disabled(busy),
            "fps",
            viewport,
            |this, i, _, cx| this.update_settings(cx, |s| s.record.fps = FPS_CHOICES[i]),
            cx,
        );
        let gpu_encoder = caps.and_then(|c| c.hardware_encoder(r.codec));
        let gpu_label = match gpu_encoder {
            Some(e) if e.ends_with("_nvenc") => "GPU · NVENC",
            Some(e) if e.ends_with("_amf") => "GPU · AMD",
            Some(_) => "GPU · Intel",
            None => "GPU",
        };
        let encoder = self.dropdown(
            Dropdown::new(
                "encoder",
                if r.hardware { gpu_label } else { "CPU" },
                vec![
                    DropdownItem::new(gpu_label, r.hardware).detail(if gpu_encoder.is_some() || caps.is_none() {
                        "Fast, low CPU use"
                    } else {
                        "Not available, uses CPU"
                    }),
                    DropdownItem::new("CPU", !r.hardware).detail("Software encoder, slower"),
                ],
            )
            .menu_width(px(220.))
            .disabled(busy),
            "encoder",
            viewport,
            |this, i, _, cx| this.update_settings(cx, |s| s.record.hardware = i == 0),
            cx,
        );
        // GPU H.264 can't take frames wider/taller than 4096 px.
        let cpu_fallback = match (caps, self.live_size()) {
            (Some(caps), Some(size)) => {
                r.hardware && gpu_encoder.is_some() && caps.encoder_for(&r, size).is_some_and(|e| e.starts_with("lib"))
            }
            _ => false,
        };

        let folder = div()
            .id("folder")
            .tooltip(Tooltip::text(s.output_dir().display().to_string()))
            .h(px(32.))
            .w_full()
            .px_2p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .bg(color(theme::ELEVATED))
            .border_1()
            .border_color(color(theme::BORDER_STRONG))
            .child(
                div()
                    .flex_none()
                    .font_family(theme::ICONS)
                    .text_size(px(12.))
                    .text_color(color(theme::MUTED))
                    .child(icon::FOLDER),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(color(theme::TEXT))
                    .child(s.output_dir().display().to_string()),
            )
            .child(div().flex_none().text_xs().text_color(color(theme::MUTED)).child("Change"))
            .map(|this| {
                if busy {
                    this.opacity(0.45)
                } else {
                    this.cursor_pointer()
                        .hover(|s| s.bg(color(theme::HOVER)))
                        .on_click(cx.listener(|this, _, _, cx| this.choose_output_dir(cx)))
                }
            });

        // --- Details ----------------------------------------------------------------
        let collapsed = !s.is_expanded("details");
        let details_header = self.section_header("details", "Details", cx);
        let detail = |label: &'static str, value: String| {
            setting_row(label, div().text_sm().text_color(color(theme::TEXT)).child(value))
        };
        let details = self.current.as_ref().map(|info| {
            let status = match self.session.as_ref().map(|s| s.state()) {
                Some(SourceState::Live { .. }) => "Live",
                Some(SourceState::Opening) => "Opening…",
                Some(SourceState::Failed(_)) => "Unavailable",
                None => "—",
            };
            let live = self.live_size().is_some();
            let scaled = match self.method {
                Some((Method::Copy, ..)) => "Native, pixel-exact".to_string(),
                Some((Method::Downscale, w, h)) => format!("Downscaled to {w} × {h}"),
                Some((Method::Fsr, w, h)) => format!("FSR to {w} × {h}"),
                None => "—".into(),
            };
            div().flex().flex_col().gap_2p5().child(details_header).when(!collapsed, |this| {
                this.child(detail("Status", status.into()))
                    .child(detail("Type", info.kind.label().into()))
                    .child(detail("Resolution", self.live_size().map_or("—".into(), |(w, h)| format!("{w} × {h}"))))
                    .child(detail("Frame rate", if live { format!("{:.1} fps", self.fps) } else { "—".into() }))
                    .child(detail(
                        "Latency",
                        match self.latency_ms {
                            Some(ms) if live => format!("{ms:.1} ms"),
                            _ => "—".into(),
                        },
                    ))
                    .child(detail("Preview", scaled))
                    .when_some(device.and_then(|s| s.active_mode()), |this, mode| {
                        this.child(detail("Format", mode.label().rsplit(" · ").next().unwrap_or_default().to_string()))
                    })
            })
        });

        let record_actions = self.render_record_actions(cx).into_any_element();
        let sections = div()
            .id("sidebar")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(section_title("Source"))
                    .child(div().flex().gap_2().child(kind_dropdown).child(refresh))
                    .child(div().flex().child(source_dropdown))
                    .when_some(mode_dropdown, |this, mode| {
                        this.child(div().mt_2().child(section_title("Device mode")))
                            .child(div().flex().child(mode))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(self.section_header("preview", "Preview", cx))
                    .when(s.is_expanded("preview"), |this| {
                        this.child(setting_row("Upscaling", row_control(upscaling)))
                            .when_some(sharpening, |this, sharpening| {
                                this.child(setting_row("Sharpening", row_control(sharpening)))
                            })
                    }),
            )
            .children(details)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2p5()
                    .child(self.section_header("recording", "Recording", cx))
                    .when(s.is_expanded("recording"), |this| {
                        this.child(setting_row("Codec", row_control(codec)))
                            .child(setting_row("Quality", row_control(quality)))
                            .child(setting_row("Frame rate", row_control(fps)))
                            .child(setting_row("Encoder", row_control(encoder)))
                            .when(cpu_fallback, |this| {
                                this.child(note(
                                    "GPU H.264 is limited to 4096 px, so this source records on the CPU. Pick HEVC or AV1 to use the GPU.",
                                    true,
                                ))
                            })
                            .child(div().mt_1().child(div().text_sm().text_color(color(theme::MUTED)).child("Save to")))
                            .child(folder)
                            .child(div().mt_2().child(record_actions))
                    }),
            )
            .when_some(self.warning.clone(), |this, msg| this.child(note(msg, true)));

        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(color(theme::SURFACE))
            .border_l_1()
            .border_color(color(theme::BORDER))
            .child(sections)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_end()
                    .px_5()
                    .pb_3()
                    .pt_1()
                    .text_xs()
                    .text_color(color(theme::SUBTLE))
                    .opacity(0.7)
                    .child("© 2026 f+e"),
            )
    }
}

/// Status line for a finished recording or screenshot.
fn saved_status(result: anyhow::Result<PathBuf>, what: &str) -> Status {
    match result {
        Ok(path) => Status {
            text: format!("{what} saved: {}", path.file_name().unwrap_or_default().to_string_lossy()).into(),
            is_error: false,
            path: Some(path),
        },
        Err(e) => Status { text: format!("{what} failed: {e:#}").into(), is_error: true, path: None },
    }
}

impl Render for Spectra {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        // Full screen shows the video alone.
        let fullscreen = window.is_fullscreen();
        div()
            .key_context("Spectra")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleRecording, _, cx| {
                if !matches!(this.recording, Recording::Idle) || this.can_record() {
                    this.toggle_recording(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &TakeScreenshot, _, cx| {
                if this.live_size().is_some() {
                    this.take_screenshot(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| {
                let native = !this.settings.native_zoom;
                this.set_native_zoom(native, cx);
            }))
            .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
            .on_action(cx.listener(|this, _: &ExitFullscreen, window, cx| {
                this.open_dropdown = None;
                if window.is_fullscreen() {
                    window.toggle_fullscreen();
                }
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(color(theme::BG))
            .text_color(color(theme::TEXT))
            .when(CUSTOM_TITLEBAR && !fullscreen, |this| this.child(self.render_titlebar(window)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(div().flex_1().min_w_0().flex().flex_col().child(self.render_preview(window, cx)))
                    .when(!fullscreen, |this| this.child(self.render_sidebar(viewport, cx))),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("secondary-r", ToggleRecording, None),
            KeyBinding::new("secondary-s", TakeScreenshot, None),
            KeyBinding::new("secondary-1", ToggleZoom, None),
            KeyBinding::new("f11", ToggleFullscreen, None),
            KeyBinding::new("escape", ExitFullscreen, None),
        ]);

        // Where the window was last time, if that's still on a connected display.
        let default = Bounds::centered(None, size(px(1180.), px(760.)), cx);
        let window_bounds = settings::load()
            .window
            .map(|w| (Bounds::new(point(px(w.x), px(w.y)), size(px(w.width), px(w.height))), w.maximized))
            .filter(|(b, _)| cx.displays().iter().any(|d| d.bounds().intersects(b)))
            .map_or(WindowBounds::Windowed(default), |(b, maximized)| {
                if maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) }
            });
        cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("Spectra".into()),
                    appears_transparent: CUSTOM_TITLEBAR,
                    traffic_light_position: Some(point(px(14.), px(12.))),
                }),
                window_min_size: Some(size(px(900.), px(560.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Spectra::new(window, cx)),
        )
        .expect("failed to open window");
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}
