# Spectra

Spectra previews, records and screenshots video sources: desktops, windows,
capture cards and cameras. It is a Rust app built on GPUI.

## Layout

- `src/main.rs`: the window and its UI.
- `src/session.rs`: one capture thread per open source; it processes frames,
  takes screenshots and feeds recordings.
- `src/source.rs`: listing sources, and capture cards/cameras through ffmpeg.
- `src/wgc.rs`, `src/gpu.rs` (Windows): Windows Graphics Capture and the
  Direct3D 11 preview pipeline, with AMD FSR 1.0 (`src/shaders/`).
- `src/screen.rs`, `src/cpu.rs` (Linux, macOS): xcap capture and the CPU
  preview pipeline.
- `src/encoder.rs`, `src/recorder.rs`: recording through ffmpeg.
- `brand.png`: the brand sheet. `cargo run --release --example brand`
  regenerates everything cut from it (the Windows icon, the Store tiles, the
  README banner); `build.rs` embeds the icon in the Windows executable.

## Checks

Before committing, these pass (CI runs them on Linux, macOS, Windows x64 and
Windows ARM):

```
cargo fmt --check
cargo clippy --release --all-targets --locked -- -D warnings
cargo test --release --locked
```

Tests that need a display, a GPU, ffmpeg or a capture device are `#[ignore]`d;
run them with `cargo test --release -- --ignored --nocapture` when touching
capture, the GPU pipeline or recording. Something you can't run is said
plainly, not claimed as checked.

## Writing

- Code reads like the code around it. Comments explain why, in plain sentences.
- `CHANGELOG.md` gets a line under `## [Unreleased]` for each change a user
  would notice, saying what changed as they see it.
- Formatting is `cargo fmt` (max width 120, `rustfmt.toml`).
- Line endings are LF (`.gitattributes`).

## Commits

- One logical change per commit, with what it needs (code, tests, docs,
  changelog).
- The subject starts with the area and says what changed, as a user sees it:
  `preview: FSR sharpens upscaled capture cards`, `recording: …`, `ci: …`,
  `docs: …`. The body says why, and anything a reviewer should know.
- Never commit build output (`target/`), keys, `.env` files, local data, or
  files git ignores.
- No `Co-Authored-By` trailers, and no mention of AI tools, in commits or
  pull requests.

## Platform notes

- On Windows, run `cargo` from PowerShell: Git Bash's `link` can shadow
  MSVC's linker.
- ffmpeg on `PATH` is needed for recording and for capture cards and cameras.
- `Cargo.lock` holds `libc` at 0.2.189 (and `mio` at 1.2.4, the last release
  that accepts it): `xattr` 0.2.3, which GPUI's HTTP client pulls in, uses
  `libc::ENOATTR`, gone from Linux since libc 0.2.190. A plain `cargo update`
  breaks the Linux build until GPUI moves off `xattr` 0.2.
