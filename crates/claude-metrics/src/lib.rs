//! Reads live Claude Code metrics off the local `~/.claude` tree.
//!
//! This crate has no dependency on `bottom`. It hands back plain data; rendering lives in
//! `src/canvas/widgets/`.
//!
//! Everything here parses defensively. The `~/.claude` schema is undocumented, it drifts
//! between releases, and a schema surprise must never take a widget down -- every field is
//! optional, unknown fields are ignored, and an unreadable file is treated as absent rather
//! than as an error.
//!
//! # Layout it reads
//!
//! - `~/.claude/sessions/<PID>.json` -- the live session registry, pruned with `kill(pid, 0)`
//! - `~/.claude/projects/<cwd-slug>/<sessionId>.jsonl` -- the transcript
//! - `~/.claude/projects/<cwd-slug>/<sessionId>/subagents/agent-*.jsonl` -- subagents
//!
//! # Counting rules
//!
//! These are not obvious and getting any of them wrong inflates every number:
//!
//! - Dedupe on `requestId` + `message.id`. Retries and resumed sessions replay identical
//!   messages, so counting lines double-counts.
//! - A message with several content blocks carries one `usage` object and counts **once**.
//! - Take `cache_creation_input_tokens` alone, never plus the `cache_creation.ephemeral_*`
//!   buckets -- the former is exactly the sum of the latter.
//! - Ignore `usage.iterations[]`; it restates the message-level counts.
//! - Skip `<synthetic>` models and `isApiErrorMessage` records.
//! - `isSidechain: true` marks a subagent, which carries the **parent** session's id.
//! - A message is written as one record **per content block**. The per-request fields
//!   (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`) repeat
//!   identically across those records and are counted once; `output_tokens` is a **running
//!   total** and is tracked as a high-water mark.
//!
//! # Known limits
//!
//! Totals were calibrated against `~/.claude.json`'s `projects[cwd].lastModelUsage`, which
//! records real per-model counts for the last session in each project. Two gaps remain, and
//! both are properties of the data source rather than of this crate:
//!
//! - **Background Haiku calls never reach a transcript.** Session titles and similar
//!   internal calls are billed but not written to `~/.claude/projects`, so Haiku totals read
//!   as zero even when `lastModelUsage` shows a small amount. Nothing here can recover them.
//! - **A few percent of a long session's tokens can be missing.** On one 1199-line session
//!   the Opus input, cache-read, and cache-write figures matched `lastModelUsage` exactly
//!   while output landed at 97.7%; a short session matched on all four fields exactly.
//!
//! Treat the numbers as a close live estimate, not as billing truth.
//!
//! # Example
//!
//! ```no_run
//! use claude_metrics::ClaudeMetrics;
//!
//! let mut metrics = ClaudeMetrics::with_default_root().expect("no home directory");
//! metrics.refresh();
//!
//! for session in metrics.sessions() {
//!     println!("{:?} in {:?}", session.name, session.cwd);
//! }
//! ```

pub mod history;
pub mod model;
pub mod range;
pub mod session;
pub mod statusline;
pub mod tailer;
pub mod transcript;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

pub use history::{Bucket, REFRESH_CHUNK_BYTES, TokenHistory};
pub use model::ModelFamily;
pub use range::StatsRange;
pub use session::Session;
pub use statusline::Statusline;
pub use tailer::Tailer;
pub use transcript::{Record, TokenTotals, UsageAccumulator};

/// Per-session reading state, carried between refreshes.
#[derive(Debug)]
struct SessionState {
    main: Tailer,
    subagents: Vec<Tailer>,
    usage: UsageAccumulator,
    /// Set once the transcript has been located, so a miss is not retried every tick.
    located: bool,
}

/// Reads and accumulates Claude Code metrics from a `~/.claude` tree.
#[derive(Debug)]
pub struct ClaudeMetrics {
    root: PathBuf,
    sessions: Vec<Session>,
    states: HashMap<String, SessionState>,
}

impl ClaudeMetrics {
    /// Read from an explicit `~/.claude` root. Useful for tests and fixtures.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            sessions: Vec::new(),
            states: HashMap::new(),
        }
    }

    /// Read from `$HOME/.claude`.
    ///
    /// Returns `None` only when there is no home directory to derive a path from.
    #[must_use]
    pub fn with_default_root() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        Some(Self::new(Path::new(&home).join(".claude")))
    }

    /// The `~/.claude` root being read.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Re-read the registry and consume whatever the transcripts have appended.
    ///
    /// Cheap enough to call on every collection tick: the registry is a handful of small
    /// files, and transcripts are read from a checkpoint rather than re-parsed.
    pub fn refresh(&mut self) {
        self.sessions = session::read_registry(&self.root.join("sessions"));

        // Drop state for sessions that have gone away, so a long-running process does not
        // accumulate tailers for every session it has ever seen.
        let live: Vec<String> = self
            .sessions
            .iter()
            .filter_map(|s| s.session_id.clone())
            .collect();
        self.states.retain(|id, _| live.contains(id));

        for session in &self.sessions {
            let Some(session_id) = session.session_id.clone() else {
                continue;
            };
            let cwd = session.cwd.clone();

            let located = self.locate_transcript(&session_id, cwd.as_deref());

            let state = self
                .states
                .entry(session_id.clone())
                .or_insert_with(|| SessionState {
                    main: Tailer::new(PathBuf::new()),
                    subagents: Vec::new(),
                    usage: UsageAccumulator::default(),
                    located: false,
                });

            // A brand-new session's transcript may not exist yet when it first registers.
            if !state.located {
                let Some(path) = located else {
                    continue;
                };

                if state.main.path() != path {
                    state.main = Tailer::new(path);
                }
                state.located = true;
            }

            let (lines, kind) = state.main.read_new();

            // A replaced transcript means the accumulated totals describe a file that is
            // no longer there, so start the count over rather than mixing two files.
            if kind == tailer::ReadKind::Restarted {
                state.usage = UsageAccumulator::default();
            }

            for line in lines {
                if let Some(record) = Record::parse(&line) {
                    state.usage.ingest(&record);
                }
            }

            // Subagent files come and go during a session, so rescan rather than caching
            // the list. Existing tailers keep their offsets.
            let found = session::find_subagent_transcripts(&self.root, &session_id, cwd.as_deref());
            for path in found {
                if !state.subagents.iter().any(|t| t.path() == path) {
                    state.subagents.push(Tailer::new(path));
                }
            }

            for tailer in &mut state.subagents {
                let (lines, _) = tailer.read_new();
                for line in lines {
                    if let Some(record) = Record::parse(&line) {
                        state.usage.ingest(&record);
                    }
                }
            }
        }
    }

    /// Live sessions, newest first.
    #[must_use]
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// Token totals for one session, by model family.
    #[must_use]
    pub fn totals_for(&self, session_id: &str) -> Vec<(ModelFamily, TokenTotals)> {
        self.states
            .get(session_id)
            .map(|state| state.usage.totals())
            .unwrap_or_default()
    }

    /// Token totals across every live session, by model family.
    #[must_use]
    pub fn totals_by_model(&self) -> Vec<(ModelFamily, TokenTotals)> {
        let mut merged: HashMap<ModelFamily, TokenTotals> = HashMap::new();

        for state in self.states.values() {
            for (family, totals) in state.usage.totals() {
                let entry = merged.entry(family).or_default();
                entry.input = entry.input.saturating_add(totals.input);
                entry.output = entry.output.saturating_add(totals.output);
                entry.cache_read = entry.cache_read.saturating_add(totals.cache_read);
                entry.cache_creation = entry.cache_creation.saturating_add(totals.cache_creation);
            }
        }

        let mut totals: Vec<(ModelFamily, TokenTotals)> = merged.into_iter().collect();
        totals.sort_unstable_by_key(|(family, _)| *family);
        totals
    }

    /// Find a session's transcript.
    ///
    /// Prefers `transcript_path` out of the cached statusline payload, which is exact.
    /// Falls back to deriving the path from `cwd` and then to scanning, both of which rest
    /// on a slug encoding that is inferred rather than documented.
    fn locate_transcript(&self, session_id: &str, cwd: Option<&str>) -> Option<PathBuf> {
        if let Some(path) = statusline::read(&self.root, Some(session_id), cwd)
            .and_then(|s| s.transcript_path)
            .map(PathBuf::from)
            && path.is_file()
        {
            return Some(path);
        }

        session::find_transcript(&self.root, session_id, cwd)
    }

    /// The cached statusline payload for a session, if the tee has written one.
    ///
    /// This is the only place cost, context-window occupancy, and the 5h/7d rate limits are
    /// available. `None` means the tee is not installed or has not run yet, which is a
    /// normal state rather than an error.
    #[must_use]
    pub fn statusline_for(&self, session_id: &str) -> Option<Statusline> {
        let cwd = self
            .sessions
            .iter()
            .find(|s| s.session_id.as_deref() == Some(session_id))
            .and_then(|s| s.cwd.as_deref());

        statusline::read(&self.root, Some(session_id), cwd)
    }

    /// How many subagent messages one session has produced.
    #[must_use]
    pub fn subagent_messages(&self, session_id: &str) -> u64 {
        self.states
            .get(session_id)
            .map_or(0, |state| state.usage.sidechain_messages)
    }

    /// The most recent turn duration for one session, in milliseconds.
    #[must_use]
    pub fn last_turn_duration_ms(&self, session_id: &str) -> Option<u64> {
        self.states.get(session_id)?.usage.last_turn_duration_ms
    }
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::{fs, io::Write, path::PathBuf};

    use super::*;

    fn fixture_root(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("claude-metrics-lib-{tag}"));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path)
            .unwrap()
            .write_all(contents.as_bytes())
            .unwrap();
    }

    fn assistant(request: &str, message: &str, model: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","requestId":"{request}","sessionId":"sess-1","isSidechain":false,"message":{{"id":"{message}","model":"{model}","content":[{{"type":"text"}},{{"type":"tool_use"}}],"usage":{{"input_tokens":1,"output_tokens":{output},"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"cache_creation":{{"ephemeral_1h_input_tokens":3,"ephemeral_5m_input_tokens":0}}}}}}}}"#
        )
    }

    #[test]
    fn a_live_session_is_read_end_to_end() {
        let root = fixture_root("e2e");
        let pid = std::process::id();

        write(
            &root.join(format!("sessions/{pid}.json")),
            &format!(
                r#"{{"pid":{pid},"sessionId":"sess-1","cwd":"/Users/redd/code","status":"busy","startedAt":1}}"#
            ),
        );

        let transcript = root.join("projects/-Users-redd-code/sess-1.jsonl");
        write(
            &transcript,
            &format!(
                "{}\n{}\n",
                assistant("req-1", "msg-1", "claude-sonnet-5", 10),
                assistant("req-2", "msg-2", "claude-haiku-4-5-20251001", 5),
            ),
        );

        let mut metrics = ClaudeMetrics::new(&root);
        metrics.refresh();

        assert_eq!(metrics.sessions().len(), 1);
        assert_eq!(metrics.sessions()[0].session_id.as_deref(), Some("sess-1"));

        let totals = metrics.totals_for("sess-1");
        assert_eq!(totals.len(), 2, "two model families");

        let grand: u64 = totals.iter().map(|(_, t)| t.output).sum();
        assert_eq!(grand, 15);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refreshing_twice_does_not_double_count() {
        let root = fixture_root("idempotent");
        let pid = std::process::id();

        write(
            &root.join(format!("sessions/{pid}.json")),
            &format!(
                r#"{{"pid":{pid},"sessionId":"sess-1","cwd":"/Users/redd/code","startedAt":1}}"#
            ),
        );
        let transcript = root.join("projects/-Users-redd-code/sess-1.jsonl");
        write(
            &transcript,
            &format!("{}\n", assistant("req-1", "msg-1", "claude-opus-5", 10)),
        );

        let mut metrics = ClaudeMetrics::new(&root);
        metrics.refresh();
        metrics.refresh();
        metrics.refresh();

        let totals = metrics.totals_for("sess-1");
        assert_eq!(totals.len(), 1);
        assert_eq!(
            totals[0].1.output, 10,
            "the checkpointed tailer must not re-read what it already consumed"
        );

        // And an append is picked up.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        writeln!(file, "{}", assistant("req-2", "msg-2", "claude-opus-5", 7)).unwrap();
        drop(file);

        metrics.refresh();
        assert_eq!(metrics.totals_for("sess-1")[0].1.output, 17);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_claude_tree_yields_nothing_rather_than_failing() {
        let mut metrics = ClaudeMetrics::new("/definitely/not/a/real/claude/root");
        metrics.refresh();

        assert!(metrics.sessions().is_empty());
        assert!(metrics.totals_by_model().is_empty());
    }
}
