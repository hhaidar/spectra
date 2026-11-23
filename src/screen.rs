//! Desktop and window capture with xcap, used where Windows Graphics Capture
//! isn't available (Linux, macOS). Frames are grabbed at a steady rate.

use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use xcap::{Monitor, Window};

use crate::source::DeviceFrame;

const INTERVAL: Duration = Duration::from_millis(33);

pub enum Target {
    Monitor(u32),
    Window(u32),
}

enum Handle {
    Monitor(Monitor),
    Window(Window),
}

pub struct ScreenCapture {
    handle: Handle,
    next: Instant,
}

impl ScreenCapture {
    pub fn new(target: Target) -> Result<Self> {
        let handle = match target {
            Target::Monitor(id) => Handle::Monitor(
                Monitor::all()?
                    .into_iter()
                    .find(|m| m.id().ok() == Some(id))
                    .ok_or_else(|| anyhow!("display is no longer connected"))?,
            ),
            Target::Window(id) => Handle::Window(
                Window::all()?
                    .into_iter()
                    .find(|w| w.id().ok() == Some(id))
                    .ok_or_else(|| anyhow!("the window was closed"))?,
            ),
        };
        Ok(Self { handle, next: Instant::now() })
    }

    /// Waits (up to `timeout`) until the next frame is due, then grabs it.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<DeviceFrame>> {
        let wait = self.next.saturating_duration_since(Instant::now());
        if wait > timeout {
            thread::sleep(timeout);
            return Ok(None);
        }
        thread::sleep(wait);
        self.next = Instant::now() + INTERVAL;
        let image = match &self.handle {
            Handle::Monitor(m) => m.capture_image()?,
            Handle::Window(w) => {
                if w.is_minimized().unwrap_or(false) {
                    return Ok(None); // keep showing the last frame
                }
                w.capture_image().map_err(|_| anyhow!("the window was closed"))?
            }
        };
        let size = image.dimensions();
        Ok(Some(DeviceFrame { rgba: image.into_raw(), size, received: Instant::now() }))
    }
}
