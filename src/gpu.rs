//! GPU preview pipeline (Direct3D 11 compute).
//!
//! Takes a captured frame (a GPU texture from Windows Graphics Capture, or CPU
//! pixels from a camera/capture card), scales it to exactly the size it will be
//! shown at, and reads back BGRA bytes ready for GPUI:
//!
//! * smaller than the source → area-weighted downscale in linear light
//! * larger than the source  → AMD FSR 1.0 (EASU + optional RCAS), if enabled
//! * same size / 1:1 view    → bit-exact copy

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0, D3D_SHADER_MACRO,
                Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile},
                ID3DBlob,
            },
            Direct3D11::*,
            Dxgi::Common::{
                DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
                DXGI_SAMPLE_DESC,
            },
        },
    },
    core::{Interface, PCSTR},
};

use crate::{
    encoder::PixelOrder,
    recorder::RawFrame,
    scaling::{Method, Scaling, plan},
    session::PreviewFrame,
};

const PREVIEW_HLSL: &str = include_str!("shaders/preview.hlsl");
const FFX_A: &str = include_str!("shaders/ffx_a.h");
const FFX_FSR1: &str = include_str!("shaders/ffx_fsr1.h");

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Params {
    con: [[u32; 4]; 4],
    src_size: [u32; 2],
    dst_size: [u32; 2],
}

struct Target {
    size: (u32, u32),
    texture: ID3D11Texture2D,
    uav: ID3D11UnorderedAccessView,
    srv: ID3D11ShaderResourceView,
}

pub struct Processor {
    pub device: ID3D11Device,
    context: ID3D11DeviceContext,
    copy: ID3D11ComputeShader,
    downscale: ID3D11ComputeShader,
    easu: ID3D11ComputeShader,
    rcas: ID3D11ComputeShader,
    params: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    /// Final BGRA output, the FSR intermediate, and a CPU-readable copy.
    output: Option<Target>,
    intermediate: Option<Target>,
    staging: Option<(ID3D11Texture2D, (u32, u32))>,
    /// For frames that arrive as CPU pixels (cameras / capture cards).
    upload: Option<(ID3D11Texture2D, ID3D11ShaderResourceView, (u32, u32))>,
    /// Copy of the newest captured frame (WGC frames go back to Windows right
    /// after processing), for screenshots and recording.
    snapshot: Option<(ID3D11Texture2D, (u32, u32))>,
    /// Which texture holds the newest full-resolution frame.
    newest: Option<Newest>,
    full_staging: Option<(ID3D11Texture2D, (u32, u32), DXGI_FORMAT)>,
}

#[derive(Clone, Copy)]
enum Newest {
    Snapshot,
    Upload,
}

impl Processor {
    pub fn new() -> Result<Self> {
        // The GPU, or Windows' software renderer (WARP) where there is none
        // (virtual machines, CI runners, some remote sessions).
        let create = |driver| {
            let mut device = None;
            let mut context = None;
            unsafe {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .map(|()| (device.unwrap(), context.unwrap()))
        };
        let (device, context): (ID3D11Device, ID3D11DeviceContext) = create(D3D_DRIVER_TYPE_HARDWARE)
            .or_else(|_| create(D3D_DRIVER_TYPE_WARP))
            .context("failed to create a Direct3D 11 device")?;
        // Windows Graphics Capture touches the device from its own threads.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            unsafe {
                let _ = mt.SetMultithreadProtected(true);
            }
        }

        let params = unsafe {
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: std::mem::size_of::<Params>().next_multiple_of(16) as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            let mut buffer = None;
            device.CreateBuffer(&desc, None, Some(&mut buffer))?;
            buffer.unwrap()
        };
        let sampler = unsafe {
            let desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MaxLOD: f32::MAX,
                ..Default::default()
            };
            let mut sampler = None;
            device.CreateSamplerState(&desc, Some(&mut sampler))?;
            sampler.unwrap()
        };

        Ok(Self {
            copy: compile(&device, "KERNEL_COPY", false)?,
            downscale: compile(&device, "KERNEL_DOWNSCALE", false)?,
            easu: compile(&device, "KERNEL_EASU", true)?,
            rcas: compile(&device, "KERNEL_RCAS", true)?,
            device,
            context,
            params,
            sampler,
            output: None,
            intermediate: None,
            staging: None,
            upload: None,
            snapshot: None,
            newest: None,
            full_staging: None,
        })
    }

    /// Processes the top-left `size` region of a GPU texture.
    pub fn process_texture(
        &mut self,
        texture: &ID3D11Texture2D,
        size: (u32, u32),
        scaling: Scaling,
    ) -> Result<(PreviewFrame, Method)> {
        let srv = unsafe {
            let mut srv = None;
            self.device.CreateShaderResourceView(texture, None, Some(&mut srv))?;
            srv.unwrap()
        };
        // Keep our own copy: the capture buffer is handed back to Windows.
        if self.snapshot.as_ref().map(|s| s.1) != Some(size) {
            let copy = self.texture(size, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_SHADER_RESOURCE.0 as u32)?;
            self.snapshot = Some((copy, size));
        }
        let region = D3D11_BOX { left: 0, top: 0, front: 0, right: size.0, bottom: size.1, back: 1 };
        unsafe {
            self.context.CopySubresourceRegion(
                &self.snapshot.as_ref().unwrap().0,
                0,
                0,
                0,
                0,
                texture,
                0,
                Some(&region),
            );
        }
        self.newest = Some(Newest::Snapshot);
        self.process(&srv, size, scaling)
    }

    /// Reads the newest frame back at full resolution.
    pub fn read_full(&mut self) -> Result<RawFrame> {
        let (texture, size, format, order) = match self.newest {
            Some(Newest::Snapshot) => {
                let (t, size) = self.snapshot.as_ref().unwrap();
                (t.clone(), *size, DXGI_FORMAT_B8G8R8A8_UNORM, PixelOrder::Bgra)
            }
            Some(Newest::Upload) => {
                let (t, _, size) = self.upload.as_ref().unwrap();
                (t.clone(), *size, DXGI_FORMAT_R8G8B8A8_UNORM, PixelOrder::Rgba)
            }
            None => bail!("no frame yet"),
        };
        if self.full_staging.as_ref().map(|s| (s.1, s.2)) != Some((size, format)) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging))? };
            self.full_staging = Some((staging.unwrap(), size, format));
        }
        let staging = self.full_staging.as_ref().unwrap().0.clone();
        let data = self.copy_out(&staging, &texture, size)?;
        Ok(RawFrame { data, size, order })
    }

    /// Uploads RGBA pixels and processes them.
    pub fn process_rgba(&mut self, rgba: &[u8], size: (u32, u32), scaling: Scaling) -> Result<(PreviewFrame, Method)> {
        if self.upload.as_ref().map(|u| u.2) != Some(size) {
            let texture = self.texture(size, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_SHADER_RESOURCE.0 as u32)?;
            let srv = unsafe {
                let mut srv = None;
                self.device.CreateShaderResourceView(&texture, None, Some(&mut srv))?;
                srv.unwrap()
            };
            self.upload = Some((texture, srv, size));
        }
        let (texture, srv, _) = self.upload.as_ref().unwrap();
        unsafe {
            self.context.UpdateSubresource(texture, 0, None, rgba.as_ptr().cast(), size.0 * 4, 0);
        }
        self.newest = Some(Newest::Upload);
        let srv = srv.clone();
        self.process(&srv, size, scaling)
    }

    fn process(
        &mut self,
        src: &ID3D11ShaderResourceView,
        src_size: (u32, u32),
        scaling: Scaling,
    ) -> Result<(PreviewFrame, Method)> {
        let (dst_size, method, sharpness) = plan(src_size, scaling, true);
        self.ensure_targets(dst_size, method == Method::Fsr)?;
        let output = self.output.as_ref().unwrap();
        let (out_uav, out_texture) = (output.uav.clone(), output.texture.clone());

        match method {
            Method::Copy => self.dispatch(&self.copy, src, &out_uav, Params::sizes(src_size, dst_size)),
            Method::Downscale => self.dispatch(&self.downscale, src, &out_uav, Params::sizes(src_size, dst_size)),
            Method::Fsr => {
                let mid = self.intermediate.as_ref().unwrap();
                let (mid_uav, mid_srv) = (mid.uav.clone(), mid.srv.clone());
                let mut params = Params::sizes(src_size, dst_size);
                params.con = easu_con(src_size, src_size, dst_size);
                self.dispatch(&self.easu, src, &mid_uav, params);
                match sharpness {
                    Some(stops) => {
                        let mut params = Params::sizes(dst_size, dst_size);
                        params.con[0] = [(-stops).exp2().to_bits(), 0, 0, 0];
                        self.dispatch(&self.rcas, &mid_srv, &out_uav, params);
                    }
                    None => self.dispatch(&self.copy, &mid_srv, &out_uav, Params::sizes(dst_size, dst_size)),
                }
            }
        }

        Ok((self.read_back(&out_texture, dst_size)?, method))
    }

    fn dispatch(
        &self,
        shader: &ID3D11ComputeShader,
        src: &ID3D11ShaderResourceView,
        dst: &ID3D11UnorderedAccessView,
        params: Params,
    ) {
        let ctx = &self.context;
        unsafe {
            ctx.UpdateSubresource(&self.params, 0, None, (&params as *const Params).cast(), 0, 0);
            ctx.CSSetShader(shader, None);
            ctx.CSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
            ctx.CSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.CSSetShaderResources(0, Some(&[Some(src.clone())]));
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&Some(dst.clone())), None);
            ctx.Dispatch(params.dst_size[0].div_ceil(8), params.dst_size[1].div_ceil(8), 1);
            // Unbind so the next pass can read what this one wrote.
            ctx.CSSetShaderResources(0, Some(&[None]));
            ctx.CSSetUnorderedAccessViews(0, 1, Some(&None), None);
        }
    }

    fn read_back(&mut self, texture: &ID3D11Texture2D, size: (u32, u32)) -> Result<PreviewFrame> {
        if self.staging.as_ref().map(|s| s.1) != Some(size) {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut staging = None;
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging))? };
            self.staging = Some((staging.unwrap(), size));
        }
        let staging = self.staging.as_ref().unwrap().0.clone();
        let bgra = self.copy_out(&staging, texture, size)?;
        Ok(PreviewFrame { width: size.0, height: size.1, bgra })
    }

    /// Copies `texture` into `staging` and reads it as tightly packed rows.
    fn copy_out(&self, staging: &ID3D11Texture2D, texture: &ID3D11Texture2D, size: (u32, u32)) -> Result<Vec<u8>> {
        let row = size.0 as usize * 4;
        let mut data = vec![0u8; row * size.1 as usize];
        unsafe {
            self.context.CopyResource(staging, texture);
            // Map waits for the GPU to finish: we want this frame now, not later.
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let src = mapped.pData as *const u8;
            for y in 0..size.1 as usize {
                std::ptr::copy_nonoverlapping(
                    src.add(y * mapped.RowPitch as usize),
                    data.as_mut_ptr().add(y * row),
                    row,
                );
            }
            self.context.Unmap(staging, 0);
        }
        Ok(data)
    }

    fn ensure_targets(&mut self, size: (u32, u32), fsr: bool) -> Result<()> {
        if self.output.as_ref().map(|t| t.size) != Some(size) {
            self.output = Some(self.target(size, DXGI_FORMAT_R8G8B8A8_UNORM)?);
        }
        if fsr && self.intermediate.as_ref().map(|t| t.size) != Some(size) {
            self.intermediate = Some(self.target(size, DXGI_FORMAT_R16G16B16A16_FLOAT)?);
        }
        Ok(())
    }

    fn target(&self, size: (u32, u32), format: DXGI_FORMAT) -> Result<Target> {
        let texture =
            self.texture(size, format, (D3D11_BIND_UNORDERED_ACCESS.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32)?;
        let (mut uav, mut srv) = (None, None);
        unsafe {
            self.device.CreateUnorderedAccessView(&texture, None, Some(&mut uav))?;
            self.device.CreateShaderResourceView(&texture, None, Some(&mut srv))?;
        }
        Ok(Target { size, texture, uav: uav.unwrap(), srv: srv.unwrap() })
    }

    fn texture(&self, size: (u32, u32), format: DXGI_FORMAT, bind: u32) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.0,
            Height: size.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            ..Default::default()
        };
        let mut texture = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture))? };
        texture.ok_or_else(|| anyhow!("CreateTexture2D returned nothing"))
    }
}

impl Params {
    fn sizes(src: (u32, u32), dst: (u32, u32)) -> Self {
        Self { src_size: [src.0, src.1], dst_size: [dst.0, dst.1], ..Default::default() }
    }
}

/// `FsrEasuCon` from ffx_fsr1.h, evaluated on the CPU.
fn easu_con(viewport: (u32, u32), input: (u32, u32), output: (u32, u32)) -> [[u32; 4]; 4] {
    let (vw, vh) = (viewport.0 as f32, viewport.1 as f32);
    let (iw, ih) = (input.0 as f32, input.1 as f32);
    let (ow, oh) = (output.0 as f32, output.1 as f32);
    let b = f32::to_bits;
    [
        [b(vw / ow), b(vh / oh), b(0.5 * vw / ow - 0.5), b(0.5 * vh / oh - 0.5)],
        [b(1.0 / iw), b(1.0 / ih), b(1.0 / iw), b(-1.0 / ih)],
        [b(-1.0 / iw), b(2.0 / ih), b(1.0 / iw), b(2.0 / ih)],
        [b(0.0), b(4.0 / ih), 0, 0],
    ]
}

/// Compiles every kernel ahead of time (FSR takes a moment to compile), so the
/// first source opens quickly. Safe to call from any thread.
pub fn warm_up() {
    for (kernel, fsr) in KERNELS {
        let _ = bytecode(kernel, fsr);
    }
}

const KERNELS: [(&str, bool); 4] =
    [("KERNEL_COPY", false), ("KERNEL_DOWNSCALE", false), ("KERNEL_EASU", true), ("KERNEL_RCAS", true)];

fn compile(device: &ID3D11Device, kernel: &'static str, fsr: bool) -> Result<ID3D11ComputeShader> {
    let bytes = bytecode(kernel, fsr)?;
    let mut shader = None;
    unsafe { device.CreateComputeShader(&bytes, None, Some(&mut shader))? };
    shader.ok_or_else(|| anyhow!("CreateComputeShader returned nothing"))
}

/// Compiled shader bytecode, cached for the life of the process.
fn bytecode(kernel: &'static str, fsr: bool) -> Result<Arc<Vec<u8>>> {
    type Cache = Vec<(&'static str, Arc<Vec<u8>>)>;
    static CACHE: Mutex<Cache> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, code)) = cache.iter().find(|(k, _)| *k == kernel) {
        return Ok(code.clone());
    }
    let code = Arc::new(compile_hlsl(kernel, fsr)?);
    cache.push((kernel, code.clone()));
    Ok(code)
}

fn compile_hlsl(kernel: &str, fsr: bool) -> Result<Vec<u8>> {
    let mut source = String::new();
    if fsr {
        source.push_str("#define A_GPU 1\n#define A_HLSL 1\n");
        source.push_str(FFX_A);
        source.push('\n');
        source.push_str(if kernel == "KERNEL_EASU" { "#define FSR_EASU_F 1\n" } else { "#define FSR_RCAS_F 1\n" });
    }
    source.push_str(&PREVIEW_HLSL.replace("FSR_INCLUDE", if fsr { FFX_FSR1 } else { "" }));

    let kernel_name = format!("{kernel}\0");
    let one = "1\0";
    let defines = [
        D3D_SHADER_MACRO { Name: PCSTR(kernel_name.as_ptr()), Definition: PCSTR(one.as_ptr()) },
        D3D_SHADER_MACRO::default(),
    ];
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR(c"preview.hlsl".as_ptr().cast()),
            Some(defines.as_ptr()),
            None,
            PCSTR(c"Main".as_ptr().cast()),
            PCSTR(c"cs_5_0".as_ptr().cast()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = result {
        let log = errors
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer() as *const u8,
                    b.GetBufferSize(),
                ))
                .into_owned()
            })
            .unwrap_or_default();
        bail!("compiling {kernel} failed: {e}\n{log}");
    }
    let code = code.unwrap();
    Ok(unsafe { std::slice::from_raw_parts(code.GetBufferPointer() as *const u8, code.GetBufferSize()) }.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(w: u32, h: u32, fsr: bool) -> Scaling {
        Scaling::Fit { width: w, height: h, fsr, sharpness: Some(0.2) }
    }

    /// Runs every kernel on the real GPU and checks the colour maths.
    #[test]
    fn gpu_kernels_are_accurate() {
        let mut gpu = Processor::new().expect("D3D11 device");
        // 4×2 source: left half pure red, right half white; checker of black/white below.
        let mut rgba = Vec::new();
        for _y in 0..2 {
            for x in 0..4 {
                rgba.extend_from_slice(if x < 2 { &[255, 0, 0, 255] } else { &[255, 255, 255, 255] });
            }
        }
        // Copy must be bit-exact (and BGRA).
        let (frame, method) = gpu.process_rgba(&rgba, (4, 2), Scaling::Native).unwrap();
        assert_eq!(method, Method::Copy);
        assert_eq!(&frame.bgra[..4], &[0, 0, 255, 255]);
        assert_eq!(&frame.bgra[12..16], &[255, 255, 255, 255]);

        // Downscale 2:1 keeps solid regions exact.
        let (frame, method) = gpu.process_rgba(&rgba, (4, 2), fit(2, 1, false)).unwrap();
        assert_eq!(method, Method::Downscale);
        assert_eq!(frame.bgra, vec![0, 0, 255, 255, 255, 255, 255, 255]);

        // Linear-light averaging: a black/white pair averages to sRGB 188, not 128.
        let bw = [0u8, 0, 0, 255, 255, 255, 255, 255];
        let (frame, _) = gpu.process_rgba(&bw, (2, 1), fit(1, 1, false)).unwrap();
        assert!((frame.bgra[0] as i32 - 188).abs() <= 1, "got {}", frame.bgra[0]);

        // FSR upscales a flat colour to the same flat colour.
        let grey = vec![128u8; 64 * 36 * 4];
        let (frame, method) = gpu.process_rgba(&grey, (64, 36), fit(128, 72, true)).unwrap();
        assert_eq!(method, Method::Fsr);
        assert_eq!((frame.width, frame.height), (128, 72));
        assert!(frame.bgra.chunks(4).all(|p| (p[0] as i32 - 128).abs() <= 1 && p[3] == 255));
    }
}
