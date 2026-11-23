# Spectra

Spectra shows a live, low-latency preview of a desktop, a window, a capture
card or a camera, and records it or takes screenshots.

- **Sources**: desktops and windows on every platform; capture cards and
  cameras on Windows (through ffmpeg's DirectShow input).
- **Preview**: fitted to the window at its exact pixel size, or 1:1. On
  Windows, frames come from Windows Graphics Capture and are scaled on the
  GPU, with optional AMD FSR 1.0 upscaling; elsewhere they are scaled on the
  CPU. Downscaling averages in linear light, so colours stay accurate.
- **Recording**: H.264, HEVC or AV1 through ffmpeg, on the GPU encoder when
  there is one, at a constant frame rate. **Screenshots** are full-resolution
  PNGs.
- Spectra remembers the last source and its settings.

## Building

Rust (stable) and, for recording and capture devices, ffmpeg on `PATH`.

```
cargo run --release
```

On Linux, GPUI and xcap need the system libraries listed in
[`.github/workflows/build.yml`](.github/workflows/build.yml).

## Licences

AMD FidelityFX Super Resolution 1.0 (`src/shaders/ffx_*.h`) is © Advanced
Micro Devices, under the MIT licence in
[`src/shaders/LICENSE-FSR.txt`](src/shaders/LICENSE-FSR.txt).
