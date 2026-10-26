//! The embedded fonts and the fallback chain: measuring, fitting and outlining text.
//!
//! Vietnamese has a precomposed code point for every letter and Japanese needs no contextual
//! shaping when written horizontally, so text is drawn glyph by glyph from each character's first
//! font in the chain, without a shaping engine. Strings are NFC before they get here.

use std::io::Read as _;
use std::sync::OnceLock;

use okbot_core::ChartError;
use tiny_skia::{Path, PathBuilder};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

static LATIN_400: &[u8] = include_bytes!("../fonts/latin-400.ttf.z");
static LATIN_700: &[u8] = include_bytes!("../fonts/latin-700.ttf.z");
#[cfg(feature = "ja")]
static JA_400: &[u8] = include_bytes!("../fonts/ja-400.ttf.z");

/// The embedded fonts, inflated on first use.
struct Builtin {
    latin: Vec<u8>,
    latin_bold: Vec<u8>,
    ja: Option<Vec<u8>>,
}

fn inflate(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .expect("the embedded fonts are valid zlib streams (fonts/build.py)");
    out
}

fn builtin() -> &'static Builtin {
    static FONTS: OnceLock<Builtin> = OnceLock::new();
    FONTS.get_or_init(|| Builtin {
        latin: inflate(LATIN_400),
        latin_bold: inflate(LATIN_700),
        #[cfg(feature = "ja")]
        ja: Some(inflate(JA_400)),
        #[cfg(not(feature = "ja"))]
        ja: None,
    })
}

/// Checks that `data` is a font okbot can draw with.
pub(crate) fn check(data: &[u8]) -> Result<(), ChartError> {
    Face::parse(data, 0)
        .map(drop)
        .map_err(|e| ChartError::Failed(format!("not a font: {e}")))
}

/// One font of a chain.
struct Slot<'a> {
    face: Face<'a>,
    /// Drawn bolder by also stroking the outline (fonts without a bold weight).
    embolden: bool,
}

/// A glyph to draw: its outline in pixels and whether to stroke it too.
pub(crate) struct Glyph {
    pub(crate) path: Path,
    pub(crate) embolden: bool,
}

/// The regular and bold fallback chains: built-in Latin and Vietnamese, built-in Japanese, then
/// the extra fonts in order.
pub(crate) struct Fonts<'a> {
    regular: Vec<Slot<'a>>,
    bold: Vec<Slot<'a>>,
}

fn parse(data: &[u8]) -> Result<Face<'_>, ChartError> {
    Face::parse(data, 0).map_err(|e| ChartError::Failed(format!("font: {e}")))
}

impl<'a> Fonts<'a> {
    pub(crate) fn new(extra: &'a [Vec<u8>]) -> Result<Self, ChartError> {
        let b = builtin();
        let mut regular = vec![Slot {
            face: parse(&b.latin)?,
            embolden: false,
        }];
        let mut bold = vec![Slot {
            face: parse(&b.latin_bold)?,
            embolden: false,
        }];
        for data in
            b.ja.iter()
                .map(Vec::as_slice)
                .chain(extra.iter().map(Vec::as_slice))
        {
            regular.push(Slot {
                face: parse(data)?,
                embolden: false,
            });
            bold.push(Slot {
                face: parse(data)?,
                embolden: true,
            });
        }
        Ok(Self { regular, bold })
    }

    fn chain(&self, bold: bool) -> &[Slot<'a>] {
        if bold { &self.bold } else { &self.regular }
    }

    fn glyph(&self, c: char, bold: bool) -> Option<(&Slot<'a>, GlyphId)> {
        self.chain(bold)
            .iter()
            .find_map(|s| s.face.glyph_index(c).map(|g| (s, g)))
    }

    /// Whether some font draws `c`.
    pub(crate) fn has(&self, c: char) -> bool {
        self.glyph(c, false).is_some()
    }

    fn advance(slot: &Slot<'_>, glyph: GlyphId, size: f32) -> f32 {
        f32::from(slot.face.glyph_hor_advance(glyph).unwrap_or(0)) * size
            / f32::from(slot.face.units_per_em())
    }

    /// The width of `text` at `size` pixels.
    pub(crate) fn width(&self, text: &str, size: f32, bold: bool) -> f32 {
        text.chars()
            .filter_map(|c| self.glyph(c, bold))
            .map(|(s, g)| Self::advance(s, g, size))
            .sum()
    }

    /// `text`, cut with `…` where it would be wider than `max`.
    pub(crate) fn fit(&self, text: &str, size: f32, bold: bool, max: f32) -> String {
        if self.width(text, size, bold) <= max {
            return text.to_string();
        }
        let ellipsis = self.width("…", size, bold);
        let mut width = 0.0;
        let mut out = String::new();
        for c in text.chars() {
            let w = self
                .glyph(c, bold)
                .map_or(0.0, |(s, g)| Self::advance(s, g, size));
            if width + w + ellipsis > max {
                break;
            }
            width += w;
            out.push(c);
        }
        format!("{}…", out.trim_end())
    }

    /// The outlines of `text` with its baseline starting at (`x`, `y`).
    pub(crate) fn glyphs(&self, text: &str, x: f32, y: f32, size: f32, bold: bool) -> Vec<Glyph> {
        let mut out = Vec::new();
        let mut x = x;
        for c in text.chars() {
            let Some((slot, glyph)) = self.glyph(c, bold) else {
                continue;
            };
            let scale = size / f32::from(slot.face.units_per_em());
            let mut pen = Pen {
                path: PathBuilder::new(),
                x,
                y,
                scale,
            };
            if slot.face.outline_glyph(glyph, &mut pen).is_some()
                && let Some(path) = pen.path.finish()
            {
                out.push(Glyph {
                    path,
                    embolden: slot.embolden,
                });
            }
            x += Self::advance(slot, glyph, size);
        }
        out
    }
}

/// Turns a glyph's outline (font units, y up) into a path in pixels (y down).
struct Pen {
    path: PathBuilder,
    x: f32,
    y: f32,
    scale: f32,
}

impl Pen {
    fn at(&self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x * self.scale, self.y - y * self.scale)
    }
}

impl OutlineBuilder for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.move_to(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.path.line_to(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.at(x1, y1);
        let (x, y) = self.at(x, y);
        self.path.quad_to(x1, y1, x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.at(x1, y1);
        let (x2, y2) = self.at(x2, y2);
        let (x, y) = self.at(x, y);
        self.path.cubic_to(x1, y1, x2, y2, x, y);
    }

    fn close(&mut self) {
        self.path.close();
    }
}
