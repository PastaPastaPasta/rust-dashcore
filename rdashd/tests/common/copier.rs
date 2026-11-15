//! CH5-5's copier (§6.3.5): Core's `dynamically_initialize_datadir` copy of a node's regtest
//! directory, with the test in control. [`PausedCopy::list`] lists the whole tree under the kept
//! directories first; [`PausedCopy::run`] pauses at [`Pause::Listed`] and before each file, and
//! copies in the order the test gives. A listed file that has vanished is an error: the contract
//! says no file the copy needs is removed meanwhile.
// Core: test_framework.py:627-650 (copy), :640-642 (the kept directories)

use std::path::{Path, PathBuf};

/// The directories of `regtest/` the copy keeps, as Core's framework keeps them.
pub const KEPT: [&str; 4] = ["blocks", "chainstate", "evodb", "indexes"];

/// Where a copy pauses.
#[derive(Debug)]
pub enum Pause<'a> {
    /// The tree is listed; no file is copied yet.
    Listed,
    /// Before the file at this path, relative to the datadir.
    Before(&'a Path),
}

/// A listed copy of `src`'s kept directories to `dst`.
pub struct PausedCopy {
    src: PathBuf,
    dst: PathBuf,
    /// The files, relative to the datadir.
    pub files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl PausedCopy {
    /// Lists every file and directory under `src/regtest/{KEPT}`.
    ///
    /// # Errors
    ///
    /// A directory could not be read.
    pub fn list(src: &Path, dst: &Path) -> Result<PausedCopy, String> {
        let mut copy = PausedCopy {
            src: src.to_owned(),
            dst: dst.to_owned(),
            files: Vec::new(),
            dirs: Vec::new(),
        };
        for kept in KEPT {
            let rel = Path::new("regtest").join(kept);
            if src.join(&rel).is_dir() {
                copy.walk(&rel)?;
            }
        }
        Ok(copy)
    }

    fn walk(&mut self, rel: &Path) -> Result<(), String> {
        self.dirs.push(rel.to_owned());
        let dir = self.src.join(rel);
        let mut entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .map(|e| e.map_err(|e| format!("{}: {e}", dir.display())))
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let rel = rel.join(entry.file_name());
            let kind = entry.file_type().map_err(|e| format!("{}: {e}", rel.display()))?;
            if kind.is_dir() {
                self.walk(&rel)?;
            } else {
                self.files.push(rel);
            }
        }
        Ok(())
    }

    /// Copies the listed files in the order `order` leaves them, calling `pause` once the tree is
    /// listed and before each file.
    ///
    /// # Errors
    ///
    /// A listed file vanished, or a copy failed.
    pub fn run(
        mut self,
        order: impl FnOnce(&mut Vec<PathBuf>),
        mut pause: impl FnMut(Pause<'_>),
    ) -> Result<(), String> {
        for dir in &self.dirs {
            let to = self.dst.join(dir);
            std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        pause(Pause::Listed);
        order(&mut self.files);
        for file in &self.files {
            pause(Pause::Before(file));
            let from = self.src.join(file);
            if !from.exists() {
                return Err(format!("{} vanished during the copy", file.display()));
            }
            std::fs::copy(&from, self.dst.join(file))
                .map_err(|e| format!("{}: {e}", file.display()))?;
        }
        Ok(())
    }
}

/// A stopped node's copy: [`PausedCopy`] in listing order, never pausing.
///
/// # Panics
///
/// The copy failed.
pub fn copy_stopped(src: &Path, dst: &Path) {
    PausedCopy::list(src, dst).and_then(|c| c.run(|_| (), |_| ())).expect("the copy");
}
