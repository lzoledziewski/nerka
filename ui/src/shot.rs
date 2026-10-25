//! Screenshots to the clipboard (owner, 2026-10-25): of the whole window,
//! or of a part of it (the plot).
//!
//! egui reads the rendered pixels after the next frame and hands them to a
//! callback outside the frame; the image waits in the context's memory
//! until the main window's next frame copies it ([`take`]).

use eframe::egui::{self, ColorImage, Context, Id, Rect};
use std::sync::Arc;

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
        c.data_mut(|d| d.insert_temp(slot(), Arc::new(image)));
        c.request_repaint_of(egui::ViewportId::ROOT);
    });
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
