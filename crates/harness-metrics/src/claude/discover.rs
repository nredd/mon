//! Finding Claude Code transcripts under `~/.claude/projects`.
//!
//! # Layout
//!
//! - `<root>/projects/<cwd-slug>/<sessionId>.jsonl` -- a session's main transcript.
//! - `<root>/projects/<cwd-slug>/<sessionId>/subagents/agent-*.jsonl` -- its subagents.
//!
//! The `<cwd-slug>` encoding is inferred from observed directory names, not documented, so
//! discovery never tries to derive it -- it walks every project directory instead.
//!
//! The modification-time filter is what makes a full-tree walk affordable. A file last
//! written before the window opened cannot contain a record inside it, so it is never
//! opened at all -- only its directory entry is read.

use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

/// Every transcript -- main or subagent -- touched at or after `cutoff` (Unix epoch
/// milliseconds).
#[must_use]
pub(crate) fn discover(root: &Path, cutoff: u64) -> Vec<PathBuf> {
    let projects = root.join("projects");
    let mut found = Vec::new();

    let Ok(entries) = std::fs::read_dir(&projects) else {
        return found;
    };

    for project in entries.flatten() {
        let Ok(children) = std::fs::read_dir(project.path()) else {
            continue;
        };

        for child in children.flatten() {
            let path = child.path();

            if path.is_dir() {
                // A session-id directory holding subagent transcripts.
                collect_modified_since(&path.join("subagents"), "jsonl", cutoff, &mut found);
                continue;
            }

            if is_fresh_jsonl(&child, cutoff) {
                found.push(path);
            }
        }
    }

    found
}

/// Every `*.<ext>` directly inside `dir` touched at or after `cutoff`.
fn collect_modified_since(dir: &Path, ext: &str, cutoff: u64, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if entry.path().extension().is_some_and(|e| e == ext) && is_fresh(&entry, cutoff) {
            found.push(entry.path());
        }
    }
}

fn is_fresh_jsonl(entry: &std::fs::DirEntry, cutoff: u64) -> bool {
    entry.path().extension().is_some_and(|e| e == "jsonl") && is_fresh(entry, cutoff)
}

/// A file with no readable mtime is treated as fresh rather than skipped. Being wrong in
/// the cheap direction costs one parse; being wrong the other way loses data.
fn is_fresh(entry: &std::fs::DirEntry, cutoff: u64) -> bool {
    let modified = entry
        .metadata()
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));

    modified.is_none_or(|stamp| stamp >= cutoff)
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fs;

    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("harness-metrics-claude-discover-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn main_transcripts_and_subagent_transcripts_are_both_found() {
        let root = tempdir("both");
        write(&root.join("projects/-Users-redd-code/sess-1.jsonl"), "{}\n");
        write(
            &root.join("projects/-Users-redd-code/sess-1/subagents/agent-a.jsonl"),
            "{}\n",
        );
        write(
            &root.join("projects/-Users-redd-code/sess-1/subagents/agent-a.meta.json"),
            "{}",
        );

        let found = discover(&root, 0);

        assert_eq!(
            found.len(),
            2,
            "the main transcript and the one subagent transcript"
        );
        assert!(found.iter().any(|p| p.ends_with("sess-1.jsonl")));
        assert!(found.iter().any(|p| p.ends_with("agent-a.jsonl")));
        assert!(
            !found.iter().any(|p| p.ends_with("agent-a.meta.json")),
            "only .jsonl files"
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_older_than_the_cutoff_are_skipped() {
        let root = tempdir("cutoff");
        write(&root.join("projects/-Users-redd-code/old.jsonl"), "{}\n");

        // A cutoff far in the future: the fixture file was just written, so it is older.
        let far_future = u64::MAX - 1;
        assert!(discover(&root, far_future).is_empty());
        assert_eq!(discover(&root, 0).len(), 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_tree_yields_nothing() {
        assert!(discover(Path::new("/definitely/not/a/real/claude/root"), 0).is_empty());
    }
}
