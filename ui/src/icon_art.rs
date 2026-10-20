//! App icon: a kidney ("nerka" in Polish) filtering a log. Messy lines of
//! text run into it from the left; two short clean lines leave through the
//! notch on the right, one highlighted like a search hit. Drawn in code so
//! there is no image file to ship or decode.
//!
//! Shapes are signed-distance functions in unit coordinates (`-1..1`,
//! y down; negative inside). Each pixel is supersampled for anti-aliasing.
//!
//! No egui here: `build.rs` includes this file to render the `.ico` that is
//! embedded in the Windows `.exe`, so the file icon always matches the
//! window icon. `icon.rs` wraps it for egui.

/// Size of the window icon, in pixels.
pub const SIZE: u32 = 256;
const SAMPLES: u32 = 4;

const FILL_DARK: [f32; 3] = [150.0, 38.0, 52.0];
const FILL_LIGHT: [f32; 3] = [222.0, 96.0, 104.0];
const OUTLINE: [f32; 3] = [92.0, 18.0, 30.0];
const HIGHLIGHT: [f32; 3] = [255.0, 214.0, 214.0];
/// Dark edge around text lines so they show on light and dark backgrounds.
const INK: [f32; 3] = [48.0, 48.0, 56.0];
const RAW: [[f32; 3]; 2] = [[168.0, 168.0, 178.0], [128.0, 128.0, 140.0]];
const HIT: [f32; 3] = [255.0, 200.0, 56.0];
const CLEAN: [f32; 3] = [130.0, 214.0, 140.0];

const OUTLINE_W: f32 = 0.06;
const LINE_HALF: f32 = 0.075;
const INK_W: f32 = 0.03;

/// Kidney placement: scale and centre of the unit kidney shape.
const K_SCALE: f32 = 0.8;
const K_X: f32 = 0.0;

/// Input text: `(y, x_start, x_end)` word runs; they end under the kidney.
const INPUT: [(f32, f32, f32); 8] = [
    (-0.56, -0.92, -0.66),
    (-0.56, -0.54, -0.20),
    (-0.28, -0.84, -0.30),
    (0.0, -0.92, -0.80),
    (0.0, -0.70, -0.30),
    (0.28, -0.90, -0.62),
    (0.28, -0.50, -0.30),
    (0.56, -0.80, -0.20),
];
/// Filtered output leaving the notch: `(y, x_end, colour)`.
const OUTPUT: [(f32, f32, [f32; 3]); 2] = [(-0.10, 0.90, HIT), (0.16, 0.80, CLEAN)];
const OUTPUT_X0: f32 = 0.12;

/// The kidney: an ellipse with a circular notch (the hilum) cut out of its
/// right side, blended smoothly; unit size, centred at the origin.
fn kidney_unit(x: f32, y: f32) -> f32 {
    let (rx, ry) = (0.60, 0.86);
    let (ex, ey) = ((x + 0.04) / rx, y / ry);
    let body = ((ex * ex + ey * ey).sqrt() - 1.0) * rx.min(ry);
    let (nx, ny) = (x - 0.64, y - 0.02);
    let notch = (nx * nx + ny * ny).sqrt() - 0.36;
    smooth_max(body, -notch, 0.22)
}

fn kidney(x: f32, y: f32) -> f32 {
    kidney_unit((x - K_X) / K_SCALE, y / K_SCALE) * K_SCALE
}

/// Distance from `(x, y)` to the segment `a`–`b`.
fn segment(x: f32, y: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (px, py, dx, dy) = (x - a.0, y - a.1, b.0 - a.0, b.1 - a.1);
    let t = ((px * dx + py * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    let (qx, qy) = (px - dx * t, py - dy * t);
    (qx * qx + qy * qy).sqrt()
}

/// Polynomial smooth maximum: like `a.max(b)` with rounded corners.
fn smooth_max(a: f32, b: f32, k: f32) -> f32 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.max(b) + h * h * k / 4.0
}

/// Colour of a text line at `(x, y)`: fill, or ink at its edge.
fn line_color(x: f32, y: f32) -> Option<[f32; 3]> {
    let inputs = INPUT.iter().enumerate().map(|(i, &(ly, x0, x1))| (ly, x0, x1, RAW[i % 2]));
    let outputs = OUTPUT.iter().map(|&(ly, x1, c)| (ly, OUTPUT_X0, x1, c));
    inputs.chain(outputs).find_map(|(ly, x0, x1, c)| {
        let d = segment(x, y, (x0, ly), (x1, ly)) - LINE_HALF;
        (d < 0.0).then_some(if d > -INK_W { INK } else { c })
    })
}

/// Colour at `(x, y)`, or `None` where the icon is transparent.
fn color_at(x: f32, y: f32) -> Option<[f32; 3]> {
    let d = kidney(x, y);
    if d >= 0.0 {
        return line_color(x, y);
    }
    if d > -OUTLINE_W {
        return Some(OUTLINE);
    }
    // Lit from the top left, with a soft glossy spot.
    let (u, v) = ((x - K_X) / K_SCALE, y / K_SCALE);
    let t = ((0.9 - u * 0.6 - v * 0.8) / 1.8).clamp(0.0, 1.0);
    let base = mix(FILL_DARK, FILL_LIGHT, t);
    let (hx, hy) = ((u + 0.30) / 0.16, (v + 0.42) / 0.26);
    let spot = (1.0 - (hx * hx + hy * hy)).clamp(0.0, 1.0);
    Some(mix(base, HIGHLIGHT, spot * 0.75))
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// The icon at `size` × `size` pixels: RGBA, rows top to bottom,
/// unmultiplied alpha.
pub fn render(size: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut cover, mut col) = (0.0f32, [0.0f32; 3]);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = ((px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32) * 2.0 - 1.0;
                    let y = ((py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32) * 2.0 - 1.0;
                    if let Some(c) = color_at(x, y) {
                        cover += 1.0;
                        for i in 0..3 {
                            col[i] += c[i];
                        }
                    }
                }
            }
            if cover == 0.0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                // Unmultiplied alpha: average colour of the covered samples.
                let a = cover / (SAMPLES * SAMPLES) as f32;
                rgba.extend(col.iter().map(|c| (c / cover).round() as u8));
                rgba.push((a * 255.0).round() as u8);
            }
        }
    }
    rgba
}

/// A Windows `.ico` with the icon at each of `sizes` (≤ 256): 32-bit BMP
/// entries (alpha channel, empty AND mask), readable by every Windows.
#[allow(dead_code)] // used by build.rs
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes.iter().map(|&s| (s, bmp_entry(s))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (s, data) in &images {
        // 256 is written as 0.
        let side = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in images {
        out.extend(data);
    }
    out
}

/// One `.ico` image: BITMAPINFOHEADER (height doubled for the mask), BGRA
/// rows bottom to top, then the 1-bit AND mask (all zero: alpha decides).
#[allow(dead_code)] // used by build.rs
fn bmp_entry(size: u32) -> Vec<u8> {
    let rgba = render(size);
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(size * size * 4 + mask_row * size).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kidney_shape() {
        // Inside: body centre and both poles; outside: the notch and corners.
        assert!(kidney_unit(-0.2, 0.0) < 0.0);
        assert!(kidney_unit(0.0, -0.7) < 0.0);
        assert!(kidney_unit(0.0, 0.7) < 0.0);
        assert!(kidney_unit(0.5, 0.0) > 0.0);
        assert!(kidney_unit(-0.95, -0.95) > 0.0);
    }

    #[test]
    fn text_goes_in_left_and_comes_out_right() {
        // Input lines are visible left of the kidney; each row ends inside it.
        assert_eq!(color_at(-0.6, -0.28), Some(RAW[0]));
        for &(y, _, _) in &INPUT {
            let end = INPUT.iter().filter(|r| r.0 == y).map(|r| r.2).fold(f32::MIN, f32::max);
            assert!(kidney(end + LINE_HALF, y) < 0.0, "row {y} pokes out");
        }
        // Output lines start inside the kidney and show right of it.
        assert!(OUTPUT.iter().all(|&(y, _, _)| kidney(OUTPUT_X0 - LINE_HALF, y) < 0.0));
        assert_eq!(color_at(0.7, -0.10), Some(HIT));
        assert_eq!(color_at(0.7, 0.16), Some(CLEAN));
        assert_eq!(color_at(-0.95, -0.95), None);
    }

    #[test]
    fn icon_is_rgba_with_transparent_corners() {
        let rgba = render(SIZE);
        assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(rgba[3], 0);
        let mid = (SIZE / 2 * SIZE + SIZE / 2 - 20) * 4;
        assert_eq!(rgba[mid as usize + 3], 255);
    }

    #[test]
    fn ico_directory_and_images() {
        let sizes = [16, 32, 256];
        let ico = ico(&sizes);
        let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().unwrap());
        assert_eq!((u16_at(0), u16_at(2), u16_at(4)), (0, 1, 3));
        let mut end = 6 + 16 * 3;
        for (k, &s) in sizes.iter().enumerate() {
            let e = 6 + 16 * k;
            assert_eq!(ico[e] as u32, s % 256, "width of entry {k}");
            assert_eq!((u16_at(e + 4), u16_at(e + 6)), (1, 32));
            let (len, off) = (u32_at(e + 8) as usize, u32_at(e + 12) as usize);
            assert_eq!(off, end, "images follow each other");
            // BITMAPINFOHEADER: 40, width, doubled height.
            assert_eq!((u32_at(off), u32_at(off + 4), u32_at(off + 8)), (40, s, 2 * s));
            assert_eq!(len, 40 + (s * s * 4) as usize + (s.div_ceil(32) * 4 * s) as usize);
            end = off + len;
        }
        assert_eq!(end, ico.len());
    }
}
