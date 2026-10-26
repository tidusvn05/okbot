//! What a chart draws, in logical pixels: the layout's output and the rasterizer's input. Tests
//! snapshot it instead of PNG bytes.

use std::fmt;

/// An sRGB colour, `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb(pub(crate) u32);

impl Rgb {
    pub(crate) fn parts(self) -> (u8, u8, u8) {
        let [_, r, g, b] = self.0.to_be_bytes();
        (r, g, b)
    }
}

/// Where a text's `x` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    Start,
    Middle,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Item {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: Rgb,
    },
    /// `y` is the baseline.
    Text {
        x: f32,
        y: f32,
        size: f32,
        bold: bool,
        anchor: Anchor,
        color: Rgb,
        text: String,
    },
    Line {
        points: Vec<(f32, f32)>,
        width: f32,
        color: Rgb,
        dash: Option<[f32; 2]>,
    },
    Dot {
        x: f32,
        y: f32,
        r: f32,
        color: Rgb,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Scene {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) items: Vec<Item>,
}

/// One decimal is enough to review a layout.
struct N(f32);

impl fmt::Display for N {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = (self.0 * 10.0).round() / 10.0;
        if v.fract() == 0.0 {
            write!(f, "{v:.0}")
        } else {
            write!(f, "{v:.1}")
        }
    }
}

impl fmt::Display for Scene {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scene {}x{}", N(self.width), N(self.height))?;
        for item in &self.items {
            match item {
                Item::Rect {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    color,
                } => writeln!(
                    f,
                    "rect {},{} {}x{} r{} #{:06x}",
                    N(*x),
                    N(*y),
                    N(*w),
                    N(*h),
                    N(*radius),
                    color.0
                )?,
                Item::Text {
                    x,
                    y,
                    size,
                    bold,
                    anchor,
                    color,
                    text,
                } => writeln!(
                    f,
                    "text {},{} {}{} {:?} #{:06x} {text:?}",
                    N(*x),
                    N(*y),
                    N(*size),
                    if *bold { " bold" } else { "" },
                    anchor,
                    color.0
                )?,
                Item::Line {
                    points,
                    width,
                    color,
                    dash,
                } => {
                    let points: Vec<String> = points
                        .iter()
                        .map(|(x, y)| format!("{},{}", N(*x), N(*y)))
                        .collect();
                    writeln!(
                        f,
                        "line w{} #{:06x}{} {}",
                        N(*width),
                        color.0,
                        dash.map(|[a, b]| format!(" dash {}/{}", N(a), N(b)))
                            .unwrap_or_default(),
                        points.join(" ")
                    )?;
                }
                Item::Dot { x, y, r, color } => {
                    writeln!(f, "dot {},{} r{} #{:06x}", N(*x), N(*y), N(*r), color.0)?;
                }
            }
        }
        Ok(())
    }
}
