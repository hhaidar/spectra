//! Windows Graphics Capture: desktops and windows delivered as GPU textures as
//! soon as Windows composes them (no polling, no CPU copies of the full frame).

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::HWND,
        Graphics::{
            Direct3D11::{ID3D11Device, ID3D11Texture2D},
            Dxgi::IDXGIDevice,
            Gdi::HMONITOR,
        },
        System::{
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
            },
        },
        UI::WindowsAndMessaging::IsWindow,
    },
    core::{IInspectable, Interface, factory},
};

const PIXEL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;
const BUFFERS: i32 = 2;

pub enum Target {
    Monitor(u32),
    Window(u32),
}

pub struct WgcCapture {
    device: IDirect3DDevice,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    /// Kept alive so its Closed event fires when the window closes.
    _item: GraphicsCaptureItem,
    /// For windows: checked when frames stop, since Closed isn't always raised
    /// (e.g. when the window's process is killed).
    window: Option<HWND>,
    pool_size: SizeInt32,
    /// Set by the FrameArrived callback.
    signal: Arc<(Mutex<bool>, Condvar)>,
    closed: Arc<AtomicBool>,
}

/// A captured frame. The texture stays valid until this is dropped.
pub struct WgcFrame {
    frame: Direct3D11CaptureFrame,
    pub texture: ID3D11Texture2D,
    pub size: (u32, u32),
    /// How long ago Windows composed this frame.
    pub age: Duration,
}

impl Drop for WgcFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close(); // hand the buffer back to the pool
    }
}

impl WgcCapture {
    pub fn new(target: Target, d3d: &ID3D11Device) -> Result<Self> {
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let window = match &target {
            Target::Window(id) => Some(HWND(*id as usize as *mut _)),
            Target::Monitor(_) => None,
        };
        let item: GraphicsCaptureItem = unsafe {
            match target {
                Target::Monitor(id) => interop.CreateForMonitor(HMONITOR(id as usize as *mut _)),
                Target::Window(id) => interop.CreateForWindow(HWND(id as usize as *mut _)),
            }
        }
        .context("Windows can't capture this source")?;

        let device: IDirect3DDevice =
            unsafe { CreateDirect3D11DeviceFromDXGIDevice(&d3d.cast::<IDXGIDevice>()?)? }.cast()?;
        let pool_size = item.Size()?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&device, PIXEL_FORMAT, BUFFERS, pool_size)?;

        let signal = Arc::new((Mutex::new(false), Condvar::new()));
        {
            let signal = signal.clone();
            pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
                *signal.0.lock().unwrap() = true;
                signal.1.notify_one();
                Ok(())
            }))?;
        }
        let closed = Arc::new(AtomicBool::new(false));
        {
            let (closed, signal) = (closed.clone(), signal.clone());
            item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                closed.store(true, Ordering::Relaxed);
                signal.1.notify_one();
                Ok(())
            }))?;
        }

        let session = pool.CreateCaptureSession(&item)?;
        // Best effort: no yellow capture border (Windows 11), include the cursor.
        let _ = session.SetIsBorderRequired(false);
        let _ = session.SetIsCursorCaptureEnabled(true);
        session.StartCapture()?;

        Ok(Self { device, pool, session, _item: item, window, pool_size, signal, closed })
    }

    /// Waits up to `timeout` for a new frame and returns the newest one,
    /// skipping any older frames that queued up.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<WgcFrame>> {
        {
            let (lock, cvar) = &*self.signal;
            let mut ready = lock.lock().unwrap();
            if !*ready {
                ready = cvar.wait_timeout(ready, timeout).unwrap().0;
            }
            *ready = false;
        }
        if self.closed.load(Ordering::Relaxed) {
            bail!("the source was closed");
        }

        let mut newest = None;
        while let Ok(frame) = self.pool.TryGetNextFrame() {
            if let Some(old) = newest.replace(frame) {
                let _ = old.Close();
            }
        }
        let Some(frame) = newest else {
            if let Some(hwnd) = self.window
                && !unsafe { IsWindow(Some(hwnd)) }.as_bool()
            {
                bail!("the window was closed");
            }
            return Ok(None);
        };

        let content = frame.ContentSize()?;
        if content.Width != self.pool_size.Width || content.Height != self.pool_size.Height {
            // The window was resized: resize the buffers (takes effect next frame).
            self.pool.Recreate(&self.device, PIXEL_FORMAT, BUFFERS, content)?;
            self.pool_size = content;
        }

        let texture: ID3D11Texture2D =
            unsafe { frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()? };
        let mut desc = Default::default();
        unsafe { texture.GetDesc(&mut desc) };
        let size = ((content.Width.max(1) as u32).min(desc.Width), (content.Height.max(1) as u32).min(desc.Height));
        let age = qpc_age(frame.SystemRelativeTime()?.Duration);
        Ok(Some(WgcFrame { frame, texture, size, age }))
    }
}

impl Drop for WgcCapture {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Time since a QPC-based timestamp in 100 ns units.
fn qpc_age(timestamp_100ns: i64) -> Duration {
    let (mut now, mut freq) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut now);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    if freq == 0 {
        return Duration::ZERO;
    }
    let now_100ns = (now as i128 * 10_000_000 / freq as i128) as i64;
    Duration::from_nanos(((now_100ns - timestamp_100ns).max(0) as u64) * 100)
}
