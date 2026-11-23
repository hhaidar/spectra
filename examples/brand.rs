//! Regenerates every brand asset from the brand sheet (`brand.png`): the app
//! icon for Windows, macOS and Linux, the Microsoft Store tiles, and the
//! README's logo.
//!
//! ```
//! cargo run --release --example brand
//! ```
//!
//! The icon is drawn here, as shapes, in the sheet's colours and proportions,
//! rather than cut out of the sheet: the sheet's icon is 269 pixels wide, so a
//! cut-out blurs at macOS's 1024 and smears at the taskbar's 16. Small sizes
//! snap the brackets to whole pixels and leave out the glow, which would only
//! muddy them.

use std::{fs, io::Cursor, path::Path};

use xcap::image::{self, ImageFormat, Rgba, RgbaImage};

/// The app's id on Linux (its desktop entry and icon name) and macOS (its bundle).
const APP_ID: &str = "io.github.hhaidar.Spectra";
/// The wordmark ("Spectra") in the sheet's hero, and the centre and width of
/// the mark beside it.
const WORDMARK: (u32, u32, u32, u32) = (826, 200, 1358, 340);
const MARK_CENTRE: (f32, f32) = (703.5, 259.5);
const MARK_WIDTH: f32 = 132.0;

fn main() {
    // Windows: the .exe, window and taskbar icon, at every size Windows asks for.
    let sizes = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256];
    write("assets/spectra.ico", &write_ico(&sizes.map(|s| icon(s, Shape::FullBleed))));

    // Microsoft Store package (MSIX) visual assets.
    let dir = "packaging/windows/Assets";
    let full = |size| icon(size, Shape::FullBleed);
    save(&full(50), &format!("{dir}/StoreLogo.png"));
    save(&full(100), &format!("{dir}/StoreLogo.scale-200.png"));
    save(&full(44), &format!("{dir}/Square44x44Logo.png"));
    save(&full(88), &format!("{dir}/Square44x44Logo.scale-200.png"));
    for size in [16, 24, 32, 48, 256] {
        let image = full(size);
        save(&image, &format!("{dir}/Square44x44Logo.targetsize-{size}.png"));
        save(&image, &format!("{dir}/Square44x44Logo.targetsize-{size}_altform-unplated.png"));
    }
    // Tiles: the icon at two-thirds of the tile's height, on transparency.
    for (scale, suffix) in [(1, ""), (2, ".scale-200")] {
        save(&tile(150 * scale, 150 * scale), &format!("{dir}/Square150x150Logo{suffix}.png"));
        save(&tile(310 * scale, 150 * scale), &format!("{dir}/Wide310x150Logo{suffix}.png"));
    }
    // Partner Center listing logo.
    save(&tile(300, 300), "packaging/windows/store/logo-300.png");

    // macOS: the app bundle's icon, on Apple's grid (an 824 body on a 1024 canvas).
    let mac = |size| icon(size, Shape::MacOs);
    let icns = write_icns(&[
        (*b"icp4", mac(16)),
        (*b"icp5", mac(32)),
        (*b"icp6", mac(64)),
        (*b"ic07", mac(128)),
        (*b"ic08", mac(256)),
        (*b"ic09", mac(512)),
        (*b"ic10", mac(1024)),
        (*b"ic11", mac(32)),
        (*b"ic12", mac(64)),
        (*b"ic13", mac(256)),
        (*b"ic14", mac(512)),
    ]);
    write("packaging/macos/Spectra.icns", &icns);

    // Linux: the hicolor theme's sizes.
    for size in [16, 24, 32, 48, 64, 128, 256, 512] {
        let path = format!("packaging/linux/share/icons/hicolor/{size}x{size}/apps/{APP_ID}.png");
        save(&icon(size, Shape::Linux), &path);
    }

    // README logo: one for GitHub's dark theme, one for its light theme.
    let sheet = image::open("brand.png").expect("brand.png in the repository root").to_rgba8();
    save(&logo(&sheet, 0xf4eff9, 1.0), "docs/logo-dark.png");
    save(&logo(&sheet, 0x17111d, 0.35), "docs/logo-light.png");

    println!("brand assets written");
}

/// How the icon's rounded square sits on its canvas.
#[derive(Clone, Copy)]
enum Shape {
    /// Edge to edge (Windows, the Store).
    FullBleed,
    /// Apple's grid: an 824/1024 body with a soft shadow below it.
    MacOs,
    /// Most of the canvas, with a soft shadow, as the hicolor theme's icons sit.
    Linux,
}

type Rgb = [f32; 3];

/// The sheet's colours.
const VIOLET: u32 = 0x7000f2;
const PLUM: u32 = 0xa4019c;
const CRIMSON: u32 = 0xf2024e;
const NIGHT: u32 = 0x130119;
const BRACKET_TL: u32 = 0xe22ffe;
const BRACKET_BL: u32 = 0xc803fa;
const BRACKET_TR: u32 = 0xfd2b60;
const BRACKET_BR: u32 = 0xfb0451;
const DOT_LEFT: u32 = 0xef12e0;
const DOT_RIGHT: u32 = 0xff1652;
const DOT_TOP: u32 = 0xff5aa6;

/// The icon at `size`×`size` pixels.
fn icon(size: u32, shape: Shape) -> RgbaImage {
    let s = size as f32;
    // The body (the rounded square): its side, top-left corner and shadow.
    let (side, shadow) = match shape {
        Shape::FullBleed => (s, None),
        Shape::MacOs => ((s * 824.0 / 1024.0).round(), Some((0.012 * s, 0.022 * s, 0.4))),
        Shape::Linux => ((s * 0.875).round(), Some((0.016 * s, 0.024 * s, 0.35))),
    };
    let origin = ((s - side) / 2.0).floor();
    let body = Body { x: origin, y: origin, side, radius: side * 0.225 };
    let mark = Mark::new(&body);
    // Glow only where there are pixels enough to show it.
    let glow = ((side - 40.0) / 88.0).clamp(0.0, 1.0);

    let mut image = RgbaImage::new(size, size);
    for (x, y, out) in image.enumerate_pixels_mut() {
        let p = (x as f32 + 0.5, y as f32 + 0.5);
        let (u, v) = ((p.0 - body.x) / side, (p.1 - body.y) / side);
        let body_d = body.distance(p);
        let body_cover = cover(body_d);

        let mut c = background(u, v);
        // A pane of glass: the upper-left half catches a little light.
        let sheen = smoothstep(1.06, 1.0, u + v * 1.1) * 0.055;
        c = mix(c, [1.0; 3], sheen);
        // The rim catches light along the top.
        if side >= 48.0 {
            let rim = (1.0 - (-body_d / (side * 0.012)).clamp(0.0, 1.0)) * (1.0 - v).powi(3) * 0.35;
            c = mix(c, [1.0; 3], rim);
        }

        let bracket_d = mark.bracket_distance(p);
        let dot_d = mark.dot_distance(p);
        let bracket_c = mark.bracket_color(p);
        let dot_c = mark.dot_color(p);
        if glow > 0.0 {
            let g = side * 0.03;
            c = screen(c, bracket_c, (-bracket_d.max(0.0) / g).exp() * 0.5 * glow);
            c = screen(c, dot_c, (-dot_d.max(0.0) / (g * 1.6)).exp() * 0.65 * glow);
        }
        c = mix(c, bracket_c, cover(bracket_d));
        c = mix(c, dot_c, cover(dot_d));

        // The body over its (black) shadow, premultiplied.
        let rgb = c.map(|ch| ch * body_cover);
        let mut alpha = body_cover;
        if let Some((offset, blur, strength)) = shadow {
            let d = body.distance((p.0, p.1 - offset));
            alpha += smoothstep(blur, -blur, d) * strength * (1.0 - body_cover);
        }
        let to_byte = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
        *out = if alpha > 0.0 {
            Rgba([to_byte(rgb[0] / alpha), to_byte(rgb[1] / alpha), to_byte(rgb[2] / alpha), to_byte(alpha)])
        } else {
            Rgba([0, 0, 0, 0])
        };
    }
    image
}

/// The background: the sheet's violet-to-crimson, bright along the top and
/// sides and falling to night in the middle, so the mark stands out.
fn background(u: f32, v: f32) -> Rgb {
    let hue =
        if u < 0.5 { mix(rgb(VIOLET), rgb(PLUM), u / 0.5) } else { mix(rgb(PLUM), rgb(CRIMSON), (u - 0.5) / 0.5) };
    let top = (-v / 0.27).exp();
    let left = 0.8 * (-u / 0.18).exp();
    let right = (1.0 - 0.7 * v) * (-(1.0 - u) / 0.15).exp();
    // Combined as light adds, so there's no crease where one takes over from another.
    let light = 1.0 - (1.0 - top) * (1.0 - left) * (1.0 - right) * 0.96;
    mix(rgb(NIGHT), hue, light)
}

/// The rounded square everything sits on.
struct Body {
    x: f32,
    y: f32,
    side: f32,
    radius: f32,
}

impl Body {
    fn distance(&self, p: (f32, f32)) -> f32 {
        let half = self.side / 2.0;
        rounded_box(p, (self.x + half, self.y + half), (half, half), self.radius)
    }
}

/// The brand mark: four corner brackets around a dot.
struct Mark {
    cx: f32,
    cy: f32,
    /// Half the brackets' outer width.
    half: f32,
    stroke: f32,
    /// An arm's length from the bracket's outer corner.
    arm: f32,
    /// The outer radius of a bracket's corner.
    corner: f32,
    /// How round an arm's end is.
    end: f32,
    dot: f32,
}

impl Mark {
    fn new(body: &Body) -> Self {
        let b = body.side;
        let (cx, cy) = (body.x + b / 2.0, body.y + b / 2.0);
        if b > 40.0 {
            let stroke = b * 0.062;
            return Mark {
                cx,
                cy,
                half: b * 0.315,
                stroke,
                arm: b * 0.2,
                corner: stroke * 1.35,
                end: stroke * 0.3,
                dot: b * 0.106,
            };
        }
        // Small sizes: whole-pixel strokes with edges on pixel boundaries, so
        // the brackets stay crisp instead of blurring across two pixels.
        let stroke = (b * 0.07).round().max(1.0);
        let left = (body.x + b * 0.185).round();
        Mark {
            cx,
            cy,
            half: cx - left,
            stroke,
            arm: (b * 0.2).round().max(stroke + 1.0),
            corner: 0.0,
            end: 0.0,
            dot: (b * 0.115).max(1.6),
        }
    }

    /// Signed distance to the nearest bracket.
    fn bracket_distance(&self, p: (f32, f32)) -> f32 {
        // Fold every bracket onto the top-left one.
        let p = (self.cx - (p.0 - self.cx).abs(), self.cy - (p.1 - self.cy).abs());
        let (left, top) = (self.cx - self.half, self.cy - self.half);
        let w = self.stroke;
        if self.corner == 0.0 {
            // Square corners: two whole-pixel bars.
            let across = rounded_box(p, (left + self.arm / 2.0, top + w / 2.0), (self.arm / 2.0, w / 2.0), 0.0);
            let down = rounded_box(p, (left + w / 2.0, top + self.arm / 2.0), (w / 2.0, self.arm / 2.0), 0.0);
            return across.min(down);
        }
        let rc = self.corner - w / 2.0;
        // The corner is a quarter ring around (ax, ay); past it the arms run
        // straight, rounded at their far ends. Each region takes its own
        // distance, rather than the minimum of clipped shapes, which would
        // leave a hairline seam where they meet.
        let (ax, ay) = (left + w / 2.0 + rc, top + w / 2.0 + rc);
        if p.0 < ax && p.1 < ay {
            return ((p.0 - ax).hypot(p.1 - ay) - rc).abs() - w / 2.0;
        }
        let end = left + self.arm;
        let across = rounded_box(p, ((left + end) / 2.0, top + w / 2.0), ((end - left) / 2.0, w / 2.0), self.end);
        let end = top + self.arm;
        let down = rounded_box(p, (left + w / 2.0, (top + end) / 2.0), (w / 2.0, (end - top) / 2.0), self.end);
        across.min(down)
    }

    fn dot_distance(&self, p: (f32, f32)) -> f32 {
        (p.0 - self.cx).hypot(p.1 - self.cy) - self.dot
    }

    /// Magenta on the left to crimson on the right, lighter at the top.
    fn bracket_color(&self, p: (f32, f32)) -> Rgb {
        let fu = ((p.0 - self.cx) / (2.0 * self.half) + 0.5).clamp(0.0, 1.0);
        let fv = ((p.1 - self.cy) / (2.0 * self.half) + 0.5).clamp(0.0, 1.0);
        let top = mix(rgb(BRACKET_TL), rgb(BRACKET_TR), fu);
        let bottom = mix(rgb(BRACKET_BL), rgb(BRACKET_BR), fu);
        mix(top, bottom, fv)
    }

    /// A glossy sphere: magenta to crimson across, pink light from above.
    fn dot_color(&self, p: (f32, f32)) -> Rgb {
        let (dx, dy) = ((p.0 - self.cx) / self.dot, (p.1 - self.cy) / self.dot);
        let across = ((dx + dy * 0.35) / 2.0 + 0.5).clamp(0.0, 1.0);
        let mut c = mix(rgb(DOT_LEFT), rgb(DOT_RIGHT), across);
        c = mix(c, rgb(DOT_TOP), ((1.0 - dy) / 2.0).clamp(0.0, 1.0).powi(2) * 0.55);
        let spot = (dx + 0.32).hypot(dy + 0.4) / 0.55;
        mix(c, [1.0; 3], (1.0 - spot).clamp(0.0, 1.0).powi(2) * 0.3)
    }
}

/// Signed distance to a box at `center` with half-size `half` and corner radius `r`.
fn rounded_box(p: (f32, f32), center: (f32, f32), half: (f32, f32), r: f32) -> f32 {
    let qx = (p.0 - center.0).abs() - half.0 + r;
    let qy = (p.1 - center.1).abs() - half.1 + r;
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

/// How much of a pixel a shape covers, from the signed distance at its centre.
fn cover(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

/// Light added the way a glow adds it: brightening, never past white.
fn screen(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [0, 1, 2].map(|i| 1.0 - (1.0 - a[i]) * (1.0 - b[i] * t))
}

fn rgb(hex: u32) -> Rgb {
    [(hex >> 16) & 0xff, (hex >> 8) & 0xff, hex & 0xff].map(|c| c as f32 / 255.0)
}

/// The mark beside the sheet's wordmark, on transparency, the letters in
/// `text` and the mark's glow at `glow`. The letters are lifted from the
/// sheet's hero by their green channel: they're white, and everything behind
/// them is violet, crimson or near-black, with almost no green.
fn logo(sheet: &RgbaImage, text: u32, glow: f32) -> RgbaImage {
    // The canvas, in sheet coordinates: the lockup with room for the glow.
    let pad = 36;
    let (x0, y0) = (MARK_CENTRE.0 as u32 - MARK_WIDTH as u32 / 2 - pad, WORDMARK.1 - pad);
    let (x1, y1) = (WORDMARK.2 + pad, WORDMARK.3 + pad);
    let side = MARK_WIDTH / 0.63;
    let body = Body {
        x: MARK_CENTRE.0 - side / 2.0 - x0 as f32,
        y: MARK_CENTRE.1 - side / 2.0 - y0 as f32,
        side,
        radius: 0.0,
    };
    let mark = Mark::new(&body);
    let g = side * 0.03;
    let text = rgb(text);

    let mut image = RgbaImage::new(x1 - x0, y1 - y0);
    for (x, y, out) in image.enumerate_pixels_mut() {
        let p = (x as f32 + 0.5, y as f32 + 0.5);
        let (bracket_d, dot_d) = (mark.bracket_distance(p), mark.dot_distance(p));
        let (bracket_c, dot_c) = (mark.bracket_color(p), mark.dot_color(p));
        let (sx, sy) = (x + x0, y + y0);
        let (wx0, wy0, wx1, wy1) = WORDMARK;
        let letters = if (wx0..wx1).contains(&sx) && (wy0..wy1).contains(&sy) {
            ((sheet.get_pixel(sx, sy)[1] as f32 - 12.0) / 220.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Painted back to front, premultiplied.
        let mut c = [0.0; 3];
        let mut a = 0.0;
        for (colour, alpha) in [
            (bracket_c, (-bracket_d.max(0.0) / g).exp() * 0.5 * glow),
            (dot_c, (-dot_d.max(0.0) / (g * 1.6)).exp() * 0.65 * glow),
            (bracket_c, cover(bracket_d)),
            (dot_c, cover(dot_d)),
            (text, letters),
        ] {
            c = [0, 1, 2].map(|i| colour[i] * alpha + c[i] * (1.0 - alpha));
            a = alpha + a * (1.0 - alpha);
        }
        let to_byte = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
        *out = if a > 0.0 {
            Rgba([to_byte(c[0] / a), to_byte(c[1] / a), to_byte(c[2] / a), to_byte(a)])
        } else {
            Rgba([0; 4])
        };
    }
    image
}

/// The icon centred on a transparent `w`×`h` tile, two-thirds of its height.
fn tile(w: u32, h: u32) -> RgbaImage {
    let size = h * 2 / 3;
    let mark = icon(size, Shape::FullBleed);
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

/// An .icns holding PNG images, each under its type code.
fn write_icns(images: &[([u8; 4], RgbaImage)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, image) in images {
        let png = png_bytes(image);
        body.extend_from_slice(kind);
        body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
        body.extend_from_slice(&png);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
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
