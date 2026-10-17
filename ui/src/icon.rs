//! App icon: a kidney ("nerka" in Polish) filtering a log. Messy lines of
//! text run into it from the left; two short clean lines leave through the
//! notch on the right, one highlighted like a search hit. Drawn in code so
//! there is no image file to ship or decode.
//!
//! Shapes are signed-distance functions in unit coordinates (`-1..1`,
//! y down; negative inside). Each pixel is supersampled for anti-aliasing.

use eframe::egui::IconData;

const SIZE: u32 = 256;
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

pub fn icon() -> IconData {
    IconData { rgba: render(SIZE), width: SIZE, height: SIZE }
}

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

fn render(size: u32) -> Vec<u8> {
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
        let i = icon();
        assert_eq!(i.rgba.len(), (i.width * i.height * 4) as usize);
        assert_eq!(i.rgba[3], 0);
        let mid = (i.height / 2 * i.width + i.width / 2 - 20) * 4;
        assert_eq!(i.rgba[mid as usize + 3], 255);
    }
}
