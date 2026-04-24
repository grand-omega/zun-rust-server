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
}
