//! Per-user path assembly. The single place anywhere in the codebase that
//! turns a (user, subdir, filename) tuple into an on-disk path. Refuses
//! filenames containing `..` or path separators — the only traversal
//! prevention that has to exist.

use std::path::{Path, PathBuf};

use crate::state::UserId;

/// Subdirectory name under `data/users/<uid>/`. Keep this small and known.
pub mod subdir {
    pub const CACHE_INPUTS: &str = "cache/inputs";
    pub const OUTPUTS: &str = "outputs";
    pub const THUMBS: &str = "thumbs";
    pub const PREVIEWS: &str = "previews";
}

/// Build `<data_dir>/users/<user_id>/<subdir>/<filename>`. Rejects any
/// filename that would escape the user dir.
pub fn user_data_path(
    data_dir: &Path,
    user_id: UserId,
    subdir: &str,
    filename: &str,
) -> anyhow::Result<PathBuf> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        anyhow::bail!("invalid filename for per-user path: {filename:?}");
    }
    Ok(data_dir
        .join("users")
        .join(user_id.0.to_string())
        .join(subdir)
        .join(filename))
}

/// Path to the per-user subdirectory itself (no filename). Used by purge
/// and tests to enumerate or clean.
pub fn user_subdir(data_dir: &Path, user_id: UserId, subdir: &str) -> PathBuf {
    data_dir
        .join("users")
        .join(user_id.0.to_string())
        .join(subdir)
}

/// Write `bytes` to `path` via a temp sibling + rename. A crash mid-write
/// leaves the target either untouched or fully written — never half-written.
pub async fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_sibling(path);
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

/// Copy `src` to `dst` via a temp sibling + rename. Same crash-safety as
/// `atomic_write`.
pub async fn atomic_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    let tmp = tmp_sibling(dst);
    tokio::fs::copy(src, &tmp).await?;
    tokio::fs::rename(&tmp, dst).await
}

fn tmp_sibling(path: &Path) -> PathBuf {
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
        let u = UserId(1);
        assert!(user_data_path(dd, u, subdir::OUTPUTS, "../oops").is_err());
        assert!(user_data_path(dd, u, subdir::OUTPUTS, "a/b").is_err());
        assert!(user_data_path(dd, u, subdir::OUTPUTS, "").is_err());
    }

    #[test]
    fn builds_expected_path() {
        let dd = Path::new("/data");
        let p = user_data_path(dd, UserId(1), subdir::CACHE_INPUTS, "abc.jpg").unwrap();
        assert_eq!(p, Path::new("/data/users/1/cache/inputs/abc.jpg"));
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
