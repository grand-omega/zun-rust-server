//! Data path assembly. The single place anywhere in the codebase that
//! turns a (subdir, filename) tuple into an on-disk path. Refuses
//! filenames containing `..` or path separators — the only traversal
//! prevention that has to exist.

use std::path::{Path, PathBuf};

/// Subdirectory name under `data/`. Keep this small and known.
pub mod subdir {
    pub const CACHE_INPUTS: &str = "cache/inputs";
    pub const OUTPUTS: &str = "outputs";
    pub const THUMBS: &str = "thumbs";
    pub const PREVIEWS: &str = "previews";
}

/// Express an absolute on-disk path as a `data_dir`-relative string for
/// storing in the DB (`output_path`, `inputs.path`, etc). Falls back to the
/// absolute path's lossy form if the prefix doesn't strip — that should
/// never happen in practice, but we don't want a broken path to fail a job.
pub fn relative_for_db(abs: &Path, data_dir: &Path) -> String {
    abs.strip_prefix(data_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs.to_string_lossy().into_owned())
}

/// Build `<data_dir>/<subdir>/<filename>`. Rejects any filename that would
/// escape the configured data dir.
pub fn data_path(data_dir: &Path, subdir: &str, filename: &str) -> anyhow::Result<PathBuf> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        anyhow::bail!("invalid filename for data path: {filename:?}");
    }
    Ok(data_dir.join(subdir).join(filename))
}

/// A staging file that removes itself unless it is renamed into place.
///
/// Every atomic write here is "fill a temp sibling, then rename". The part
/// that kept getting forgotten is the failure path: an early return between
/// those two steps left the temp file on disk, and nothing collects it —
/// `backup::prune_old` filters on the `.db` extension, which a
/// `.db.tmp.a1b2c3d4` name does not match, and `purge` only deletes paths
/// recorded in the DB, which a temp file never reaches. Worse, the likeliest
/// reason to fail here is a full disk, so the leak lands exactly when the
/// space is needed.
///
/// Making that cleanup a `Drop` rather than a line each call site has to
/// remember is the point: forgetting is no longer possible. Callers commit
/// after the rename succeeds.
pub struct Staged {
    path: PathBuf,
    committed: bool,
}

impl Staged {
    /// Stage a temp sibling of `dest`.
    pub fn new(dest: &Path) -> Self {
        Self {
            path: tmp_sibling(dest),
            committed: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The rename landed; the staging path is gone, so stop tracking it.
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.committed {
            // Blocking unlink on an error path: one syscall, and there is no
            // async Drop. Failing to clean up is ignored — we are already
            // returning the more interesting error.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Write `bytes` to `path` via a temp sibling + rename. A crash mid-write
/// leaves the target either untouched or fully written — never half-written.
pub async fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut staged = Staged::new(path);
    tokio::fs::write(staged.path(), bytes).await?;
    tokio::fs::rename(staged.path(), path).await?;
    staged.commit();
    Ok(())
}

/// Blocking twin of [`atomic_write`], for callers already inside
/// `spawn_blocking` (image encoding, say) where the tokio file APIs are the
/// wrong tool. One implementation of the invariant, two entry points —
/// `derived_images::render_only` used to carry its own copy.
pub fn atomic_write_blocking(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut staged = Staged::new(path);
    std::fs::write(staged.path(), bytes)?;
    std::fs::rename(staged.path(), path)?;
    staged.commit();
    Ok(())
}

/// Copy `src` to `dst` via a temp sibling + rename. Same crash-safety as
/// `atomic_write`.
pub async fn atomic_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut staged = Staged::new(dst);
    tokio::fs::copy(src, staged.path()).await?;
    tokio::fs::rename(staged.path(), dst).await?;
    staged.commit();
    Ok(())
}

/// Build a randomly-suffixed temp sibling path for `path`, e.g.
/// `foo.db` -> `foo.db.tmp.a1b2c3d4`. Shared by `atomic_write`/`atomic_copy`
/// and by callers (like `backup::snapshot_once`) that need to stage a file
/// via a non-`tokio::fs` write API before renaming it into place.
pub fn tmp_sibling(path: &Path) -> PathBuf {
    let suffix: u32 = rand::random();
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp.{suffix:08x}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_separators() {
        let dd = Path::new("/data");
        assert!(data_path(dd, subdir::OUTPUTS, "../oops").is_err());
        assert!(data_path(dd, subdir::OUTPUTS, "a/b").is_err());
        assert!(data_path(dd, subdir::OUTPUTS, "").is_err());
    }

    #[test]
    fn builds_expected_path() {
        let dd = Path::new("/data");
        let p = data_path(dd, subdir::CACHE_INPUTS, "abc.jpg").unwrap();
        assert_eq!(p, Path::new("/data/cache/inputs/abc.jpg"));
    }

    #[tokio::test]
    async fn atomic_write_persists_bytes_at_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.bin");
        atomic_write(&target, b"hello").await.unwrap();
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"hello");
        // No leftover .tmp.* in the parent directory.
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut count = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            assert!(!e.file_name().to_string_lossy().contains(".tmp."));
            count += 1;
        }
        assert_eq!(count, 1);
    }

    #[test]
    fn atomic_write_blocking_persists_bytes_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.bin");
        atomic_write_blocking(&target, b"hello").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    /// Count leftover `.tmp.*` files in a directory.
    fn stray_tmp_files(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")
            })
            .count()
    }

    /// A destination that is a non-empty directory makes `rename` fail while
    /// the staging write succeeds — the exact window where the temp file used
    /// to be abandoned, and the one no cleanup elsewhere would ever collect.
    fn unrenameable_target(dir: &Path) -> PathBuf {
        let target = dir.join("occupied");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("child"), b"x").unwrap();
        target
    }

    #[tokio::test]
    async fn atomic_write_removes_its_staging_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let target = unrenameable_target(dir.path());
        assert!(atomic_write(&target, b"hello").await.is_err());
        assert_eq!(stray_tmp_files(dir.path()), 0, "staging file was abandoned");
    }

    #[test]
    fn atomic_write_blocking_removes_its_staging_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let target = unrenameable_target(dir.path());
        assert!(atomic_write_blocking(&target, b"hello").is_err());
        assert_eq!(stray_tmp_files(dir.path()), 0, "staging file was abandoned");
    }

    #[tokio::test]
    async fn atomic_copy_removes_its_staging_file_when_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        std::fs::write(&src, b"hi").unwrap();
        let target = unrenameable_target(dir.path());
        assert!(atomic_copy(&src, &target).await.is_err());
        assert_eq!(stray_tmp_files(dir.path()), 0, "staging file was abandoned");
    }

    #[tokio::test]
    async fn atomic_copy_persists_bytes_at_target() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dst = dir.path().join("dst.bin");
        tokio::fs::write(&src, b"hi").await.unwrap();
        atomic_copy(&src, &dst).await.unwrap();
        assert_eq!(tokio::fs::read(&dst).await.unwrap(), b"hi");
    }
}
