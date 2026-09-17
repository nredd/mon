//! Finding pi session files under `<root>/sessions/<slug>/<timestamp>_<uuid>.jsonl`.
//!
//! The modification-time filter is what makes a full-tree walk affordable. A file last
//! written before the window opened cannot contain a record inside it, so it is never
//! opened at all -- only its directory entry is read.

use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Every `*.jsonl` file one level under `<root>/sessions` touched at or after `cutoff`
/// (Unix epoch milliseconds).
#[must_use]
pub(crate) fn discover(root: &Path, cutoff: u64) -> Vec<PathBuf> {
    let sessions = root.join("sessions");
    let mut found = Vec::new();

    let Ok(slugs) = std::fs::read_dir(&sessions) else {
        return found;
    };

    for slug in slugs.flatten() {
        let Ok(files) = std::fs::read_dir(slug.path()) else {
            continue;
        };

        for file in files.flatten() {
            let path = file.path();

            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }

            let modified = file
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));

            // A file with no readable mtime is read rather than skipped. Being wrong in the
            // cheap direction costs one parse; being wrong the other way loses data.
            if modified.is_none_or(|stamp| stamp >= cutoff) {
                found.push(path);
            }
        }
    }

    found
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("harness-metrics-pi-discover-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn session_files_one_level_deep_are_found() {
        let root = tempdir("found");
        write(
            &root.join("sessions/my-project/2026-01-01T00-00-00_abc.jsonl"),
            "{}\n",
        );
        write(&root.join("sessions/my-project/notes.txt"), "hi");

        let found = discover(&root, 0);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("2026-01-01T00-00-00_abc.jsonl"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_older_than_the_cutoff_are_skipped() {
        let root = tempdir("cutoff");
        write(&root.join("sessions/my-project/old.jsonl"), "{}\n");

        let far_future = u64::MAX - 1;
        assert!(discover(&root, far_future).is_empty());
        assert_eq!(discover(&root, 0).len(), 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_tree_yields_nothing() {
        assert!(discover(Path::new("/definitely/not/a/real/pi/root"), 0).is_empty());
    }
}
