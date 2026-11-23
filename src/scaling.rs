//! How a captured frame is scaled for the preview (shared by every platform).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scaling {
    /// Fit inside this many physical pixels.
    Fit {
        width: u32,
        height: u32,
        /// Upscale with FSR when the source is smaller than the area.
        fsr: bool,
        /// RCAS sharpening in stops (0 = strongest); `None` disables RCAS.
        sharpness: Option<f32>,
    },
    /// Full native resolution.
    Native,
}

/// What the pipeline actually did, for display in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Copy,
    Downscale,
    Fsr,
}

/// Decides the output size and method for a source of `src` pixels. `fsr` is
/// whether this platform can run FSR at all.
pub fn plan(src: (u32, u32), scaling: Scaling, fsr_available: bool) -> ((u32, u32), Method, Option<f32>) {
    match scaling {
        Scaling::Native => (src, Method::Copy, None),
        Scaling::Fit { width, height, fsr, sharpness } => {
            let scale = (width as f64 / src.0 as f64).min(height as f64 / src.1 as f64);
            let fitted =
                (((src.0 as f64 * scale).round() as u32).max(1), ((src.1 as f64 * scale).round() as u32).max(1));
            if fitted.0 < src.0 {
                (fitted, Method::Downscale, None)
            } else if fsr && fsr_available && fitted.0 > src.0 {
                // FSR 1 is designed for up to 4× per axis.
                let max = (src.0 * 4, src.1 * 4);
                let size = if fitted.0 > max.0 { max } else { fitted };
                (size, Method::Fsr, sharpness)
            } else {
                // Shown at native size; GPUI stretches it if needed.
                (src, Method::Copy, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(w: u32, h: u32, fsr: bool) -> Scaling {
        Scaling::Fit { width: w, height: h, fsr, sharpness: Some(0.2) }
    }

    #[test]
    fn plans_sizes() {
        assert_eq!(plan((5120, 1440), fit(800, 400, true), true).0, (800, 225));
        assert_eq!(plan((5120, 1440), fit(800, 400, true), true).1, Method::Downscale);
        assert_eq!(plan((1920, 1080), fit(2560, 1600, true), true), ((2560, 1440), Method::Fsr, Some(0.2)));
        assert_eq!(plan((1920, 1080), fit(2560, 1600, false), true), ((1920, 1080), Method::Copy, None));
        assert_eq!(plan((1920, 1080), fit(2560, 1600, true), false), ((1920, 1080), Method::Copy, None));
        assert_eq!(plan((640, 360), fit(9999, 9999, true), true).0, (2560, 1440)); // capped at 4×
        assert_eq!(plan((1920, 1080), Scaling::Native, true).1, Method::Copy);
    }
}
