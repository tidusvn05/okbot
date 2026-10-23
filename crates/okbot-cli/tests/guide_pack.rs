//! The `okbot-guide` knowledge pack must describe the configuration this version accepts: every
//! TOML example in it is parsed against the real schema. Blocks fenced as ```` ```toml <program> ````
//! are another program's configuration and only have to be valid TOML.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::path::{Path, PathBuf};

fn pack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../okbot-knowledge/packs/okbot-guide")
}

/// `(file, first line, block)` for every block of the pack whose fence line is `fence`.
fn toml_blocks(fence: fn(&str) -> bool) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(pack_dir()).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut block: Option<(usize, String)> = None;
        for (n, line) in text.lines().enumerate() {
            match (&mut block, line.trim()) {
                (None, line) if fence(line) => block = Some((n + 2, String::new())),
                (Some(_), "```") => {
                    let (start, body) = block.take().unwrap();
                    out.push((name.clone(), start, body));
                }
                (Some((_, body)), _) => {
                    body.push_str(line);
                    body.push('\n');
                }
                (None, _) => {}
            }
        }
    }
    out.sort();
    out
}

fn merge(into: &mut toml::Table, from: toml::Table) {
    for (key, value) in from {
        match (into.get_mut(&key), value) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge(a, b),
            (_, value) => {
                into.insert(key, value);
            }
        }
    }
}

#[test]
fn every_toml_example_uses_known_settings() {
    let blocks = toml_blocks(|line| line == "```toml");
    assert!(blocks.len() > 20, "found only {} examples", blocks.len());
    let dir = tempfile::tempdir().unwrap();
    for (file, line, block) in blocks {
        let at = format!("{file}:{line}");
        let snippet: toml::Table =
            toml::from_str(&block).unwrap_or_else(|e| panic!("{at} is not valid TOML: {e}"));
        let mut config: toml::Table =
            toml::from_str("config_version = 1\n[bot]\nname = \"guide\"\n").unwrap();
        merge(&mut config, snippet);
        let text = toml::to_string(&config).unwrap();
        // Examples are fragments: a missing field elsewhere is fine, an unknown one is not.
        if let Err(e) = okbot_config::Config::from_text(dir.path(), &text) {
            let message = e.to_string();
            assert!(
                !message.contains("unknown field") && !message.contains("unknown variant"),
                "{at}: {message}"
            );
        }
    }
}

#[test]
fn other_programs_toml_examples_parse() {
    let blocks = toml_blocks(|line| line.starts_with("```toml "));
    assert!(blocks.len() > 1, "found only {} examples", blocks.len());
    for (file, line, block) in blocks {
        if let Err(e) = toml::from_str::<toml::Table>(&block) {
            panic!("{file}:{line} is not valid TOML: {e}");
        }
    }
}

#[test]
fn the_pack_lints_clean_in_a_new_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let kb = dir.path().join("kb");
    okbot_knowledge::write_default_bundle(&kb).unwrap();
    let pack = okbot_knowledge::pack("okbot-guide").unwrap();

    let first = okbot_knowledge::install_pack(pack, &kb).unwrap();
    assert!(first.indexed);
    assert_eq!(first.unchanged, Vec::<PathBuf>::new());
    assert!(
        first
            .written
            .contains(&PathBuf::from("knowledge/okbot-guide/index.md"))
    );

    // Installing again changes nothing and links the pack once.
    let again = okbot_knowledge::install_pack(pack, &kb).unwrap();
    assert_eq!(again.written, Vec::<PathBuf>::new());
    assert!(!again.indexed);
    let index = std::fs::read_to_string(kb.join("knowledge/index.md")).unwrap();
    assert_eq!(index.matches("(okbot-guide/index.md)").count(), 1);

    // A stale copy is replaced; the user's own files are left alone.
    std::fs::write(kb.join("knowledge/okbot-guide/zalo.md"), "old").unwrap();
    std::fs::write(kb.join("knowledge/okbot-guide/mine.txt"), "mine").unwrap();
    let upgrade = okbot_knowledge::install_pack(pack, &kb).unwrap();
    assert_eq!(
        upgrade.written,
        [PathBuf::from("knowledge/okbot-guide/zalo.md")]
    );
    assert!(kb.join("knowledge/okbot-guide/mine.txt").exists());
    std::fs::remove_file(kb.join("knowledge/okbot-guide/mine.txt")).unwrap();

    let bundle = okbase::Bundle::open(
        &kb,
        okbase::OpenOptions {
            state_dir: okbase::StateDir::Path(dir.path().join("index")),
            ..okbase::OpenOptions::default()
        },
    )
    .unwrap();
    bundle.sync().unwrap();
    let report = bundle
        .lint(&okbase::LintConfig::level(okbase::Level::L2))
        .unwrap();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["errors"], 0, "{json:#}");
    assert_eq!(json["warnings"], 0, "{json:#}");
}
