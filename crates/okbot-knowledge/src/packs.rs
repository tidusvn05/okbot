//! Knowledge packs: ready-made folders of knowledge that `okbot kb pack add` copies into a bundle.
//!
//! A pack owns `knowledge/<name>/`. Installing it again replaces the pack's files with the ones of
//! this okbot version, so the pack always describes the binary that installed it.

use std::path::{Path, PathBuf};

use include_dir::{Dir, DirEntry, include_dir};

static OKBOT_GUIDE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/packs/okbot-guide");

/// A pack shipped with okbot.
#[derive(Debug)]
pub struct Pack {
    /// Folder name under `knowledge/`.
    pub name: &'static str,
    /// One sentence for `okbot kb pack list` and the knowledge index.
    pub description: &'static str,
    files: &'static Dir<'static>,
}

/// Every pack shipped with this version.
pub static PACKS: &[Pack] = &[Pack {
    name: "okbot-guide",
    description: "How to set up and run okbot: platforms, agents, permissions, services through \
                  MCP, automations, notifications and deployment.",
    files: &OKBOT_GUIDE,
}];

/// What installing a pack changed, as paths relative to the bundle root.
#[derive(Debug, Default)]
pub struct PackInstall {
    /// New or changed files.
    pub written: Vec<PathBuf>,
    /// Files already identical to the pack's.
    pub unchanged: Vec<PathBuf>,
    /// `knowledge/index.md` gained a link to the pack.
    pub indexed: bool,
}

/// Finds a pack by name.
#[must_use]
pub fn pack(name: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|p| p.name == name)
}

/// Writes `pack` into the bundle at `kb`, replacing the pack's own files, and links it from
/// `knowledge/index.md` when that index does not mention it yet. Other files are never touched.
pub fn install_pack(pack: &Pack, kb: &Path) -> std::io::Result<PackInstall> {
    let root = kb.join("knowledge").join(pack.name);
    let mut out = PackInstall::default();
    write_dir(pack.files, &root, &mut out)?;
    let prefix = Path::new("knowledge").join(pack.name);
    for p in out.written.iter_mut().chain(out.unchanged.iter_mut()) {
        *p = prefix.join(&*p);
    }
    out.written.sort();
    out.unchanged.sort();

    let index = kb.join("knowledge/index.md");
    let link = format!("({}/index.md)", pack.name);
    let text = std::fs::read_to_string(&index).unwrap_or_else(|_| "# Index: knowledge\n".into());
    if !text.contains(&link) {
        let text = format!(
            "{}\n* [{}]({}/index.md) - {}\n",
            text.trim_end(),
            pack.name,
            pack.name,
            pack.description
        );
        std::fs::write(&index, text)?;
        out.indexed = true;
    }
    Ok(out)
}

fn write_dir(source: &Dir<'_>, target: &Path, out: &mut PackInstall) -> std::io::Result<()> {
    for entry in source.entries() {
        match entry {
            DirEntry::Dir(d) => {
                std::fs::create_dir_all(target.join(d.path()))?;
                write_dir(d, target, out)?;
            }
            DirEntry::File(f) => {
                let path = target.join(f.path());
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if std::fs::read(&path).is_ok_and(|old| old == f.contents()) {
                    out.unchanged.push(f.path().to_path_buf());
                    continue;
                }
                std::fs::write(&path, f.contents())?;
                out.written.push(f.path().to_path_buf());
            }
        }
    }
    Ok(())
}
