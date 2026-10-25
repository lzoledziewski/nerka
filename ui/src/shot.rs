//! Screenshots to the clipboard (owner, 2026-10-25): of the whole window,
//! or of a part of it (the plot).
//!
//! egui reads the rendered pixels after the next frame and hands them to a
//! callback outside the frame; the image waits in the context's memory
//! until the main window's next frame copies it ([`take`]).

use eframe::egui::{self, ColorImage, Context, Id, Rect};
use std::sync::{Arc, Mutex};

fn slot() -> Id {
    Id::unique("nerka-screenshot")
}

/// Ask for a screenshot of the current window (`ctx` is the viewport's), or
/// of `region` of it (in points), to go to the clipboard.
pub fn request(ctx: &Context, region: Option<Rect>) {
    let ppp = ctx.pixels_per_point();
    let c = ctx.clone();
    ctx.request_screenshot(move |image| {
        let image = match region {
            Some(r) => crop(&image, r, ppp),
            None => (*image).clone(),
        };
        deliver(&c, image);
    });
}

/// Ask for `region` of the current window into `into` (a part of a picture
/// put together from several frames), and a frame of this window then.
pub fn request_part(ctx: &Context, region: Rect, into: Arc<Mutex<Option<ColorImage>>>) {
    let ppp = ctx.pixels_per_point();
    let (c, id) = (ctx.clone(), ctx.viewport_id());
    ctx.request_screenshot(move |image| {
        *into.lock().unwrap_or_else(|e| e.into_inner()) = Some(crop(&image, region, ppp));
        c.request_repaint_of(id);
    });
}

/// Hand a finished picture to the main window, which copies it.
pub fn deliver(ctx: &Context, image: ColorImage) {
    ctx.data_mut(|d| d.insert_temp(slot(), Arc::new(image)));
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

/// Pictures of a scrolled area put together: `parts` were taken with the
/// content scrolled by `scrolls` (points, increasing); the content shows in
/// rows `top..bottom` (points) of each, above it a header and below a
/// footer that every part has. The result: the header, all the content
/// once, the footer.
pub fn stitch(parts: &[ColorImage], scrolls: &[f32], top: f32, bottom: f32, ppp: f32) -> Option<ColorImage> {
    let first = parts.first()?;
    let [w, h] = first.size;
    let px = |v: f32| ((v * ppp).round().max(0.0) as usize).min(h);
    let (top_px, bottom_px) = (px(top), px(bottom));
    let mut rows: Vec<&[egui::Color32]> = Vec::new();
    fn row(img: &ColorImage, y: usize) -> &[egui::Color32] {
        let w = img.size[0];
        &img.pixels[y * w..(y + 1) * w]
    }
    rows.extend((0..top_px).map(|y| row(first, y)));
    // Content: from each part, what the ones before didn't show.
    let mut shown = 0.0f32;
    for (img, &s) in parts.iter().zip(scrolls) {
        if img.size != first.size {
            return None;
        }
        let from = px(top + (shown - s).max(0.0)).max(top_px);
        rows.extend((from..bottom_px).map(|y| row(img, y)));
        shown = s + (bottom - top);
    }
    let last = parts.last()?;
    rows.extend((bottom_px..h).map(|y| row(last, y)));
    let pixels: Vec<egui::Color32> = rows.concat();
    Some(ColorImage::new([w, pixels.len() / w.max(1)], pixels))
}

/// A screenshot waiting to be copied, if any.
pub fn take(ctx: &Context) -> Option<Arc<ColorImage>> {
    ctx.data_mut(|d| {
        let image = d.get_temp::<Arc<ColorImage>>(slot());
        d.remove::<Arc<ColorImage>>(slot());
        image
    })
}

/// `region` (points) of `image` (pixels), kept inside the image.
fn crop(image: &ColorImage, region: Rect, ppp: f32) -> ColorImage {
    let [w, h] = image.size;
    let px = |v: f32, max: usize| ((v * ppp).round().max(0.0) as usize).min(max);
    let (x0, y0) = (px(region.min.x, w), px(region.min.y, h));
    let (x1, y1) = (px(region.max.x, w).max(x0), px(region.max.y, h).max(y0));
    image.region_by_pixels([x0, y0], [x1 - x0, y1 - y0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, pos2};

    #[test]
    fn scrolled_parts_are_stitched_without_repeats() {
        // A 1-pixel-wide picture: header row 0, content rows 1..4, footer
        // row 4; content 0..6 seen scrolled by 0 and then by 2 (clamped).
        let part =
            |rows: [u8; 5]| ColorImage::new([1, 5], rows.iter().map(|&v| Color32::from_gray(v)).collect::<Vec<_>>());
        let a = part([200, 10, 11, 12, 250]);
        let b = part([200, 12, 13, 14, 250]);
        let all = stitch(&[a, b], &[0.0, 2.0], 1.0, 4.0, 1.0).unwrap();
        let grays: Vec<u8> = all.pixels.iter().map(|c| c.r()).collect();
        assert_eq!(grays, [200, 10, 11, 12, 13, 14, 250]);
    }

    #[test]
    fn a_region_is_cut_in_pixels_and_kept_inside() {
        let mut image = ColorImage::filled([10, 8], Color32::BLACK);
        image[(4, 2)] = Color32::RED;
        // At 2 pixels per point, points (2, 1)..(4, 3) are pixels (4, 2)..(8, 6).
        let part = crop(&image, Rect::from_min_max(pos2(2.0, 1.0), pos2(4.0, 3.0)), 2.0);
        assert_eq!(part.size, [4, 4]);
        assert_eq!(part[(0, 0)], Color32::RED);
        // Past the edges: cut to the image.
        let part = crop(&image, Rect::from_min_max(pos2(-5.0, 3.0), pos2(50.0, 50.0)), 2.0);
        assert_eq!(part.size, [10, 2]);
    }
}
