//! Finding Codex CLI rollout files under `<root>/sessions` and `<root>/archived_sessions`.
//!
//! # Layout
//!
//! - `<root>/sessions/<YYYY>/<MM>/<DD>/rollout-<timestamp>-<uuid>.jsonl`
//! - `<root>/archived_sessions/rollout-<timestamp>-<uuid>.jsonl`
//!
//! The modification-time filter is what makes a full-tree walk affordable. A file last
//! written before the window opened cannot contain a record inside it, so it is never
//! opened at all -- only its directory entry is read.

use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Every `rollout-*.jsonl` file touched at or after `cutoff` (Unix epoch milliseconds),
/// under both the dated `sessions` tree and the flat `archived_sessions` directory.
#[must_use]
pub(crate) fn discover(root: &Path, cutoff: u64) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(&root.join("sessions"), cutoff, &mut found);
    walk(&root.join("archived_sessions"), cutoff, &mut found);
    found
}

/// Recurse into `dir`, collecting every fresh `rollout-*.jsonl` file at any depth. The
/// `sessions` tree is three levels deep (`YYYY/MM/DD`) and `archived_sessions` is flat;
/// walking generically covers both without hardcoding either shape.
fn walk(dir: &Path, cutoff: u64, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            walk(&path, cutoff, found);
            continue;
        }

        let is_rollout = path
            .file_stem()
            .and_then(|n| n.to_str())
            .is_some_and(|stem| stem.starts_with("rollout-"))
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"));

        if !is_rollout {
            continue;
        }

        let modified = entry
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

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("harness-metrics-codex-discover-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn rollouts_are_found_in_the_dated_tree_and_the_archive() {
        let root = tempdir("both");
        write(
            &root.join("sessions/2026/01/15/rollout-2026-01-15T00-00-00-abc.jsonl"),
            "{}\n",
        );
        write(
            &root.join("archived_sessions/rollout-old-def.jsonl"),
            "{}\n",
        );
        write(
            &root.join("sessions/2026/01/15/not-a-rollout.jsonl"),
            "{}\n",
        );

        let found = discover(&root, 0);

        assert_eq!(found.len(), 2);
        assert!(
            found
                .iter()
                .any(|p| p.ends_with("rollout-2026-01-15T00-00-00-abc.jsonl"))
        );
        assert!(found.iter().any(|p| p.ends_with("rollout-old-def.jsonl")));
        assert!(!found.iter().any(|p| p.ends_with("not-a-rollout.jsonl")));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_older_than_the_cutoff_are_skipped() {
        let root = tempdir("cutoff");
        write(&root.join("sessions/2026/01/15/rollout-a.jsonl"), "{}\n");

        let far_future = u64::MAX - 1;
        assert!(discover(&root, far_future).is_empty());
        assert_eq!(discover(&root, 0).len(), 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_tree_yields_nothing() {
        assert!(discover(Path::new("/definitely/not/a/real/codex/root"), 0).is_empty());
    }
}
