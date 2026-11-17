//! Running as a macOS app bundle (MA3-OSC.app).
//!
//! An app started from Finder or Spotlight runs in `/`, and the bundle itself must
//! not be written to (that would break its code signature). So the app keeps its
//! config, mappings and logs in `~/Library/Application Support/MA3-OSC`. On the
//! first start, the starter files in `Contents/Resources` are copied there.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

const CONFIG: &str = "ma3-osc.json";

/// `~/Library/Application Support/MA3-OSC`
pub fn support_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/MA3-OSC"))
}

/// `Contents/Resources` of the app bundle `exe` lives in, if it is in one.
fn bundle_resources(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let in_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && contents.parent()?.extension()? == "app";
    in_bundle.then(|| contents.join("Resources"))
}

/// The config in Application Support. When running from an app bundle and it
/// doesn't exist yet, it is created from the bundle's starter files.
pub fn user_config(exe: Option<&Path>) -> Option<PathBuf> {
    let dir = support_dir()?;
    let cfg = dir.join(CONFIG);
    if cfg.exists() {
        return Some(cfg);
    }
    let resources = bundle_resources(exe?)?;
    if let Err(e) = seed(&resources, &dir) {
        // the tray will show "cannot read config" with this path
        eprintln!("warning: cannot set up {}: {e:#}", dir.display());
    }
    Some(cfg)
}

/// Copy the starter config, mappings and MA plugin into `dir`.
/// Existing files are never overwritten.
fn seed(resources: &Path, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    for name in ["mappings", "plugin"] {
        let src = resources.join(name);
        if src.is_dir() {
            copy_missing(&src, &dir.join(name))?;
        }
    }
    // the config goes last: once it exists, setup is complete
    let (from, to) = (resources.join(CONFIG), dir.join(CONFIG));
    std::fs::copy(&from, &to).with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
    Ok(())
}

fn copy_missing(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("cannot create {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("cannot read {}", src.display()))? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_missing(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to).with_context(|| format!("cannot copy {}", from.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_bundle() {
        let exe = Path::new("/Applications/MA3-OSC.app/Contents/MacOS/ma3-osc");
        assert_eq!(bundle_resources(exe), Some(PathBuf::from("/Applications/MA3-OSC.app/Contents/Resources")));
        assert_eq!(bundle_resources(Path::new("/repo/target/release/ma3-osc")), None);
        assert_eq!(bundle_resources(Path::new("/x/Contents/MacOS/ma3-osc")), None);
    }

    #[test]
    fn seed_copies_without_overwriting() {
        let root = std::env::temp_dir().join(format!("ma3-osc-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (res, dir) = (root.join("Resources"), root.join("support"));
        std::fs::create_dir_all(res.join("mappings")).unwrap();
        std::fs::write(res.join(CONFIG), "{}").unwrap();
        std::fs::write(res.join("mappings/a.json"), "new").unwrap();
        std::fs::create_dir_all(dir.join("mappings")).unwrap();
        std::fs::write(dir.join("mappings/a.json"), "edited").unwrap();

        seed(&res, &dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(CONFIG)).unwrap(), "{}");
        assert_eq!(std::fs::read_to_string(dir.join("mappings/a.json")).unwrap(), "edited");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
