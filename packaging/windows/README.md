# Microsoft Store

Spectra ships to the Store as an MSIX package: a full-trust desktop app with
its icon tiles in `Assets/` (generated from `brand.png` by
`cargo run --release --example brand`).

## Once: reserve the app in Partner Center

1. In [Partner Center](https://partner.microsoft.com/dashboard), create a new
   app and reserve the name **Spectra**.
2. Under **Product management → Product identity**, note:
   - `Package/Identity/Name` → `MSIX_IDENTITY_NAME`
   - `Package/Identity/Publisher` (`CN=…`) → `MSIX_PUBLISHER`
   - `Package/Properties/PublisherDisplayName` → `MSIX_PUBLISHER_DISPLAY_NAME`
3. Add those three as repository **variables** (Settings → Secrets and
   variables → Actions → Variables), so CI builds packages with that identity.
   Locally, pass them as `-IdentityName`, `-Publisher` and
   `-PublisherDisplayName`, or set the same environment variables.

## Each release

1. Bump `version` in `Cargo.toml` (the package version is it plus `.0`).
2. Build the packages. CI's **Build** workflow does it on every push to
   `master`: download the `spectra-msixbundle` artifact. Locally, on each
   architecture:

   ```powershell
   packaging\windows\build-msix.ps1 -FFmpeg C:\path\to\ffmpeg.exe
   packaging\windows\build-msix.ps1 -Bundle target\msix\Spectra_*_x64.msix, target\msix\Spectra_*_arm64.msix
   ```

3. In Partner Center, start a submission and upload the `.msixbundle` under
   **Packages**. The Store signs it; it doesn't need signing here.

## The listing

- **Description**: "Preview, record and screenshot your desktop, a window, a
  capture card or a camera", and what the README says.
- **Screenshots**: `docs/screenshot.png` (1600×872), plus others at 1366×768
  or larger.
- **Logo**: `store/logo-300.png`.
- **Restricted capability `runFullTrust`**: Partner Center asks why. Spectra
  is a desktop app (Win32) that captures the screen with Windows Graphics
  Capture and records through ffmpeg.

## ffmpeg

Recording, capture cards and cameras need ffmpeg. Spectra uses an
`ffmpeg.exe` next to `spectra.exe` before the one on `PATH`, so the Store
package should ship one (`-FFmpeg`). Ship a build you're allowed to
redistribute: LGPL builds can go in as is, with their licence and source
offer; GPL builds (with x264/x265, like gyan.dev's "full" build) put the
whole package under the GPL's terms. NVENC/AMF/QSV encoding works with LGPL
builds; the CPU encoders x264 and x265 are GPL.

## Trying a package locally

Unsigned packages don't install. With Developer Mode on (Settings → System →
For developers), register the unpacked layout instead:

```powershell
Add-AppxPackage -Register target\msix\layout-x64\AppxManifest.xml
```

Before submitting, the Windows App Certification Kit (in the Windows SDK)
runs the Store's checks on a package.
