//! Charts drawn as images (ADR 0016): the data the agent gives, and the port that draws it.

use serde::{Deserialize, Serialize};

use crate::prompt::Tone;

/// Categories of a bar chart.
pub const MAX_BARS: usize = 15;
/// Categories of a column chart.
pub const MAX_COLUMNS: usize = 12;
/// Points of a line chart.
pub const MAX_LINE_POINTS: usize = 60;
/// Series of a column or line chart.
pub const MAX_SERIES: usize = 3;
/// Characters of a label or a series name.
pub const MAX_CHART_LABEL: usize = 60;
/// The largest value a chart takes, in size.
pub const MAX_CHART_VALUE: f64 = 1e15;

/// How a chart draws its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ChartKind {
    /// Horizontal bars: comparing and ranking, long labels. One series.
    #[default]
    Bar,
    /// Vertical columns: short categories in order (months, days).
    Column,
    /// Lines: trends over time.
    Line,
}

/// One series of values, one per label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Series {
    /// Its name, shown in the legend when there are several.
    #[serde(default)]
    pub name: Option<String>,
    /// The values; `None` is a gap.
    pub values: Vec<Option<f64>>,
}

/// A chart to draw: labels along one axis and one to [`MAX_SERIES`] series of values.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Chart {
    /// How to draw it.
    #[serde(default)]
    pub kind: ChartKind,
    /// A title drawn in the image.
    #[serde(default)]
    pub title: Option<String>,
    /// The unit of the values (`kg`, `đồng`, `円`), written once under the title.
    #[serde(default)]
    pub unit: Option<String>,
    /// The categories or points in order.
    pub labels: Vec<String>,
    /// The series.
    pub series: Vec<Series>,
    /// The label drawn in the accent colour while the others are muted.
    #[serde(default)]
    pub highlight: Option<usize>,
    /// The colour of a single series.
    #[serde(default)]
    pub tone: Tone,
    /// The language for numbers and the image's description (`vi`, `en`, `ja`); guessed from the
    /// labels when unset.
    #[serde(default)]
    pub locale: Option<String>,
}

impl Chart {
    /// Checks the chart against the limits of its kind; the error says what to change.
    pub fn validate(&self) -> Result<(), String> {
        let (max_labels, max_series) = match self.kind {
            ChartKind::Bar => (MAX_BARS, 1),
            ChartKind::Column => (MAX_COLUMNS, MAX_SERIES),
            ChartKind::Line => (MAX_LINE_POINTS, MAX_SERIES),
        };
        let kind = match self.kind {
            ChartKind::Bar => "bar",
            ChartKind::Column => "column",
            ChartKind::Line => "line",
        };
        let min_labels = if self.kind == ChartKind::Line { 2 } else { 1 };
        if self.labels.len() < min_labels || self.labels.len() > max_labels {
            return Err(format!(
                "a {kind} chart has {min_labels} to {max_labels} labels"
            ));
        }
        if self.series.is_empty() || self.series.len() > max_series {
            return Err(format!("a {kind} chart has 1 to {max_series} series"));
        }
        for label in &self.labels {
            label_ok("chart label", label)?;
        }
        if self.series.len() > 1 && self.series.iter().any(|s| s.name.is_none()) {
            return Err("series need names when there are several".into());
        }
        for s in &self.series {
            if let Some(name) = &s.name {
                label_ok("series name", name)?;
            }
            if s.values.len() != self.labels.len() {
                return Err("each series has one value per label".into());
            }
            if s.values
                .iter()
                .flatten()
                .any(|v| !v.is_finite() || v.abs() > MAX_CHART_VALUE)
            {
                return Err(format!(
                    "chart values are numbers no larger than {MAX_CHART_VALUE:e} in size"
                ));
            }
        }
        if self
            .series
            .iter()
            .all(|s| s.values.iter().all(Option::is_none))
        {
            return Err("a chart needs at least one value".into());
        }
        if let Some(title) = &self.title {
            label_ok("chart title", title)?;
        }
        if let Some(unit) = &self.unit
            && unit.chars().count() > 30
        {
            return Err("chart unit: at most 30 characters".into());
        }
        if self.highlight.is_some_and(|i| i >= self.labels.len()) {
            return Err("the highlight must be one of the labels".into());
        }
        Ok(())
    }
}

fn label_ok(what: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what}s must not be empty"));
    }
    if text.chars().count() > MAX_CHART_LABEL {
        return Err(format!("{what}: at most {MAX_CHART_LABEL} characters"));
    }
    Ok(())
}

/// A picture to send, with what it shows in words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Image {
    /// The file name (`chart.png`).
    pub name: String,
    /// What it shows, for people who cannot see it and for platforms' alt text.
    pub alt: String,
    /// Size in pixels.
    pub width: u32,
    /// Size in pixels.
    pub height: u32,
    /// The PNG data. Never stored: empty in a prompt read back from the store, where it means the
    /// message keeps the picture it was sent with.
    #[serde(skip)]
    pub png: Vec<u8>,
}

/// Why a chart was not drawn. The runtime then shows it as text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChartError {
    /// The chart breaks a limit.
    #[error("invalid chart: {0}")]
    Invalid(String),
    /// No font has these characters.
    #[error("no font for {0:?}")]
    Glyphs(String),
    /// Drawing failed.
    #[error("cannot draw the chart: {0}")]
    Failed(String),
}

/// Draws charts as PNG images (ADR 0016). Synchronous and CPU-bound: the runtime calls it off the
/// async threads.
pub trait ChartRenderer: Send + Sync {
    /// Draws `chart`.
    fn render(&self, chart: &Chart) -> Result<Image, ChartError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chart(kind: ChartKind, labels: &[&str], series: Vec<Vec<Option<f64>>>) -> Chart {
        Chart {
            kind,
            labels: labels.iter().map(|l| (*l).to_string()).collect(),
            series: series
                .into_iter()
                .enumerate()
                .map(|(i, values)| Series {
                    name: Some(format!("s{i}")),
                    values,
                })
                .collect(),
            ..Chart::default()
        }
    }

    #[test]
    fn limits_by_kind() {
        let one = chart(ChartKind::Bar, &["a", "b"], vec![vec![Some(1.0), None]]);
        assert_eq!(one.validate(), Ok(()));
        let two = chart(
            ChartKind::Bar,
            &["a"],
            vec![vec![Some(1.0)], vec![Some(2.0)]],
        );
        assert!(two.validate().is_err());
        let column = Chart {
            kind: ChartKind::Column,
            ..two
        };
        assert_eq!(column.validate(), Ok(()));
        let line = chart(ChartKind::Line, &["a"], vec![vec![Some(1.0)]]);
        assert!(line.validate().is_err(), "a line needs two points");
        let bad = chart(ChartKind::Bar, &["a"], vec![vec![Some(f64::NAN)]]);
        assert!(bad.validate().is_err());
        let empty = chart(ChartKind::Bar, &["a"], vec![vec![None]]);
        assert!(empty.validate().is_err());
        let short = chart(ChartKind::Bar, &["a", "b"], vec![vec![Some(1.0)]]);
        assert!(short.validate().is_err());
    }
}
