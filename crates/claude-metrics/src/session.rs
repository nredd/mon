//! The live session registry at `~/.claude/sessions/<PID>.json`.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

/// One live Claude Code session, as recorded by its own process.
///
/// Every field past `pid` is optional: the registry file is written by a running process
/// and can be read mid-write, and its schema is undocumented.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Session {
    /// OS process id. Also the registry filename.
    pub pid: i32,
    /// The session's UUID, which names its transcript file.
    pub session_id: Option<String>,
    /// Working directory the session was started in.
    pub cwd: Option<String>,
    /// Unix epoch milliseconds.
    pub started_at: Option<u64>,
    /// Claude Code version string.
    pub version: Option<String>,
    /// `interactive`, and others.
    pub kind: Option<String>,
    /// How it was launched, e.g. `cli`.
    pub entrypoint: Option<String>,
    /// tmux location as `session:@window.%pane`, when running under tmux.
    pub tmux: Option<String>,
    /// Human-facing session name.
    pub name: Option<String>,
    /// `busy`, `idle`, and others.
    pub status: Option<String>,
    /// Unix epoch milliseconds of the last update.
    pub updated_at: Option<u64>,
}

impl Session {
    /// Whether the owning process is still alive.
    ///
    /// Sessions are not always cleaned up on exit -- a killed process leaves its registry
    /// file behind -- so liveness has to be checked rather than assumed.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        process_is_alive(self.pid)
    }

    /// Whether the session reports itself as actively working.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.status.as_deref() == Some("busy")
    }

    /// The tmux pane id (`%28`) this session runs in, if any.
    #[must_use]
    pub fn tmux_pane(&self) -> Option<&str> {
        self.tmux.as_ref()?.rsplit('.').next()
    }
}

/// `kill(pid, 0)`: succeeds if the process exists and we may signal it, and fails with
/// `EPERM` if it exists but we may not. Both mean alive.
#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    // SAFETY: `kill` with signal 0 performs the permission and existence checks without
    // delivering a signal. It cannot affect the target process.
    let result = unsafe { libc::kill(pid, 0) };

    if result == 0 {
        return true;
    }

    // EPERM means the process exists but belongs to someone else.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: i32) -> bool {
    // No portable equivalent; assume alive rather than hiding live sessions.
    true
}

/// Read every live session out of a registry directory.
///
/// Unreadable or unparseable entries are skipped. A registry that does not exist yet is
/// simply empty -- that is the normal state on a machine with no sessions running.
#[must_use]
pub fn read_registry(dir: &Path) -> Vec<Session> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut sessions: Vec<Session> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();

            // The directory also holds `<pid>.<hash>.key` files, which are not sessions.
            if path.extension()?.to_str()? != "json" {
                return None;
            }

            let contents = fs::read_to_string(&path).ok()?;
            let session: Session = serde_json::from_str(&contents).ok()?;

            session.is_alive().then_some(session)
        })
        .collect();

    // Newest first, with a stable tiebreak so the table does not shuffle between frames.
    sessions.sort_unstable_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.pid.cmp(&b.pid))
    });

    sessions
}

/// Encode a working directory the way `~/.claude/projects` names its subdirectories.
///
/// The encoding is inferred from observed directory names, not documented, so callers
/// should treat a miss as ordinary and fall back to searching.
#[must_use]
pub fn project_slug(cwd: &str) -> String {
    cwd.chars()
        .map(|c| {
            if c == '/' || c == '.' || c == '_' {
                '-'
            } else {
                c
            }
        })
        .collect()
}

/// Locate a session's transcript under `<root>/projects`.
///
/// Tries the slug derived from `cwd` first, then falls back to scanning every project
/// directory. The fallback matters: the slug encoding is inferred, and a session whose
/// `cwd` moved still has its transcript under the original slug.
#[must_use]
pub fn find_transcript(root: &Path, session_id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    let projects = root.join("projects");
    let filename = format!("{session_id}.jsonl");

    if let Some(cwd) = cwd {
        let direct = projects.join(project_slug(cwd)).join(&filename);
        if direct.is_file() {
            return Some(direct);
        }
    }

    fs::read_dir(&projects).ok()?.flatten().find_map(|entry| {
        let candidate = entry.path().join(&filename);
        candidate.is_file().then_some(candidate)
    })
}

/// Locate a session's subagent transcripts.
#[must_use]
pub fn find_subagent_transcripts(root: &Path, session_id: &str, cwd: Option<&str>) -> Vec<PathBuf> {
    let projects = root.join("projects");

    let dirs = cwd
        .map(|cwd| vec![projects.join(project_slug(cwd)).join(session_id)])
        .unwrap_or_default()
        .into_iter()
        .chain(
            fs::read_dir(&projects)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path().join(session_id)),
        );

    for dir in dirs {
        let subagents = dir.join("subagents");
        let Ok(entries) = fs::read_dir(&subagents) else {
            continue;
        };

        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("agent-"))
            })
            .collect();

        if !found.is_empty() {
            found.sort_unstable();
            return found;
        }
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::io::Write;

    use super::*;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "claude-metrics-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn the_current_process_reads_as_alive_and_pid_one_does_not_read_as_dead() {
        let me = Session {
            pid: i32::try_from(std::process::id()).unwrap_or(-1),
            ..Default::default()
        };
        assert!(me.is_alive());

        // pid 1 exists but belongs to root: the EPERM branch must still say "alive".
        let init = Session {
            pid: 1,
            ..Default::default()
        };
        assert!(init.is_alive(), "EPERM means alive, not dead");

        let bogus = Session {
            pid: -1,
            ..Default::default()
        };
        assert!(!bogus.is_alive());
    }

    #[test]
    fn dead_sessions_and_non_json_entries_are_pruned() {
        let dir = tempdir();
        let live = std::process::id();

        write(
            &dir.join(format!("{live}.json")),
            &format!(r#"{{"pid":{live},"sessionId":"live","status":"busy","startedAt":2}}"#),
        );
        // A pid that cannot be running: far above any real pid_max.
        write(
            &dir.join("2147483646.json"),
            r#"{"pid":2147483646,"sessionId":"dead","startedAt":1}"#,
        );
        write(&dir.join("12345.abc.key"), "not json");
        write(&dir.join("garbage.json"), "{{{{");

        let sessions = read_registry(&dir);
        assert_eq!(sessions.len(), 1, "only the live session survives");
        assert_eq!(sessions[0].session_id.as_deref(), Some("live"));
        assert!(sessions[0].is_busy());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcripts_are_found_by_slug_and_by_fallback_scan() {
        let root = tempdir();
        write(
            &root.join("projects/-Users-redd-code/abc.jsonl"),
            "{\"type\":\"user\"}\n",
        );
        write(
            &root.join("projects/-some-other-place/moved.jsonl"),
            "{\"type\":\"user\"}\n",
        );

        let by_slug = find_transcript(&root, "abc", Some("/Users/redd/code"));
        assert!(by_slug.is_some(), "the derived slug must resolve");

        // The slug encoding is inferred, so a wrong or stale `cwd` has to fall back.
        let by_scan = find_transcript(&root, "moved", Some("/Users/redd/code"));
        assert!(
            by_scan.is_some(),
            "a transcript under an unexpected slug must still be found"
        );

        assert!(find_transcript(&root, "nope", None).is_none());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn subagent_transcripts_are_found_and_sorted() {
        let root = tempdir();
        let base = root.join("projects/-Users-redd-code/sess/subagents");
        write(&base.join("agent-bbb.jsonl"), "{}\n");
        write(&base.join("agent-aaa.jsonl"), "{}\n");
        write(&base.join("agent-aaa.meta.json"), "{}");

        let found = find_subagent_transcripts(&root, "sess", Some("/Users/redd/code"));
        assert_eq!(found.len(), 2, "only the .jsonl files, not the .meta.json");
        assert!(found[0].ends_with("agent-aaa.jsonl"), "must be sorted");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn project_slug_matches_the_observed_encoding() {
        assert_eq!(project_slug("/Users/redd/code"), "-Users-redd-code");
        assert_eq!(
            project_slug("/Users/redd/.local/share/chezmoi"),
            "-Users-redd--local-share-chezmoi"
        );
    }
}
