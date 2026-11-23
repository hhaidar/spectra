//! CPU preview pipeline, used where the Direct3D one isn't available (Linux,
//! macOS). Same interface as `gpu.rs`, without FSR: frames are shown natively
//! or downscaled in linear light.

use std::sync::LazyLock;

use anyhow::{Result, anyhow};

use crate::{
    encoder::PixelOrder,
    recorder::RawFrame,
    scaling::{Method, Scaling, plan},
    session::PreviewFrame,
};

/// Nothing to compile ahead of time on the CPU path.
pub fn warm_up() {}

pub struct Processor {
    /// The newest frame, kept for screenshots and recording.
    newest: Option<(Vec<u8>, (u32, u32))>,
}

impl Processor {
    pub fn new() -> Result<Self> {
        Ok(Self { newest: None })
    }

    pub fn process_rgba(&mut self, rgba: &[u8], size: (u32, u32), scaling: Scaling) -> Result<(PreviewFrame, Method)> {
        let (dst, method, _) = plan(size, scaling, false);
        let bgra = match method {
            Method::Downscale => downscale(rgba, size, dst),
            _ => rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
        };
        self.newest = Some((rgba.to_vec(), size));
        let (width, height) = if method == Method::Downscale { dst } else { size };
        Ok((PreviewFrame { width, height, bgra }, if method == Method::Downscale { method } else { Method::Copy }))
    }

    /// The newest frame at full resolution.
    pub fn read_full(&mut self) -> Result<RawFrame> {
        let (data, size) = self.newest.clone().ok_or_else(|| anyhow!("no frame yet"))?;
        Ok(RawFrame { data, size, order: PixelOrder::Rgba })
    }
}

static SRGB_TO_LINEAR: LazyLock<[f32; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    })
});

const LINEAR_STEPS: usize = 4096;
static LINEAR_TO_SRGB: LazyLock<[u8; LINEAR_STEPS + 1]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f32 / LINEAR_STEPS as f32;
        let s = if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (s * 255.0).round().clamp(0.0, 255.0) as u8
    })
});

/// Box filter in linear light: each output pixel averages the source pixels
/// it covers, so fine detail keeps its true brightness. Output is BGRA.
fn downscale(rgba: &[u8], src: (u32, u32), dst: (u32, u32)) -> Vec<u8> {
    let (sw, sh, dw, dh) = (src.0 as usize, src.1 as usize, dst.0 as usize, dst.1 as usize);
    let span = |i: usize, s: usize, d: usize| (i * s / d, ((i + 1) * s / d).max(i * s / d + 1).min(s));
    let columns: Vec<(usize, usize)> = (0..dw).map(|x| span(x, sw, dw)).collect();
    let to_linear = &*SRGB_TO_LINEAR;
    let to_srgb = |v: f32| LINEAR_TO_SRGB[((v * LINEAR_STEPS as f32).round() as usize).min(LINEAR_STEPS)];
    let mut out = Vec::with_capacity(dw * dh * 4);
    for y in 0..dh {
        let (y0, y1) = span(y, sh, dh);
        for &(x0, x1) in &columns {
            let mut sum = [0f32; 3];
            for row in y0..y1 {
                for px in rgba[(row * sw + x0) * 4..(row * sw + x1) * 4].chunks_exact(4) {
                    sum[0] += to_linear[px[0] as usize];
                    sum[1] += to_linear[px[1] as usize];
                    sum[2] += to_linear[px[2] as usize];
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            out.extend_from_slice(&[to_srgb(sum[2] / n), to_srgb(sum[1] / n), to_srgb(sum[0] / n), 255]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(w: u32, h: u32) -> Scaling {
        Scaling::Fit { width: w, height: h, fsr: true, sharpness: None }
    }

    #[test]
    fn cpu_pipeline_is_accurate() {
        let mut cpu = Processor::new().unwrap();
        // Left half pure red, right half white.
        let mut rgba = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                rgba.extend_from_slice(if x < 2 { &[255, 0, 0, 255] } else { &[255, 255, 255, 255] });
            }
        }
        let (frame, method) = cpu.process_rgba(&rgba, (4, 2), Scaling::Native).unwrap();
        assert_eq!(method, Method::Copy);
        assert_eq!(&frame.bgra[..4], &[0, 0, 255, 255]);

        let (frame, method) = cpu.process_rgba(&rgba, (4, 2), fit(2, 1)).unwrap();
        assert_eq!(method, Method::Downscale);
        assert_eq!(frame.bgra, vec![0, 0, 255, 255, 255, 255, 255, 255]);

        // Linear-light averaging: black + white → sRGB 188, not 128.
        let (frame, _) = cpu.process_rgba(&[0, 0, 0, 255, 255, 255, 255, 255], (2, 1), fit(1, 1)).unwrap();
        assert!((frame.bgra[0] as i32 - 188).abs() <= 1, "got {}", frame.bgra[0]);

        // No FSR on the CPU path: a larger area shows the source as is.
        let (frame, method) = cpu.process_rgba(&rgba, (4, 2), fit(8, 4)).unwrap();
        assert_eq!((method, frame.width), (Method::Copy, 4));

        assert_eq!(cpu.read_full().unwrap().size, (4, 2));
    }
}
