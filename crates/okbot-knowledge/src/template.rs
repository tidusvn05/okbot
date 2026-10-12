//! The default bundle written by `okbot init` (design §5.1).

use std::path::{Path, PathBuf};

use include_dir::{Dir, DirEntry, include_dir};

static TEMPLATE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/template");

/// Writes the default bundle into `dir`, creating it if needed. Existing files are never
/// overwritten. Returns the files written, relative to `dir`.
pub fn write_default_bundle(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    write_dir(&TEMPLATE, dir, &mut written)?;
    written.sort();
    Ok(written)
}

fn write_dir(source: &Dir<'_>, target: &Path, written: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in source.entries() {
        match entry {
            DirEntry::Dir(d) => {
                std::fs::create_dir_all(target.join(d.path()))?;
                write_dir(d, target, written)?;
            }
            DirEntry::File(f) => {
                let path = target.join(f.path());
                if path.exists() {
                    continue;
                }
                std::fs::write(&path, f.contents())?;
                written.push(f.path().to_path_buf());
            }
        }
    }
    Ok(())
}

/// The template's files, relative to the bundle root (for `okbot kb upgrade` and tests).
pub fn default_bundle_files() -> Vec<PathBuf> {
    fn walk(d: &Dir<'_>, out: &mut Vec<PathBuf>) {
        for entry in d.entries() {
            match entry {
                DirEntry::Dir(sub) => walk(sub, out),
                DirEntry::File(f) => out.push(f.path().to_path_buf()),
            }
        }
    }
    let mut out = Vec::new();
    walk(&TEMPLATE, &mut out);
    out.sort();
    out
}
