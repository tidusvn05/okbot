//! okbot's charts as PNG images (ADR 0016).
//!
//! [`PngCharts`] implements [`okbot_core::ChartRenderer`]: bar, column and line charts laid out
//! for chat (480 logical pixels wide, drawn at 2.5×), with embedded Noto fonts for Vietnamese,
//! English and Japanese (the `ja` feature) and extra fonts for other scripts. Text is NFC
//! normalized; emoji are dropped; a chart with characters no font has fails with
//! [`ChartError::Glyphs`], so the caller shows it as text instead of drawing boxes.
//!
//! Only `tiny-skia` and `ttf-parser` draw: no SVG, no shaping engine, no system fonts.

// Drawing converts between counts, values and pixels everywhere; every value is bounded by the
// chart limits (at most 60 points, values within 1e15, images within 1200 × 1680 pixels).
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

mod font;
mod layout;
mod number;
mod raster;
mod scene;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;

use okbot_core::{Chart, ChartError, ChartKind, ChartRenderer, Image};
use unicode_normalization::UnicodeNormalization as _;

use crate::font::Fonts;
use crate::number::Locale;

/// Device pixels per logical pixel: 480 logical pixels become 1200.
const SCALE: f32 = 2.5;
/// The file name of every chart image.
pub const FILE_NAME: &str = "chart.png";
/// Characters of an image's description.
const MAX_ALT: usize = 400;

/// Draws charts with the embedded fonts and any extra ones.
#[derive(Default)]
pub struct PngCharts {
    extra: Vec<Vec<u8>>,
}

impl std::fmt::Debug for PngCharts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PngCharts")
            .field("extra_fonts", &self.extra.len())
            .finish()
    }
}

impl PngCharts {
    /// The embedded fonts only.
    pub fn new() -> Self {
        Self::default()
    }

    /// The embedded fonts, then `fonts` (TrueType or OpenType data) for characters they lack.
    pub fn with_fonts(fonts: Vec<Vec<u8>>) -> Result<Self, ChartError> {
        for data in &fonts {
            font::check(data)?;
        }
        Ok(Self { extra: fonts })
    }
}

/// Characters charts leave out: controls, emoji and their joiners and selectors.
fn dropped(c: char) -> bool {
    matches!(
        u32::from(c),
        0x200B..=0x200D
            | 0x2060
            | 0x2600..=0x27BF
            | 0x2B00..=0x2BFF
            | 0xFE00..=0xFE0F
            | 0x1F000..=0x1FAFF
            | 0xE0020..=0xE007F
    )
}

/// `text` in NFC on one line without emoji; an error names a character no font draws.
fn clean(text: &str, fonts: &Fonts<'_>) -> Result<String, ChartError> {
    let nfc: String = text
        .nfc()
        .filter(|c| !dropped(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let out = nfc.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(c) = out.chars().find(|c| !fonts.has(*c)) {
        return Err(ChartError::Glyphs(c.to_string()));
    }
    Ok(out)
}

/// An optional text, gone when nothing drawable is left.
fn clean_opt(text: Option<&String>, fonts: &Fonts<'_>) -> Result<Option<String>, ChartError> {
    Ok(text
        .map(|t| clean(t, fonts))
        .transpose()?
        .filter(|t| !t.is_empty()))
}

/// `chart` with every text cleaned; a label left empty fails, as it could not be told apart.
fn prepare(chart: &Chart, fonts: &Fonts<'_>) -> Result<Chart, ChartError> {
    let mut out = chart.clone();
    for label in &mut out.labels {
        let cleaned = clean(label, fonts)?;
        if cleaned.is_empty() {
            return Err(ChartError::Glyphs(label.clone()));
        }
        *label = cleaned;
    }
    for series in &mut out.series {
        series.name = clean_opt(series.name.as_ref(), fonts)?;
    }
    out.title = clean_opt(chart.title.as_ref(), fonts)?;
    out.unit = clean_opt(chart.unit.as_ref(), fonts)?;
    Ok(out)
}

/// The image's description: kind, title and unit, then per series its highest and lowest point
/// (or, for lines, where it starts and ends).
fn describe(chart: &Chart, locale: Locale) -> String {
    let w = locale.words();
    let ja = locale == Locale::Ja;
    let item = |label: &str, v: f64| {
        if ja {
            format!("{label}（{}）", locale.full(v))
        } else {
            format!("{label} ({})", locale.full(v))
        }
    };
    let mut out = match chart.kind {
        ChartKind::Column => w.column,
        ChartKind::Line => w.line,
        _ => w.bar,
    }
    .to_string();
    if let Some(title) = &chart.title {
        out.push_str(if ja { "：" } else { ": " });
        out.push_str(title);
    }
    if let Some(unit) = &chart.unit {
        let _ = if ja {
            write!(out, "（{unit}）")
        } else {
            write!(out, " ({unit})")
        };
    }
    let mut parts = Vec::new();
    for series in &chart.series {
        let points: Vec<(&str, f64)> = chart
            .labels
            .iter()
            .zip(&series.values)
            .filter_map(|(l, v)| v.map(|v| (l.as_str(), v)))
            .collect();
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            continue;
        };
        let mut part = series
            .name
            .as_ref()
            .filter(|_| chart.series.len() > 1)
            .map(|n| format!("{n}: "))
            .unwrap_or_default();
        if chart.kind == ChartKind::Line {
            let from = if w.from.is_empty() {
                String::new()
            } else {
                format!("{} ", w.from)
            };
            let _ = write!(
                part,
                "{from}{} {} {}",
                item(first.0, first.1),
                w.to,
                item(last.0, last.1)
            );
        } else {
            let high = points
                .iter()
                .fold(first, |a, b| if b.1 > a.1 { b } else { a });
            let low = points
                .iter()
                .fold(first, |a, b| if b.1 < a.1 { b } else { a });
            let _ = write!(
                part,
                "{} {}{}{} {}",
                w.highest,
                item(high.0, high.1),
                w.sep,
                w.lowest,
                item(low.0, low.1)
            );
        }
        parts.push(part);
    }
    if !parts.is_empty() {
        out.push_str(if ja { "。" } else { ". " });
        out.push_str(&parts.join(if ja { "。" } else { "; " }));
        out.push_str(if ja { "。" } else { "." });
    }
    if out.chars().count() > MAX_ALT {
        out = out.chars().take(MAX_ALT - 1).collect::<String>() + "…";
    }
    out
}

/// The language of `chart`'s numbers and description.
fn locale_of(chart: &Chart) -> Locale {
    Locale::pick(
        chart.locale.as_deref(),
        chart
            .title
            .iter()
            .chain(chart.unit.iter())
            .map(String::as_str),
        chart.labels.iter().map(String::as_str),
    )
}

impl ChartRenderer for PngCharts {
    fn render(&self, chart: &Chart) -> Result<Image, ChartError> {
        chart.validate().map_err(ChartError::Invalid)?;
        let fonts = Fonts::new(&self.extra)?;
        let chart = prepare(chart, &fonts)?;
        let locale = locale_of(&chart);
        let scene = layout::draw(&chart, &fonts, locale);
        let (png, width, height) = raster::png(&scene, &fonts, SCALE)?;
        Ok(Image {
            name: FILE_NAME.into(),
            alt: describe(&chart, locale),
            width,
            height,
            png,
        })
    }
}
