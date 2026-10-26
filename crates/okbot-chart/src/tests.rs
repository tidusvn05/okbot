#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use okbot_core::{Chart, ChartError, ChartKind, ChartRenderer as _, Series, Tone};
use proptest::prelude::*;

use crate::font::Fonts;
use crate::number::Locale;
use crate::{PngCharts, describe, layout, locale_of, prepare};

fn chart(kind: ChartKind, labels: &[&str], series: &[(&str, &[Option<f64>])]) -> Chart {
    Chart {
        kind,
        labels: labels.iter().map(|l| (*l).to_string()).collect(),
        series: series
            .iter()
            .map(|(name, values)| Series {
                name: (!name.is_empty()).then(|| (*name).to_string()),
                values: values.to_vec(),
            })
            .collect(),
        ..Chart::default()
    }
}

fn sales() -> Chart {
    Chart {
        title: Some("Doanh thu tuần".into()),
        unit: Some("đồng".into()),
        highlight: Some(2),
        ..chart(
            ChartKind::Bar,
            &[
                "Trà sữa trân châu đường đen",
                "Cà phê đá",
                // Decomposed, as some keyboards send it.
                "Pho\u{31b}\u{309} bo\u{300} ta\u{301}i chi\u{301}n",
                "抹茶ラテ（アイス）",
                "✅ Iced Americano",
            ],
            &[(
                "",
                &[
                    Some(1_234_500.0),
                    Some(955_000.0),
                    Some(1_420_000.0),
                    Some(610_000.0),
                    Some(300_000.0),
                ],
            )],
        )
    }
}

fn months() -> Chart {
    Chart {
        title: Some("月別売上".into()),
        unit: Some("円".into()),
        ..chart(
            ChartKind::Column,
            &["1月", "2月", "3月", "4月", "5月", "6月"],
            &[
                (
                    "2025年",
                    &[
                        Some(1_200_000.0),
                        Some(980_000.0),
                        Some(1_430_000.0),
                        Some(1_610_000.0),
                        Some(1_550_000.0),
                        Some(1_720_000.0),
                    ],
                ),
                (
                    "2026年",
                    &[
                        Some(1_350_000.0),
                        Some(1_120_000.0),
                        Some(1_500_000.0),
                        None,
                        Some(1_880_000.0),
                        Some(2_040_000.0),
                    ],
                ),
            ],
        )
    }
}

fn trend() -> Chart {
    let values: Vec<Option<f64>> = (0..30)
        .map(|d| (d != 12).then(|| 40.0 + f64::from(d) * 1.5 + f64::from(d % 7) * 4.0))
        .collect();
    let labels: Vec<String> = (1..=30).map(|d| format!("Oct {d}")).collect();
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    Chart {
        title: Some("Daily active users".into()),
        tone: Tone::Success,
        ..chart(ChartKind::Line, &labels, &[("", &values)])
    }
}

fn scene(chart: &Chart) -> String {
    let fonts = Fonts::new(&[]).unwrap();
    let chart = prepare(chart, &fonts).unwrap();
    layout::draw(&chart, &fonts, locale_of(&chart)).to_string()
}

#[test]
fn bar_scene() {
    insta::assert_snapshot!(scene(&sales()));
}

#[test]
fn column_scene() {
    insta::assert_snapshot!(scene(&months()));
}

#[test]
fn line_scene() {
    insta::assert_snapshot!(scene(&trend()));
}

#[test]
fn images_are_png_sized_for_chat() {
    let renderer = PngCharts::new();
    for chart in [sales(), months(), trend()] {
        let image = renderer.render(&chart).unwrap();
        assert!(image.png.starts_with(b"\x89PNG"), "a PNG");
        assert_eq!(image.width, 1200);
        assert!(image.height <= 1680, "{}", image.height);
        assert_eq!(image.name, "chart.png");
        if let Ok(dir) = std::env::var("OKBOT_CHART_OUT") {
            let name = format!("{dir}/{:?}.png", chart.kind).to_lowercase();
            std::fs::write(name, &image.png).unwrap();
        }
    }
}

#[test]
fn text_is_normalized_and_emoji_dropped() {
    let fonts = Fonts::new(&[]).unwrap();
    let chart = prepare(&sales(), &fonts).unwrap();
    assert_eq!(chart.labels[2], "Phở bò tái chín");
    assert_eq!(chart.labels[4], "Iced Americano");
}

#[test]
fn characters_no_font_has_fail() {
    let korean = chart(ChartKind::Bar, &["매출"], &[("", &[Some(1.0)])]);
    assert!(matches!(
        PngCharts::new().render(&korean),
        Err(ChartError::Glyphs(c)) if c == "매"
    ));
    let only_emoji = chart(ChartKind::Bar, &["🍵"], &[("", &[Some(1.0)])]);
    assert!(matches!(
        PngCharts::new().render(&only_emoji),
        Err(ChartError::Glyphs(_))
    ));
}

#[test]
fn every_vietnamese_letter_and_kana_is_drawn() {
    let fonts = Fonts::new(&[]).unwrap();
    let marks = ["", "\u{300}", "\u{301}", "\u{303}", "\u{309}", "\u{323}"];
    let mut missing = Vec::new();
    for base in "aăâeêioôơuưyAĂÂEÊIOÔƠUƯY".chars() {
        for mark in marks {
            let letter: String =
                unicode_normalization::UnicodeNormalization::nfc(format!("{base}{mark}").as_str())
                    .collect();
            if letter.chars().count() != 1 || !letter.chars().all(|c| fonts.has(c)) {
                missing.push(letter);
            }
        }
    }
    for c in "đĐ₫".chars() {
        if !fonts.has(c) {
            missing.push(c.to_string());
        }
    }
    if cfg!(feature = "ja") {
        let kana = ('\u{3041}'..='\u{3096}').chain('\u{30A1}'..='\u{30FA}');
        let kanji = "今週売上月別円日本語東京大阪抹茶年度合計前比増減客数品目店舗".chars();
        missing.extend(
            kana.chain(kanji)
                .filter(|c| !fonts.has(*c))
                .map(String::from),
        );
    }
    assert_eq!(missing, Vec::<String>::new());
}

#[test]
fn descriptions_in_each_language() {
    let fonts = Fonts::new(&[]).unwrap();
    let vi = prepare(&sales(), &fonts).unwrap();
    assert_eq!(
        describe(&vi, Locale::Vi),
        "Biểu đồ thanh ngang: Doanh thu tuần (đồng). cao nhất Phở bò tái chín (1.420.000), \
         thấp nhất Iced Americano (300.000)."
    );
    assert_eq!(
        describe(&months(), Locale::Ja),
        "縦棒グラフ：月別売上（円）。2025年: 最大 6月（1,720,000）、最小 2月（980,000）。\
         2026年: 最大 6月（2,040,000）、最小 2月（1,120,000）。"
    );
    let en = describe(&trend(), Locale::En);
    assert!(
        en.starts_with("Line chart: Daily active users. from Oct 1 (40) to Oct 30 (87.5)"),
        "{en}"
    );
}

#[test]
fn extra_fonts_must_be_fonts() {
    assert!(PngCharts::with_fonts(vec![b"not a font".to_vec()]).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Any labels and values either draw or fail cleanly, within the image limits.
    #[test]
    fn never_panics(
        kind in prop_oneof![Just(ChartKind::Bar), Just(ChartKind::Column), Just(ChartKind::Line)],
        labels in prop::collection::vec("\\PC{0,70}", 0..20),
        values in prop::collection::vec(prop::option::of(-1e16f64..1e16), 0..20),
        highlight in prop::option::of(0usize..20),
    ) {
        let n = labels.len();
        let chart = Chart {
            kind,
            labels,
            series: vec![Series { name: None, values: values.into_iter().take(n).collect() }],
            highlight,
            ..Chart::default()
        };
        if let Ok(image) = PngCharts::new().render(&chart) {
            prop_assert!(image.height <= 1680);
        }
    }
}
