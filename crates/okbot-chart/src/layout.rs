//! Lays a chart out for chat: 480 logical pixels wide, so that a phone showing it about 360 dp
//! wide still draws 14 px labels at about 11 dp.

use okbot_core::{Chart, ChartKind, Tone};

use crate::font::Fonts;
use crate::number::{Locale, ticks};
use crate::scene::{Anchor, Item, Rgb, Scene};

pub(crate) const WIDTH: f32 = 480.0;
const PAD: f32 = 20.0;
const TITLE: f32 = 17.0;
const SUBTITLE: f32 = 12.5;
const LABEL: f32 = 14.0;
const VALUE: f32 = 13.0;
const AXIS: f32 = 12.0;
/// Height of one bar's row, and the thickness of the bar.
const ROW: f32 = 30.0;
const BAR: f32 = 18.0;
/// Height of a column or line chart's plot.
const PLOT: f32 = 200.0;

const INK: Rgb = Rgb(0x0011_1827);
const MUTED: Rgb = Rgb(0x0064_748B);
const GRID: Rgb = Rgb(0x00E5_E7EB);
const AXIS_LINE: Rgb = Rgb(0x009C_A3AF);
/// Bars that are not the highlight.
const DIM: Rgb = Rgb(0x00CB_D5E1);
/// Several series: blue, orange, teal (told apart with colour blindness too; lines also differ
/// by dash).
const SERIES: [Rgb; 3] = [Rgb(0x0025_63EB), Rgb(0x00EA_580C), Rgb(0x000D_9488)];
const DASHES: [Option<[f32; 2]>; 3] = [None, Some([6.0, 4.0]), Some([2.0, 3.0])];

struct Canvas<'f, 'a> {
    fonts: &'f Fonts<'a>,
    items: Vec<Item>,
}

impl Canvas<'_, '_> {
    fn width(&self, text: &str, size: f32, bold: bool) -> f32 {
        self.fonts.width(text, size, bold)
    }

    #[allow(clippy::too_many_arguments)] // a text item has this many parts
    fn text(
        &mut self,
        x: f32,
        y: f32,
        size: f32,
        bold: bool,
        anchor: Anchor,
        color: Rgb,
        text: String,
    ) {
        if !text.is_empty() {
            self.items.push(Item::Text {
                x,
                y,
                size,
                bold,
                anchor,
                color,
                text,
            });
        }
    }

    /// `text` centred on `x`, kept inside the image.
    fn centred(&mut self, x: f32, y: f32, size: f32, max: f32, color: Rgb, text: &str) {
        let text = self.fonts.fit(text, size, false, max);
        let half = self.width(&text, size, false) / 2.0;
        let x = x.clamp(PAD + half, WIDTH - PAD - half);
        self.text(x, y, size, false, Anchor::Middle, color, text);
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, color: Rgb) {
        self.items.push(Item::Rect {
            x,
            y,
            w,
            h,
            radius,
            color,
        });
    }

    fn line(&mut self, points: Vec<(f32, f32)>, width: f32, color: Rgb, dash: Option<[f32; 2]>) {
        self.items.push(Item::Line {
            points,
            width,
            color,
            dash,
        });
    }
}

/// Lays out `chart`, whose texts are NFC and drawable.
pub(crate) fn draw(chart: &Chart, fonts: &Fonts<'_>, locale: Locale) -> Scene {
    let mut c = Canvas {
        fonts,
        items: Vec::new(),
    };
    let top = header(&mut c, chart, locale);
    let bottom = match chart.kind {
        ChartKind::Column => columns(&mut c, chart, locale, top),
        ChartKind::Line => lines(&mut c, chart, locale, top),
        _ => bars(&mut c, chart, locale, top),
    };
    Scene {
        width: WIDTH,
        height: (bottom + PAD).ceil(),
        items: c.items,
    }
}

/// The colour of one series.
fn series_color(chart: &Chart, series: usize) -> Rgb {
    if chart.series.len() > 1 {
        SERIES[series % SERIES.len()]
    } else if chart.tone == Tone::Neutral {
        SERIES[0]
    } else {
        Rgb(chart.tone.color())
    }
}

/// The colour of one bar or column: muted when another label is the highlight.
fn fill(chart: &Chart, series: usize, label: usize) -> Rgb {
    match chart.highlight {
        Some(h) if h != label => DIM,
        _ => series_color(chart, series),
    }
}

/// Title, unit and legend; returns where the plot starts.
fn header(c: &mut Canvas<'_, '_>, chart: &Chart, locale: Locale) -> f32 {
    let inner = WIDTH - 2.0 * PAD;
    let mut y = PAD;
    if let Some(title) = &chart.title {
        y += TITLE;
        let title = c.fonts.fit(title, TITLE, true, inner);
        c.text(PAD, y, TITLE, true, Anchor::Start, INK, title);
        y += TITLE * 0.45;
    }
    if let Some(unit) = &chart.unit {
        y += SUBTITLE + 4.0;
        let text = locale.unit(unit);
        let text = c.fonts.fit(&text, SUBTITLE, false, inner);
        c.text(PAD, y, SUBTITLE, false, Anchor::Start, MUTED, text);
        y += SUBTITLE * 0.45;
    }
    if chart.series.len() > 1 {
        y = legend(c, chart, y + 10.0) + 6.0;
    }
    y + 12.0
}

/// One swatch and name per series, wrapping; returns the bottom.
fn legend(c: &mut Canvas<'_, '_>, chart: &Chart, top: f32) -> f32 {
    let (mut x, mut y) = (PAD, top);
    for (i, series) in chart.series.iter().enumerate() {
        let name = c.fonts.fit(
            series.name.as_deref().unwrap_or_default(),
            SUBTITLE,
            false,
            150.0,
        );
        let w = 18.0 + c.width(&name, SUBTITLE, false);
        if x + w > WIDTH - PAD && x > PAD {
            x = PAD;
            y += 18.0;
        }
        let color = series_color(chart, i);
        if chart.kind == ChartKind::Line {
            c.line(
                vec![(x, y + 7.0), (x + 13.0, y + 7.0)],
                2.5,
                color,
                DASHES[i % 3],
            );
        } else {
            c.rect(x, y + 2.0, 10.0, 10.0, 2.0, color);
        }
        c.text(
            x + 18.0,
            y + 11.5,
            SUBTITLE,
            false,
            Anchor::Start,
            INK,
            name,
        );
        x += w + 16.0;
    }
    y + 14.0
}

/// The smallest and largest values (`0, 1` when there are none).
fn extent(chart: &Chart) -> (f64, f64) {
    let values = chart.series.iter().flat_map(|s| s.values.iter().flatten());
    values
        .fold(None, |acc: Option<(f64, f64)>, &v| {
            Some(acc.map_or((v, v), |(lo, hi)| (lo.min(v), hi.max(v))))
        })
        .unwrap_or((0.0, 1.0))
}

/// Horizontal bars with their labels on the left and full values on the right.
fn bars(c: &mut Canvas<'_, '_>, chart: &Chart, locale: Locale, top: f32) -> f32 {
    let inner = WIDTH - 2.0 * PAD;
    let values = &chart.series[0].values;
    let texts: Vec<String> = values
        .iter()
        .map(|v| v.map_or_else(|| "—".to_string(), |v| locale.full(v)))
        .collect();
    let value_w = texts
        .iter()
        .map(|t| c.width(t, VALUE, true))
        .fold(0.0, f32::max);
    let longest = chart
        .labels
        .iter()
        .map(|l| c.width(l, LABEL, false))
        .fold(0.0, f32::max);
    let mut label_w = longest.min(inner * 0.42);
    let room = |label_w: f32| inner - label_w - 12.0 - value_w - 10.0;
    if room(label_w) < 100.0 {
        label_w = (label_w - (100.0 - room(label_w))).max(60.0);
    }
    let area_w = room(label_w).max(40.0);
    let area_x = PAD + label_w + 12.0;
    let (lo, hi) = extent(chart);
    let (lo, hi) = (lo.min(0.0), hi.max(0.0));
    let span = if hi > lo { hi - lo } else { 1.0 };
    let x_of = |v: f64| area_x + area_w * ((v - lo) / span) as f32;
    let zero = x_of(0.0);
    for (i, (label, text)) in chart.labels.iter().zip(texts).enumerate() {
        let mid = top + i as f32 * ROW + ROW / 2.0;
        let label = c.fonts.fit(label, LABEL, false, label_w);
        c.text(
            PAD,
            mid + LABEL * 0.36,
            LABEL,
            false,
            Anchor::Start,
            INK,
            label,
        );
        if let Some(v) = values[i] {
            let end = x_of(v);
            let (x, w) = (zero.min(end), (end - zero).abs().max(2.0));
            c.rect(x, mid - BAR / 2.0, w, BAR, 3.0, fill(chart, 0, i));
        }
        let strong = chart.highlight == Some(i);
        let color = if strong { INK } else { MUTED };
        c.text(
            WIDTH - PAD,
            mid + VALUE * 0.36,
            VALUE,
            strong,
            Anchor::End,
            color,
            text,
        );
    }
    let bottom = top + chart.labels.len() as f32 * ROW;
    if lo < 0.0 {
        c.line(vec![(zero, top), (zero, bottom)], 1.0, AXIS_LINE, None);
    }
    bottom
}

/// A plot with a value axis on the left and grid lines at round ticks.
struct Frame {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    lo: f64,
    hi: f64,
}

impl Frame {
    fn new(c: &mut Canvas<'_, '_>, chart: &Chart, locale: Locale, top: f32, zero: bool) -> Self {
        let (mut lo, mut hi) = extent(chart);
        if zero {
            (lo, hi) = (lo.min(0.0), hi.max(0.0));
        }
        let ticks = ticks(lo, hi, 5);
        let labels: Vec<String> = ticks.iter().map(|t| locale.compact(*t)).collect();
        let axis_w = labels
            .iter()
            .map(|l| c.width(l, AXIS, false))
            .fold(0.0, f32::max);
        let x = PAD + axis_w + 8.0;
        let frame = Self {
            x,
            y: top,
            w: WIDTH - PAD - x,
            h: PLOT,
            lo: ticks.first().copied().unwrap_or(lo),
            hi: ticks.last().copied().unwrap_or(hi),
        };
        for (t, label) in ticks.iter().zip(labels) {
            let y = frame.y_of(*t);
            let color = if *t == 0.0 { AXIS_LINE } else { GRID };
            c.line(vec![(frame.x, y), (frame.x + frame.w, y)], 1.0, color, None);
            c.text(
                x - 8.0,
                y + AXIS * 0.36,
                AXIS,
                false,
                Anchor::End,
                MUTED,
                label,
            );
        }
        frame
    }

    fn y_of(&self, v: f64) -> f32 {
        let span = if self.hi > self.lo {
            self.hi - self.lo
        } else {
            1.0
        };
        self.y + self.h * (1.0 - ((v - self.lo) / span) as f32)
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Vertical columns, side by side when there are several series.
fn columns(c: &mut Canvas<'_, '_>, chart: &Chart, locale: Locale, top: f32) -> f32 {
    let n = chart.labels.len();
    let series = chart.series.len();
    let values_on_top = series == 1 && n <= 8;
    let top = if values_on_top { top + 14.0 } else { top };
    let f = Frame::new(c, chart, locale, top, true);
    let group = f.w / n as f32;
    let used = group * 0.72;
    let bar_w = used / series as f32;
    let zero = f.y_of(0.0);
    for (s, data) in chart.series.iter().enumerate() {
        for (i, v) in data.values.iter().enumerate() {
            let Some(v) = *v else { continue };
            let x = f.x + group * i as f32 + (group - used) / 2.0 + bar_w * s as f32;
            let end = f.y_of(v);
            let gap = if series > 1 { 1.5 } else { 0.0 };
            let (y, h) = (zero.min(end), (end - zero).abs().max(1.5));
            c.rect(x + gap / 2.0, y, bar_w - gap, h, 2.5, fill(chart, s, i));
            if values_on_top {
                let strong = chart.highlight == Some(i);
                let y = if v >= 0.0 {
                    end - 5.0
                } else {
                    end + AXIS + 3.0
                };
                let color = if strong { INK } else { MUTED };
                let text = locale.compact(v);
                c.text(
                    x + bar_w / 2.0,
                    y,
                    AXIS,
                    strong,
                    Anchor::Middle,
                    color,
                    text,
                );
            }
        }
    }
    for (i, label) in chart.labels.iter().enumerate() {
        let x = f.x + group * (i as f32 + 0.5);
        c.centred(x, f.bottom() + 18.0, AXIS, group - 4.0, INK, label);
    }
    f.bottom() + 24.0
}

/// Which labels of `n` points fit under a line chart: every `step`-th, and the last.
fn shown(n: usize, step: usize) -> Vec<usize> {
    let mut out: Vec<usize> = (0..n).step_by(step.max(1)).collect();
    if let Some(&last) = out.last()
        && last != n - 1
    {
        if (n - 1 - last) * 5 >= step * 3 {
            out.push(n - 1);
        } else if let Some(l) = out.last_mut() {
            *l = n - 1;
        }
    }
    out
}

/// Lines through the points, broken at gaps; one value labelled (the highlight, or the last).
fn lines(c: &mut Canvas<'_, '_>, chart: &Chart, locale: Locale, top: f32) -> f32 {
    let n = chart.labels.len();
    let (lo, hi) = extent(chart);
    let zero = lo >= 0.0 && lo <= hi * 0.5;
    let f = Frame::new(c, chart, locale, top + 14.0, zero);
    let x_of = |i: usize| f.x + 6.0 + (f.w - 12.0) * i as f32 / (n - 1) as f32;
    let single = chart.series.len() == 1;
    for (s, data) in chart.series.iter().enumerate() {
        let color = series_color(chart, s);
        let dash = if single { None } else { DASHES[s % 3] };
        let mut run = Vec::new();
        for (i, v) in data.values.iter().enumerate() {
            match v {
                Some(v) => run.push((x_of(i), f.y_of(*v))),
                None if !run.is_empty() => c.line(std::mem::take(&mut run), 2.25, color, dash),
                None => {}
            }
        }
        if !run.is_empty() {
            c.line(run, 2.25, color, dash);
        }
        let dots = n <= 24;
        let marked = chart.highlight.or_else(|| {
            single
                .then(|| data.values.iter().rposition(Option::is_some))
                .flatten()
        });
        for (i, v) in data.values.iter().enumerate() {
            let Some(v) = *v else { continue };
            let (x, y) = (x_of(i), f.y_of(v));
            if Some(i) == marked {
                c.items.push(Item::Dot {
                    x,
                    y,
                    r: 4.5,
                    color,
                });
                let text = locale.compact(v);
                let half = c.width(&text, AXIS, true) / 2.0;
                let x = x.clamp(PAD + half, WIDTH - PAD - half);
                c.text(x, y - 9.0, AXIS, true, Anchor::Middle, INK, text);
            } else if dots {
                c.items.push(Item::Dot {
                    x,
                    y,
                    r: 2.75,
                    color,
                });
            }
        }
    }
    let widest = chart
        .labels
        .iter()
        .map(|l| c.width(l, AXIS, false))
        .fold(0.0, f32::max)
        .min(110.0);
    let slots = ((f.w / (widest + 12.0)) as usize).max(2);
    for i in shown(n, n.div_ceil(slots)) {
        c.centred(
            x_of(i),
            f.bottom() + 18.0,
            AXIS,
            widest,
            INK,
            &chart.labels[i],
        );
    }
    f.bottom() + 24.0
}

#[cfg(test)]
mod tests {
    use super::shown;

    #[test]
    fn line_labels_thin_out_and_keep_the_last() {
        assert_eq!(shown(5, 1), [0, 1, 2, 3, 4]);
        assert_eq!(shown(12, 3), [0, 3, 6, 9, 11]);
        assert_eq!(shown(11, 3), [0, 3, 6, 10]);
        assert_eq!(shown(60, 9), [0, 9, 18, 27, 36, 45, 59]);
    }
}
