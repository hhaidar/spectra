//! Regenerates every brand asset from `brand.png` (the brand sheet):
//! the Windows icon, the Microsoft Store tiles and the README banner.
//!
//! ```
//! cargo run --release --example brand
//! ```

use std::{fs, io::Cursor, path::Path};

use xcap::image::{self, ImageFormat, Rgba, RgbaImage, imageops::FilterType};

/// Where the app icon sits on the brand sheet, and its corner radius (pixels).
const ICON: (u32, u32, u32, u32) = (267, 134, 269, 248);
const ICON_RADIUS: f32 = 64.0;
/// The hero banner across the top of the sheet.
const BANNER: (u32, u32, u32, u32) = (0, 0, 1536, 474);

fn main() {
    let sheet = image::open("brand.png").expect("brand.png in the repository root").to_rgba8();
    let (x, y, w, h) = ICON;
    let icon_art = image::imageops::crop_imm(&sheet, x, y, w, h).to_image();

    // Windows: the .exe / window icon.
    let sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
    let ico = write_ico(&sizes.map(|s| icon(&icon_art, s)));
    write("assets/spectra.ico", &ico);

    // Microsoft Store package (MSIX) visual assets.
    let dir = "packaging/windows/Assets";
    save(&icon(&icon_art, 50), &format!("{dir}/StoreLogo.png"));
    save(&icon(&icon_art, 100), &format!("{dir}/StoreLogo.scale-200.png"));
    save(&icon(&icon_art, 44), &format!("{dir}/Square44x44Logo.png"));
    save(&icon(&icon_art, 88), &format!("{dir}/Square44x44Logo.scale-200.png"));
    for size in [16, 24, 32, 48, 256] {
        let image = icon(&icon_art, size);
        save(&image, &format!("{dir}/Square44x44Logo.targetsize-{size}.png"));
        save(&image, &format!("{dir}/Square44x44Logo.targetsize-{size}_altform-unplated.png"));
    }
    // Tiles: the icon at two-thirds of the tile's height, on transparency.
    for (scale, suffix) in [(1, ""), (2, ".scale-200")] {
        save(&tile(&icon_art, 150 * scale, 150 * scale), &format!("{dir}/Square150x150Logo{suffix}.png"));
        save(&tile(&icon_art, 310 * scale, 150 * scale), &format!("{dir}/Wide310x150Logo{suffix}.png"));
    }
    // Partner Center listing logo.
    save(&tile(&icon_art, 300, 300), "packaging/windows/store/logo-300.png");

    // README banner.
    let (x, y, w, h) = BANNER;
    let banner = image::imageops::crop_imm(&sheet, x, y, w, h).to_image();
    save(&image::imageops::resize(&banner, 1280, 1280 * h / w, FilterType::Lanczos3), "docs/banner.png");

    println!("brand assets written");
}

/// The app icon at `size`×`size`, its rounded corners transparent.
fn icon(art: &RgbaImage, size: u32) -> RgbaImage {
    let mut image = image::imageops::resize(art, size, size, FilterType::Lanczos3);
    // The artwork isn't quite square, so its corners become slightly elliptical.
    let (aw, ah) = (art.width() as f32, art.height() as f32);
    let (sx, sy) = (size as f32 / aw, size as f32 / ah);
    // Inset by a source pixel to drop the background that bleeds into the rim.
    let inset = (1.0 * sx, 1.0 * sy);
    let (rx, ry) = (ICON_RADIUS * sx, ICON_RADIUS * sy);
    let (w, h) = (size as f32 - 2.0 * inset.0, size as f32 - 2.0 * inset.1);
    for (x, y, px) in image.enumerate_pixels_mut() {
        // 4×4 supersampled coverage of the rounded rectangle.
        let mut covered = 0;
        for sy in 0..4 {
            for sx in 0..4 {
                let px_x = x as f32 + (sx as f32 + 0.5) / 4.0 - inset.0;
                let px_y = y as f32 + (sy as f32 + 0.5) / 4.0 - inset.1;
                if inside_rounded(px_x, px_y, w, h, rx, ry) {
                    covered += 1;
                }
            }
        }
        px[3] = ((px[3] as u32 * covered) / 16) as u8;
    }
    image
}

fn inside_rounded(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> bool {
    if x < 0.0 || y < 0.0 || x > w || y > h {
        return false;
    }
    let cx = if x < rx {
        rx
    } else if x > w - rx {
        w - rx
    } else {
        return true;
    };
    let cy = if y < ry {
        ry
    } else if y > h - ry {
        h - ry
    } else {
        return true;
    };
    let (dx, dy) = ((x - cx) / rx, (y - cy) / ry);
    dx * dx + dy * dy <= 1.0
}

/// The icon centred on a transparent `w`×`h` tile, two-thirds of its height.
fn tile(art: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let size = h * 2 / 3;
    let mark = icon(art, size);
    let mut canvas = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));
    image::imageops::overlay(&mut canvas, &mark, ((w - size) / 2) as i64, ((h - size) / 2) as i64);
    canvas
}

/// An .ico holding PNG-compressed images (supported since Windows Vista).
fn write_ico(images: &[RgbaImage]) -> Vec<u8> {
    let pngs: Vec<Vec<u8>> = images.iter().map(png_bytes).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (image, png) in images.iter().zip(&pngs) {
        let dim = |v: u32| if v >= 256 { 0 } else { v as u8 };
        out.extend_from_slice(&[dim(image.width()), dim(image.height()), 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in pngs {
        out.extend_from_slice(&png);
    }
    out
}

fn png_bytes(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).expect("PNG encoding");
    bytes
}

fn save(image: &RgbaImage, path: &str) {
    write(path, &png_bytes(image));
}

fn write(path: &str, bytes: &[u8]) {
    if let Some(dir) = Path::new(path).parent() {
        fs::create_dir_all(dir).expect("create output folder");
    }
    fs::write(path, bytes).expect("write asset");
}
