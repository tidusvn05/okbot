//! Numbers as people of each language write them, and "nice" axis ticks.

/// The language of a chart's numbers and description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Locale {
    Vi,
    En,
    Ja,
}

/// Letters only Vietnamese uses among the three languages.
fn vietnamese(c: char) -> bool {
    matches!(
        c,
        'ă' | 'â' | 'đ' | 'ê' | 'ô' | 'ơ' | 'ư' | 'Ă' | 'Â' | 'Đ' | 'Ê' | 'Ô' | 'Ơ' | 'Ư'
    ) || ('\u{1EA0}'..='\u{1EF9}').contains(&c)
}

fn japanese(c: char) -> bool {
    ('\u{3040}'..='\u{30FF}').contains(&c) || ('\u{4E00}'..='\u{9FFF}').contains(&c)
}

impl Locale {
    /// The chart's `locale` when it names one of the three; else the script of its title and
    /// unit; else the script most labels use (the first label's on a tie); else English.
    pub(crate) fn pick<'a>(
        tag: Option<&str>,
        mut heading: impl Iterator<Item = &'a str>,
        labels: impl Iterator<Item = &'a str>,
    ) -> Self {
        let tag = tag.unwrap_or_default().to_ascii_lowercase();
        for (prefix, locale) in [("vi", Self::Vi), ("ja", Self::Ja), ("en", Self::En)] {
            if tag == prefix || tag.starts_with(&format!("{prefix}-")) {
                return locale;
            }
        }
        let script = |t: &str| {
            if t.chars().any(japanese) {
                Some(Self::Ja)
            } else if t.chars().any(vietnamese) {
                Some(Self::Vi)
            } else {
                None
            }
        };
        if let Some(locale) = heading.find_map(script) {
            return locale;
        }
        let scripts: Vec<Self> = labels.filter_map(script).collect();
        let count = |l: Self| scripts.iter().filter(|s| **s == l).count();
        match count(Self::Vi).cmp(&count(Self::Ja)) {
            std::cmp::Ordering::Greater => Self::Vi,
            std::cmp::Ordering::Less => Self::Ja,
            std::cmp::Ordering::Equal => scripts.first().copied().unwrap_or(Self::En),
        }
    }

    /// `Unit: kg`, `単位：円`.
    pub(crate) fn unit(self, unit: &str) -> String {
        match self {
            Self::Vi => format!("Đơn vị: {unit}"),
            Self::En => format!("Unit: {unit}"),
            Self::Ja => format!("単位：{unit}"),
        }
    }

    /// The group and decimal separators.
    fn separators(self) -> (char, char) {
        match self {
            Self::Vi => ('.', ','),
            Self::En | Self::Ja => (',', '.'),
        }
    }

    /// `value` in full: grouped digits and up to `decimals` decimals, trailing zeros dropped.
    fn digits(self, value: f64, decimals: usize) -> String {
        let (group, point) = self.separators();
        let text = format!("{:.*}", decimals, value.abs());
        let (int, frac) = text.split_once('.').unwrap_or((&text, ""));
        let frac = frac.trim_end_matches('0');
        let mut out = String::new();
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i).is_multiple_of(3) {
                out.push(group);
            }
            out.push(c);
        }
        if !frac.is_empty() {
            out.push(point);
            out.push_str(frac);
        }
        // -0 and values that round to zero have no sign.
        if value < 0.0 && out.chars().any(|c| c.is_ascii_digit() && c != '0') {
            out.insert(0, '−');
        }
        out
    }

    /// `value` in full, as on a bar: `1.234.500` (vi), `1,234,500` (en, ja), up to 2 decimals.
    pub(crate) fn full(self, value: f64) -> String {
        self.digits(value, 2)
    }

    /// `value` short, as on an axis: `1,2 tr`, `1.2M`, `120万`.
    pub(crate) fn compact(self, value: f64) -> String {
        let steps: &[(f64, &str)] = match self {
            Self::Vi => &[(1e9, " tỷ"), (1e6, " tr"), (1e3, " nghìn")],
            Self::En => &[(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")],
            Self::Ja => &[(1e12, "兆"), (1e8, "億"), (1e4, "万")],
        };
        let size = value.abs();
        for &(scale, suffix) in steps {
            if size >= scale {
                let short = value / scale;
                let decimals = usize::from(short.abs() < 100.0);
                return format!("{}{suffix}", self.digits(short, decimals));
            }
        }
        self.digits(value, if size < 10.0 { 2 } else { 1 })
    }

    /// The words of the image's description.
    pub(crate) fn words(self) -> Words {
        match self {
            Self::Vi => Words {
                bar: "Biểu đồ thanh ngang",
                column: "Biểu đồ cột",
                line: "Biểu đồ đường",
                highest: "cao nhất",
                lowest: "thấp nhất",
                from: "từ",
                to: "đến",
                sep: ", ",
            },
            Self::En => Words {
                bar: "Bar chart",
                column: "Column chart",
                line: "Line chart",
                highest: "highest",
                lowest: "lowest",
                from: "from",
                to: "to",
                sep: ", ",
            },
            Self::Ja => Words {
                bar: "横棒グラフ",
                column: "縦棒グラフ",
                line: "折れ線グラフ",
                highest: "最大",
                lowest: "最小",
                from: "",
                to: "→",
                sep: "、",
            },
        }
    }
}

/// Words for describing a chart in one language.
pub(crate) struct Words {
    pub(crate) bar: &'static str,
    pub(crate) column: &'static str,
    pub(crate) line: &'static str,
    pub(crate) highest: &'static str,
    pub(crate) lowest: &'static str,
    pub(crate) from: &'static str,
    pub(crate) to: &'static str,
    pub(crate) sep: &'static str,
}

/// Round ticks covering `min..=max`: steps of 1, 2, 2.5 or 5 times a power of ten, at most
/// `count + 1` of them, the tightest fit first and then the count nearest `count`.
pub(crate) fn ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    let (min, max) = if (max - min).abs() < f64::EPSILON {
        if max == 0.0 {
            (0.0, 1.0)
        } else {
            (min - min.abs() * 0.5, max + max.abs() * 0.5)
        }
    } else {
        (min, max)
    };
    let raw = (max - min) / count.saturating_sub(1).max(1) as f64;
    let power = 10f64.powf(raw.log10().floor());
    let mut best: Option<(f64, usize, f64, f64, f64)> = None;
    for scale in [0.1, 1.0, 10.0] {
        for m in [1.0, 2.0, 2.5, 5.0] {
            let step = m * power * scale;
            let (lo, hi) = ((min / step).floor(), (max / step).ceil());
            let n = (hi - lo) as usize + 1;
            if n < 3 || n > count + 1 {
                continue;
            }
            // 2.5 only when clearly tighter.
            let spread = (hi - lo) * step * if m == 2.5 { 1.1 } else { 1.0 };
            let off = n.abs_diff(count);
            let better = best.is_none_or(|(s, o, ..)| {
                spread < s - s * 1e-9 || ((spread - s).abs() <= s * 1e-9 && off < o)
            });
            if better {
                best = Some((spread, off, step, lo, hi));
            }
        }
    }
    let Some((_, _, step, lo, hi)) = best else {
        return vec![min, max];
    };
    // Undo float drift (0.15000000000000002) by rounding to the step's decimals.
    let precision = 10f64.powf((2.0 - step.log10().floor()).max(0.0));
    (0..=(hi - lo) as usize)
        .map(|i| ((lo + i as f64) * step * precision).round() / precision)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_by_language() {
        let cases = [
            (Locale::Vi, 1_234_500.0, "1.234.500", "1,2 tr"),
            (Locale::En, 1_234_500.0, "1,234,500", "1.2M"),
            (Locale::Ja, 1_234_500.0, "1,234,500", "123万"),
            (Locale::Ja, 34_560_000.0, "34,560,000", "3,456万"),
            (Locale::Ja, 120_000_000.0, "120,000,000", "1.2億"),
            (Locale::Vi, 2_500_000_000.0, "2.500.000.000", "2,5 tỷ"),
            (Locale::Vi, 1.5, "1,5", "1,5"),
            (Locale::En, -1234.567, "−1,234.57", "−1.2K"),
            (Locale::En, 0.0, "0", "0"),
            (Locale::En, 999.0, "999", "999"),
        ];
        for (locale, value, full, compact) in cases {
            assert_eq!(locale.full(value), full, "{locale:?} {value}");
            assert_eq!(locale.compact(value), compact, "{locale:?} {value}");
        }
    }

    #[test]
    fn languages_are_guessed_from_the_labels() {
        let pick = |heading: &[&str], labels: &[&str]| {
            Locale::pick(None, heading.iter().copied(), labels.iter().copied())
        };
        assert_eq!(pick(&[], &["Trà sữa", "Coffee"]), Locale::Vi);
        assert_eq!(
            pick(&[], &["抹茶", "Trà sữa"]),
            Locale::Ja,
            "a tie: the first"
        );
        assert_eq!(pick(&[], &["抹茶", "Trà sữa", "Cà phê"]), Locale::Vi);
        assert_eq!(pick(&["Doanh thu tuần"], &["抹茶", "Matcha"]), Locale::Vi);
        assert_eq!(pick(&["Sales"], &["Tea", "Coffee"]), Locale::En);
        let tagged = Locale::pick(Some("ja-JP"), std::iter::empty(), ["Tea"].into_iter());
        assert_eq!(tagged, Locale::Ja);
        let other = Locale::pick(Some("fr"), std::iter::empty(), ["Tea"].into_iter());
        assert_eq!(other, Locale::En);
    }

    #[test]
    fn round_ticks() {
        assert_eq!(
            ticks(0.0, 1_420_000.0, 5),
            [0.0, 500_000.0, 1e6, 1_500_000.0]
        );
        assert_eq!(
            ticks(0.0, 2_040_000.0, 5),
            [0.0, 500_000.0, 1e6, 1_500_000.0, 2e6, 2_500_000.0]
        );
        assert_eq!(ticks(0.0, 7.0, 5), [0.0, 2.0, 4.0, 6.0, 8.0]);
        assert_eq!(ticks(-3.0, 9.0, 5), [-5.0, 0.0, 5.0, 10.0]);
        assert_eq!(ticks(0.0, 0.0, 5), [0.0, 0.2, 0.4, 0.6, 0.8, 1.0]);
        assert_eq!(ticks(0.1, 0.3, 5), [0.1, 0.15, 0.2, 0.25, 0.3]);
    }
}
