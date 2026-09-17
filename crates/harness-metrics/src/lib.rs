//! Reads live token metrics off the local transcript trees of three coding-agent harnesses:
//! Claude Code, Codex CLI, and pi.
//!
//! This crate has no dependency on `bottom`/`mon`. It hands back plain data; rendering lives
//! in `src/canvas/widgets/`.
//!
//! # No harness-specific special-casing
//!
//! Every harness is read through the exact same mechanism: walk its transcript tree,
//! tail each file incrementally from a byte-offset checkpoint, and maintain both a
//! time-bucketed history and an unbounded running cumulative total per model family. A
//! caller wanting a tokens/second rate differences the cumulative snapshot between two
//! refreshes -- there is no synchronous hook into "a model call just finished" for any
//! harness, including Claude.
//!
//! That means every harness's numbers lag the actual model call by up to one refresh tick.
//! This trade-off is deliberate: treating every harness identically is worth more than
//! having one harness read closer to real time than the others. See the crate-private
//! `ledger` module for the harness-agnostic engine this runs on, and `claude`, `codex`,
//! `pi` for what each backend parses off disk.
//!
//! Everything parses defensively. Every one of these schemas is undocumented or drifts
//! between releases, and a schema surprise must never take a widget down -- every field is
//! optional, unknown fields are ignored, and an unreadable file is treated as absent rather
//! than as an error.
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//!
//! use harness_metrics::{merge_all, Harness, HarnessLedger};
//!
//! let window = Duration::from_secs(3600);
//! let bucket = Duration::from_secs(60);
//!
//! let mut claude = HarnessLedger::new(Harness::Claude, window, bucket).expect("no home dir");
//! let mut codex = HarnessLedger::new(Harness::Codex, window, bucket).expect("no home dir");
//! claude.refresh();
//! codex.refresh();
//!
//! // Per harness: buckets keyed by model family, plus a cumulative snapshot for rate math.
//! for (family, totals) in claude.cumulative_totals() {
//!     println!("Claude {family}: {} tokens all-time", totals.total());
//! }
//!
//! // Or merged across harnesses, buckets keyed by harness label instead.
//! let all = merge_all(&[&claude, &codex]);
//! for bucket in &all.buckets {
//!     println!("{}: {} tokens", bucket.start_ms, bucket.total());
//! }
//! ```

pub mod claude;
pub mod codex;
mod iso8601;
mod ledger;
pub mod pi;
pub mod tailer;

use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub use ledger::{Bucket, TokenTotals};
pub use tailer::Tailer;

/// A coding-agent harness this crate can read token metrics for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Harness {
    /// Claude Code, reading `~/.claude`.
    Claude,
    /// The Codex CLI, reading `$CODEX_HOME` or `~/.codex`.
    Codex,
    /// pi, reading `~/.pi/agent`.
    Pi,
}

impl Harness {
    /// Every harness, in a fixed order. Callers use this as a draw order and colour index,
    /// same rationale as each backend's `Family::ALL`.
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Pi];

    /// A short display label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Harness::Claude => "Claude",
            Harness::Codex => "Codex",
            Harness::Pi => "Pi",
        }
    }

    /// This harness's default transcript root.
    ///
    /// `None` only when there is no home directory to derive a path from (Codex, with
    /// `$CODEX_HOME` set, is the one case that does not need one).
    #[must_use]
    pub fn default_root(self) -> Option<PathBuf> {
        match self {
            Harness::Claude => home().map(|home| home.join(".claude")),
            Harness::Codex => std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| home().map(|home| home.join(".codex"))),
            Harness::Pi => home().map(|home| home.join(".pi").join("agent")),
        }
    }

    /// This harness's model families, in fixed draw order, as display labels.
    fn family_order(self) -> Vec<&'static str> {
        match self {
            Harness::Claude => claude::ClaudeFamily::ALL
                .iter()
                .map(|f| f.label())
                .collect(),
            Harness::Codex => codex::CodexFamily::ALL.iter().map(|f| f.label()).collect(),
            Harness::Pi => pi::PiFamily::ALL.iter().map(|f| f.label()).collect(),
        }
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The concrete backend a [`HarnessLedger`] drives. One variant per [`Harness`], each
/// wrapping the same generic [`ledger::Ledger`] instantiated on that backend's record type.
enum Inner {
    Claude(ledger::Ledger<claude::Record>),
    Codex(ledger::Ledger<codex::Record>),
    Pi(ledger::Ledger<pi::Record>),
}

/// Reads and accumulates token metrics for one harness.
///
/// Wraps the crate-private `ledger::Ledger`, picking the right backend (discovery function
/// and record parser) for the [`Harness`] it was built with. Every method here simply
/// dispatches to the wrapped ledger -- this struct's only job is hiding which of the three
/// concrete backends is underneath.
pub struct HarnessLedger {
    harness: Harness,
    inner: Inner,
}

impl HarnessLedger {
    /// Read one harness from its default root.
    ///
    /// `window` and `bucket` configure the windowed history the same way the crate-private
    /// `ledger::Ledger::new` does; a typical caller passes 1 hour / 1 minute.
    ///
    /// Returns `None` only when the harness has no default root to read (no `$HOME`, and
    /// for Codex, no `$CODEX_HOME` either).
    #[must_use]
    pub fn new(harness: Harness, window: Duration, bucket: Duration) -> Option<Self> {
        let root = harness.default_root()?;
        Some(Self::with_root(harness, root, window, bucket))
    }

    /// Read one harness from an explicit root. Useful for tests and fixtures.
    #[must_use]
    pub fn with_root(harness: Harness, root: PathBuf, window: Duration, bucket: Duration) -> Self {
        let inner = match harness {
            Harness::Claude => {
                Inner::Claude(ledger::Ledger::new(root, window, bucket, claude::discover))
            }
            Harness::Codex => {
                Inner::Codex(ledger::Ledger::new(root, window, bucket, codex::discover))
            }
            Harness::Pi => Inner::Pi(ledger::Ledger::new(root, window, bucket, pi::discover)),
        };

        Self { harness, inner }
    }

    /// The harness this ledger reads.
    #[must_use]
    pub fn harness(&self) -> Harness {
        self.harness
    }

    /// The root being read.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        match &self.inner {
            Inner::Claude(l) => l.root(),
            Inner::Codex(l) => l.root(),
            Inner::Pi(l) => l.root(),
        }
    }

    /// The bucket width, so a caller can label an axis without restating it.
    #[must_use]
    pub fn bucket(&self) -> Duration {
        match &self.inner {
            Inner::Claude(l) => l.bucket(),
            Inner::Codex(l) => l.bucket(),
            Inner::Pi(l) => l.bucket(),
        }
    }

    /// The window covered, oldest bucket to now.
    #[must_use]
    pub fn window(&self) -> Duration {
        match &self.inner {
            Inner::Claude(l) => l.window(),
            Inner::Codex(l) => l.window(),
            Inner::Pi(l) => l.window(),
        }
    }

    /// Re-read whatever the transcripts have appended and drop anything now out of window.
    /// Cheap enough to call on every collection tick: each file is read from a checkpoint
    /// rather than re-parsed.
    pub fn refresh(&mut self) {
        match &mut self.inner {
            Inner::Claude(l) => l.refresh(),
            Inner::Codex(l) => l.refresh(),
            Inner::Pi(l) => l.refresh(),
        }
    }

    /// Refresh against an explicit instant rather than the system clock.
    ///
    /// Exists for deterministic tests -- a caller building fixtures with fixed timestamps
    /// needs the window's "now" to line up with them, not with wall-clock time.
    pub fn refresh_at(&mut self, now_ms: u64) {
        match &mut self.inner {
            Inner::Claude(l) => l.refresh_at(now_ms),
            Inner::Codex(l) => l.refresh_at(now_ms),
            Inner::Pi(l) => l.refresh_at(now_ms),
        }
    }

    /// Windowed history, oldest bucket first, keyed by model family label.
    #[must_use]
    pub fn buckets(&self) -> Vec<Bucket> {
        match &self.inner {
            Inner::Claude(l) => l.buckets(),
            Inner::Codex(l) => l.buckets(),
            Inner::Pi(l) => l.buckets(),
        }
    }

    /// Model family labels that contributed anything in the window, in this harness's
    /// fixed draw order.
    #[must_use]
    pub fn families_present(&self) -> Vec<&'static str> {
        let order = self.harness.family_order();
        match &self.inner {
            Inner::Claude(l) => l.families_present(&order),
            Inner::Codex(l) => l.families_present(&order),
            Inner::Pi(l) => l.families_present(&order),
        }
    }

    /// The unbounded running cumulative total per model family label, in this harness's
    /// fixed draw order.
    ///
    /// This never evicts. Difference two snapshots of this across refresh ticks to derive a
    /// tokens/second rate -- transcripts are read a tick behind the model call that produced
    /// them, so this is always slightly lagging, never synchronous.
    #[must_use]
    pub fn cumulative_totals(&self) -> Vec<(&'static str, TokenTotals)> {
        let order = self.harness.family_order();
        match &self.inner {
            Inner::Claude(l) => l.cumulative_totals(&order),
            Inner::Codex(l) => l.cumulative_totals(&order),
            Inner::Pi(l) => l.cumulative_totals(&order),
        }
    }
}

/// A view merging several harness ledgers into one, grouped by harness label instead of by
/// model family label: every family within one harness is summed into a single
/// [`TokenTotals`] for that harness's slot.
///
/// Every ledger passed to [`merge_all`] should share the same `window`/`bucket`
/// configuration -- buckets are merged by their start time, and ledgers built with
/// different bucket widths will not line up.
pub struct AllHarnessSnapshot {
    /// Windowed history, one slot per harness label per bucket rather than per family.
    pub buckets: Vec<Bucket>,
    /// The unbounded running cumulative total, one entry per harness label.
    pub cumulative_totals: Vec<(&'static str, TokenTotals)>,
}

/// Merge several harness ledgers into one "all harnesses" view.
///
/// See [`AllHarnessSnapshot`] and the crate-level example.
#[must_use]
pub fn merge_all(ledgers: &[&HarnessLedger]) -> AllHarnessSnapshot {
    let mut merged_buckets: BTreeMap<u64, Vec<(&'static str, TokenTotals)>> = BTreeMap::new();
    let mut cumulative: Vec<(&'static str, TokenTotals)> = Vec::new();

    for ledger in ledgers {
        let label = ledger.harness.label();

        for bucket in ledger.buckets() {
            let harness_total = sum_totals(&bucket.totals);

            let entry = merged_buckets.entry(bucket.start_ms).or_default();
            match entry.iter_mut().find(|(candidate, _)| *candidate == label) {
                Some((_, totals)) => totals.saturating_add_assign(harness_total),
                None => entry.push((label, harness_total)),
            }
        }

        let harness_cumulative = sum_totals(&ledger.cumulative_totals());
        if harness_cumulative == TokenTotals::default() {
            // An empty harness contributes no slot at all, rather than a zeroed one that a
            // caller would have to filter out itself.
            continue;
        }

        match cumulative
            .iter_mut()
            .find(|(candidate, _)| *candidate == label)
        {
            Some((_, totals)) => totals.saturating_add_assign(harness_cumulative),
            None => cumulative.push((label, harness_cumulative)),
        }
    }

    let buckets = merged_buckets
        .into_iter()
        .map(|(start_ms, totals)| Bucket { start_ms, totals })
        .collect();

    AllHarnessSnapshot {
        buckets,
        cumulative_totals: cumulative,
    }
}

fn sum_totals(totals: &[(&'static str, TokenTotals)]) -> TokenTotals {
    totals
        .iter()
        .fold(TokenTotals::default(), |mut acc, (_, t)| {
            acc.saturating_add_assign(*t);
            acc
        })
}

#[cfg(test)]
mod tests {
    // Panicking on a bad fixture is the point in a test -- a fixture that will not
    // parse is a broken test, not a runtime condition to handle.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::{fs, io::Write, path::Path};

    use super::*;

    fn fixture_root(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("harness-metrics-lib-{tag}"));
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

    /// `2026-08-24T20:00:00.000Z` in epoch millis, matching every fixture record's
    /// timestamp -- tests drive [`HarnessLedger::refresh_at`] with this rather than the
    /// system clock so an old fixture never ages out of the default 1-hour window.
    const BASE_MS: u64 = 1_787_601_600_000;

    fn claude_assistant(request: &str, message: &str, model: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-08-24T20:00:00.000Z","requestId":"{request}","sessionId":"sess-1","isSidechain":false,"message":{{"id":"{message}","model":"{model}","usage":{{"input_tokens":1,"output_tokens":{output},"cache_read_input_tokens":2,"cache_creation_input_tokens":3}}}}}}"#
        )
    }

    #[test]
    fn a_claude_transcript_is_read_end_to_end() {
        let root = fixture_root("claude-e2e");
        write(
            &root.join("projects/-Users-redd-code/sess-1.jsonl"),
            &format!(
                "{}\n{}\n",
                claude_assistant("req-1", "msg-1", "claude-sonnet-5", 10),
                claude_assistant("req-2", "msg-2", "claude-haiku-4-5-20251001", 5),
            ),
        );

        let mut ledger = HarnessLedger::with_root(
            Harness::Claude,
            root.clone(),
            Duration::from_secs(3600),
            Duration::from_secs(60),
        );
        ledger.refresh_at(BASE_MS + 1_000);

        assert_eq!(ledger.harness(), Harness::Claude);

        let totals = ledger.cumulative_totals();
        let grand: u64 = totals.iter().map(|(_, t)| t.output).sum();
        assert_eq!(grand, 15);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refreshing_twice_does_not_double_count() {
        let root = fixture_root("claude-idempotent");
        let path = root.join("projects/-Users-redd-code/sess-1.jsonl");
        write(
            &path,
            &format!(
                "{}\n",
                claude_assistant("req-1", "msg-1", "claude-opus-5", 10)
            ),
        );

        let mut ledger = HarnessLedger::with_root(
            Harness::Claude,
            root.clone(),
            Duration::from_secs(3600),
            Duration::from_secs(60),
        );
        ledger.refresh_at(BASE_MS + 1_000);
        ledger.refresh_at(BASE_MS + 2_000);
        ledger.refresh_at(BASE_MS + 3_000);

        let output: u64 = ledger
            .cumulative_totals()
            .iter()
            .map(|(_, t)| t.output)
            .sum();
        assert_eq!(
            output, 10,
            "a checkpointed tailer must not re-read what it consumed"
        );

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            "{}",
            claude_assistant("req-2", "msg-2", "claude-opus-5", 7)
        )
        .unwrap();
        drop(file);

        ledger.refresh_at(BASE_MS + 4_000);
        let output: u64 = ledger
            .cumulative_totals()
            .iter()
            .map(|(_, t)| t.output)
            .sum();
        assert_eq!(output, 17);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_missing_tree_yields_nothing_rather_than_failing() {
        let mut ledger = HarnessLedger::with_root(
            Harness::Claude,
            PathBuf::from("/definitely/not/a/real/claude/root"),
            Duration::from_secs(3600),
            Duration::from_secs(60),
        );
        ledger.refresh_at(BASE_MS);

        assert!(ledger.cumulative_totals().is_empty());
    }

    #[test]
    fn every_harness_defaults_to_the_documented_root_shape() {
        // SAFETY: test-only env mutation, single-threaded within this process's test run
        // for this specific check.
        unsafe {
            std::env::set_var("HOME", "/home/redd");
            std::env::remove_var("CODEX_HOME");
        }

        assert_eq!(
            Harness::Claude.default_root(),
            Some(PathBuf::from("/home/redd/.claude"))
        );
        assert_eq!(
            Harness::Codex.default_root(),
            Some(PathBuf::from("/home/redd/.codex"))
        );
        assert_eq!(
            Harness::Pi.default_root(),
            Some(PathBuf::from("/home/redd/.pi/agent"))
        );

        // SAFETY: see above.
        unsafe {
            std::env::set_var("CODEX_HOME", "/custom/codex");
        }
        assert_eq!(
            Harness::Codex.default_root(),
            Some(PathBuf::from("/custom/codex"))
        );

        // SAFETY: see above.
        unsafe {
            std::env::remove_var("CODEX_HOME");
        }
    }

    #[test]
    fn merge_all_groups_by_harness_label_instead_of_family() {
        let root = fixture_root("merge-all");
        write(
            &root.join("projects/-Users-redd-code/sess-1.jsonl"),
            &format!(
                "{}\n{}\n",
                claude_assistant("req-1", "msg-1", "claude-sonnet-5", 10),
                claude_assistant("req-2", "msg-2", "claude-haiku-4-5-20251001", 5),
            ),
        );

        let mut claude = HarnessLedger::with_root(
            Harness::Claude,
            root.clone(),
            Duration::from_secs(3600),
            Duration::from_secs(60),
        );
        claude.refresh_at(BASE_MS + 1_000);

        let empty_root = fixture_root("merge-all-empty");
        let mut codex = HarnessLedger::with_root(
            Harness::Codex,
            empty_root.clone(),
            Duration::from_secs(3600),
            Duration::from_secs(60),
        );
        codex.refresh_at(BASE_MS + 1_000);

        let all = merge_all(&[&claude, &codex]);

        assert_eq!(
            all.cumulative_totals.len(),
            1,
            "an empty harness contributes no slot at all"
        );
        assert_eq!(all.cumulative_totals[0].0, "Claude");
        assert_eq!(
            all.cumulative_totals[0].1.output, 15,
            "two families' worth of Claude output merged into one harness-level number"
        );

        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&empty_root).unwrap();
    }
}
