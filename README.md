# Spectra

Preview, record and screenshot your desktop, a window, a capture card or a camera.

![Spectra previewing a test pattern, upscaled with FSR](docs/screenshot.png)

## Run

Needs [Rust](https://rustup.rs) and, for recording and capture devices, [ffmpeg](https://ffmpeg.org) on your `PATH`.

```
cargo run --release
```

FSR 1.0 shaders in `src/shaders` are © AMD, [MIT licence](src/shaders/LICENSE-FSR.txt).
