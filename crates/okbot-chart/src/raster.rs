//! Draws a [`Scene`] into a PNG on a white background.

use okbot_core::ChartError;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Rect, Stroke, StrokeDash,
    Transform,
};

use crate::font::Fonts;
use crate::scene::{Anchor, Item, Rgb, Scene};

fn paint(color: Rgb) -> Paint<'static> {
    let (r, g, b) = color.parts();
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    paint
}

/// A rectangle with rounded corners (the radius is clamped to half the shorter side).
fn rounded(x: f32, y: f32, w: f32, h: f32, radius: f32) -> Option<Path> {
    let r = radius.min(w / 2.0).min(h / 2.0).max(0.0);
    if r == 0.0 {
        return Rect::from_xywh(x, y, w, h).map(PathBuilder::from_rect);
    }
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

/// Encodes `scene` at `scale` device pixels per logical pixel; returns the PNG and its size.
pub(crate) fn png(
    scene: &Scene,
    fonts: &Fonts<'_>,
    scale: f32,
) -> Result<(Vec<u8>, u32, u32), ChartError> {
    let width = (scene.width * scale).ceil() as u32;
    let height = (scene.height * scale).ceil() as u32;
    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| ChartError::Failed(format!("no {width}x{height} image")))?;
    pixmap.fill(Color::WHITE);
    let t = Transform::from_scale(scale, scale);
    for item in &scene.items {
        match item {
            Item::Rect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                if let Some(path) = rounded(*x, *y, *w, *h, *radius) {
                    pixmap.fill_path(&path, &paint(*color), FillRule::Winding, t, None);
                }
            }
            Item::Text {
                x,
                y,
                size,
                bold,
                anchor,
                color,
                text,
            } => {
                let w = fonts.width(text, *size, *bold);
                let x = match anchor {
                    Anchor::Start => *x,
                    Anchor::Middle => x - w / 2.0,
                    Anchor::End => x - w,
                };
                let paint = paint(*color);
                let embolden = Stroke {
                    width: size * 0.045,
                    line_join: LineJoin::Round,
                    ..Stroke::default()
                };
                for glyph in fonts.glyphs(text, x, *y, *size, *bold) {
                    pixmap.fill_path(&glyph.path, &paint, FillRule::Winding, t, None);
                    if glyph.embolden {
                        pixmap.stroke_path(&glyph.path, &paint, &embolden, t, None);
                    }
                }
            }
            Item::Line {
                points,
                width,
                color,
                dash,
            } => {
                let mut p = PathBuilder::new();
                for (i, (x, y)) in points.iter().enumerate() {
                    if i == 0 {
                        p.move_to(*x, *y);
                    } else {
                        p.line_to(*x, *y);
                    }
                }
                // A single point draws as a dot.
                if points.len() == 1 {
                    p.line_to(points[0].0 + 0.01, points[0].1);
                }
                let Some(path) = p.finish() else { continue };
                let stroke = Stroke {
                    width: *width,
                    line_cap: LineCap::Round,
                    line_join: LineJoin::Round,
                    dash: dash.and_then(|d| StrokeDash::new(d.to_vec(), 0.0)),
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint(*color), &stroke, t, None);
            }
            Item::Dot { x, y, r, color } => {
                if let Some(path) = PathBuilder::from_circle(*x, *y, *r) {
                    pixmap.fill_path(&path, &paint(*color), FillRule::Winding, t, None);
                }
            }
        }
    }
    let png = pixmap
        .encode_png()
        .map_err(|e| ChartError::Failed(e.to_string()))?;
    Ok((png, width, height))
}
