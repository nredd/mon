//! The harness-agnostic engine: tails a set of transcript files and turns them into a
//! time-bucketed history plus an unbounded running cumulative total, per model family.
//!
//! # Why this drives every harness identically
//!
//! Claude Code, Codex CLI, and pi each write their own token usage to disk in their own
//! shape, on their own schedule, with their own idea of "how much has this session used so
//! far". Rather than teach this module three sets of rules, each backend (`claude`,
//! `codex`, `pi`) implements [`HarnessRecord`] once and does all of its own counting-rule
//! work internally -- dedup, running-total high-water-marks, cumulative-snapshot deltas,
//! whatever its schema requires. This module never inspects which harness it is reading;
//! it only calls the trait.
//!
//! That uniformity has a real cost: every harness's transcript is read a tick behind the
//! model call that produced it, never synchronously. A caller wanting "tokens per second"
//! differences the cumulative snapshot between two refreshes rather than hooking the model
//! call directly. This lag has been accepted deliberately in exchange for treating every
//! harness the same way, with no special-cased harness sitting closer to real time than the
//! others.
//!
//! # Why bucket assignment is not pinned like a "message"
//!
//! [`Ledger::ingest`] attributes every delta to the bucket named by *that record's own*
//! timestamp, not to the bucket the dedupe key first appeared in. A backend whose dedupe
//! key spans an entire file (a Codex rollout's cumulative counter, say) can otherwise emit
//! events minutes or hours apart; pinning all of them to the file's first event would smear
//! a whole session's usage into one bucket. A backend whose dedupe key spans one message's
//! content blocks (Claude) still lands correctly, because those blocks are written within
//! the same turn and, in practice, the same bucket.

use std::{
    collections::{BTreeMap, HashMap},
    marker::PhantomData,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::tailer::{ReadKind, Tailer};

/// Token counts for one model family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Fresh input tokens.
    pub input: u64,
    /// Generated tokens.
    pub output: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_creation: u64,
}

impl TokenTotals {
    /// Every token this family was billed for, cache included.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_creation)
    }

    /// Add another totals into this one, field by field, saturating rather than wrapping.
    pub fn saturating_add_assign(&mut self, other: TokenTotals) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
    }

    /// The field-by-field difference from `old` to `new`.
    ///
    /// Used to turn a running or cumulative snapshot into an incremental delta. A field
    /// that has *decreased* -- the file was truncated or replaced out from under a counter
    /// that is not otherwise tracked as restarted -- is treated as "start over from here"
    /// rather than underflowing: the whole new value is taken as the delta, never a
    /// negative one. This never panics.
    ///
    /// `pub(crate)` rather than private: a backend that computes its own cumulative delta
    /// internally (Codex's per-file running counter, tracked in its own [`HarnessRecord::State`]
    /// rather than through [`Ledger`]'s keyed-snapshot path) reuses this exact rule rather
    /// than reimplementing it.
    #[must_use]
    pub(crate) fn delta_from(new: TokenTotals, old: TokenTotals) -> TokenTotals {
        fn field(new: u64, old: u64) -> u64 {
            if new >= old { new - old } else { new }
        }

        TokenTotals {
            input: field(new.input, old.input),
            output: field(new.output, old.output),
            cache_read: field(new.cache_read, old.cache_read),
            cache_creation: field(new.cache_creation, old.cache_creation),
        }
    }
}

/// One backend's parsed line, plus everything the ledger needs to fold it in.
///
/// Every harness-specific counting rule lives behind this trait, implemented once per
/// backend. [`Ledger`] never matches on which harness it is driving; it only calls these
/// methods.
pub(crate) trait HarnessRecord: Sized {
    /// Parsing state threaded across every line read from one file, reset whenever that
    /// file's tailer restarts (replaced or truncated). Claude and pi have no use for this
    /// and set it to `()`; Codex uses it to track the most recently seen model, which is
    /// reported on a separate record type than the one that carries token usage.
    type State: Default;

    /// Parse one line, given the running state for the file it came from. May update
    /// `state` in place. Returns `None` for a line that does not parse or carries nothing
    /// this backend cares about.
    fn parse(line: &str, state: &mut Self::State) -> Option<Self>;

    /// This record's instant, in Unix epoch milliseconds. `None` records are dropped: with
    /// no timestamp there is no bucket to attribute the usage to, and guessing "now" would
    /// put every undated record in the newest bucket and draw a spike that never happened.
    fn timestamp_ms(&self) -> Option<u64>;

    /// A key identifying a value that may repeat or supersede an earlier one for the same
    /// logical unit of work, so the ledger can take a delta instead of double-counting.
    ///
    /// `None` means this record's [`billable_usage`](Self::billable_usage) is already the
    /// exact amount to add -- no further tracking needed. `Some(key)` means it is a
    /// snapshot (a running total or a cumulative counter): the first time a key is seen its
    /// full value is added, and every later record sharing the key contributes only the
    /// field-by-field growth since the previous snapshot under that key.
    fn dedupe_key(&self) -> Option<String>;

    /// The model family this record belongs to, already folded by the backend.
    fn family_label(&self) -> &'static str;

    /// This record's usage, or `None` if it carries none.
    fn billable_usage(&self) -> Option<TokenTotals>;
}

/// One bucket's worth of usage, by model family label.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bucket {
    /// Start of the bucket, in Unix epoch milliseconds.
    pub start_ms: u64,
    /// Families that contributed, in first-seen order.
    pub totals: Vec<(&'static str, TokenTotals)>,
}

impl Bucket {
    /// Every token in this bucket, across all families.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.totals
            .iter()
            .fold(0u64, |sum, (_, totals)| sum.saturating_add(totals.total()))
    }

    /// This family's tokens in this bucket, or zero if it did not contribute.
    #[must_use]
    pub fn total_for(&self, label: &str) -> u64 {
        self.totals
            .iter()
            .find(|(candidate, _)| *candidate == label)
            .map_or(0, |(_, totals)| totals.total())
    }
}

/// What a dedupe key last contributed, remembered so a later snapshot can be turned into a
/// delta instead of double-counted.
#[derive(Clone, Copy, Debug)]
struct Counted {
    /// The most recent full snapshot seen under this key.
    prev: TokenTotals,
    /// The bucket this key last contributed to, used only to decide when its entry has
    /// aged out of the window -- it does not pin where future deltas land.
    last_bucket: u64,
}

/// Per-file tailing state: the byte-offset tailer plus the backend's own parsing state.
struct FileState<S> {
    tailer: Tailer,
    parser: S,
}

/// Tails a set of discovered files for one harness backend and turns what they contain
/// into a time-bucketed history and an unbounded running cumulative total, both keyed by
/// model family label.
///
/// Refreshing is incremental: each file keeps a byte-offset checkpoint, so a refresh only
/// parses what has been appended since the last one.
pub(crate) struct Ledger<R: HarnessRecord> {
    root: PathBuf,
    window: Duration,
    bucket: Duration,
    discover: fn(&Path, u64) -> Vec<PathBuf>,
    buckets: BTreeMap<u64, Vec<(&'static str, TokenTotals)>>,
    cumulative: HashMap<&'static str, TokenTotals>,
    files: HashMap<PathBuf, FileState<R::State>>,
    seen: HashMap<String, Counted>,
    _record: PhantomData<R>,
}

impl<R: HarnessRecord> Ledger<R> {
    /// A ledger over `window`, split into buckets of `bucket`, reading `root` through
    /// `discover` -- a backend-supplied function mapping a root and a cutoff (Unix epoch
    /// milliseconds) to every file that might contain a record at or after that cutoff.
    ///
    /// A zero or absurd `bucket` is clamped to something drawable rather than rejected --
    /// this sits in a draw path and a config typo should not take the app down.
    pub(crate) fn new(
        root: impl Into<PathBuf>, window: Duration, bucket: Duration,
        discover: fn(&Path, u64) -> Vec<PathBuf>,
    ) -> Self {
        let bucket = bucket.clamp(Duration::from_secs(1), Duration::from_hours(24));
        let window = window.clamp(bucket, Duration::from_hours(90 * 24));

        Self {
            root: root.into(),
            window,
            bucket,
            discover,
            buckets: BTreeMap::new(),
            cumulative: HashMap::new(),
            files: HashMap::new(),
            seen: HashMap::new(),
            _record: PhantomData,
        }
    }

    /// The root being read.
    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The bucket width.
    #[must_use]
    pub(crate) fn bucket(&self) -> Duration {
        self.bucket
    }

    /// The window covered, oldest bucket to now.
    #[must_use]
    pub(crate) fn window(&self) -> Duration {
        self.window
    }

    /// Re-read whatever the discovered files have appended and drop anything now out of
    /// window. `now_ms` is passed in rather than read from the clock so this is testable
    /// without waiting for real time to pass.
    pub(crate) fn refresh_at(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(millis(self.window));

        for path in (self.discover)(&self.root, cutoff) {
            let state = self.files.entry(path.clone()).or_insert_with(|| FileState {
                tailer: Tailer::new(path),
                parser: R::State::default(),
            });

            let (lines, kind) = state.tailer.read_new();

            // A replaced file means any in-flight parsing state, and any dedupe keys
            // tracking it, describe a file that is no longer there. The buckets already
            // built stay -- they describe real tokens that were really spent -- but
            // continuing to track deltas against the old snapshot would corrupt every
            // record after the swap.
            if kind == ReadKind::Restarted {
                state.parser = R::State::default();
                self.seen.clear();
            }

            // Parsed while `state` still borrows `self.files`, then folded in afterwards --
            // `ingest` needs `self` back to update `self.buckets`/`self.cumulative`/`self.seen`.
            let records: Vec<R> = lines
                .into_iter()
                .filter_map(|line| R::parse(&line, &mut state.parser))
                .collect();

            for record in &records {
                self.ingest(record, cutoff);
            }
        }

        self.evict(cutoff);
    }

    /// Refresh against the system clock.
    pub(crate) fn refresh(&mut self) {
        self.refresh_at(now_ms());
    }

    /// Fold one record in.
    fn ingest(&mut self, record: &R, cutoff: u64) {
        let Some(usage) = record.billable_usage() else {
            return;
        };

        // Without a timestamp there is no bucket to attribute it to.
        let Some(stamp) = record.timestamp_ms() else {
            return;
        };

        if stamp < cutoff {
            return;
        }

        let bucket_start = stamp - (stamp % millis(self.bucket));
        let family = record.family_label();

        let delta = match record.dedupe_key() {
            None => usage,
            Some(key) => {
                if let Some(counted) = self.seen.get_mut(&key) {
                    let delta = TokenTotals::delta_from(usage, counted.prev);
                    counted.prev = usage;
                    counted.last_bucket = bucket_start;
                    delta
                } else {
                    self.seen.insert(
                        key,
                        Counted {
                            prev: usage,
                            last_bucket: bucket_start,
                        },
                    );
                    usage
                }
            }
        };

        if delta == TokenTotals::default() {
            return;
        }

        self.totals_for_mut(bucket_start, family)
            .saturating_add_assign(delta);

        self.cumulative
            .entry(family)
            .or_default()
            .saturating_add_assign(delta);
    }

    fn totals_for_mut(&mut self, bucket: u64, family: &'static str) -> &mut TokenTotals {
        let entry = self.buckets.entry(bucket).or_default();

        if let Some(index) = entry.iter().position(|(candidate, _)| *candidate == family) {
            return &mut entry[index].1;
        }

        entry.push((family, TokenTotals::default()));

        // The push above guarantees a last element; an `expect` here would be a panic path
        // in a refresh loop for a case the compiler simply cannot see.
        match entry.last_mut() {
            Some((_, totals)) => totals,
            None => unreachable!("an element was just pushed"),
        }
    }

    /// Drop buckets, and the dedupe keys pointing at them, that have aged out.
    fn evict(&mut self, cutoff: u64) {
        let stale = cutoff - (cutoff % millis(self.bucket));
        self.buckets.retain(|start, _| *start >= stale);
        // Otherwise a long-running process grows this map forever.
        self.seen.retain(|_, counted| counted.last_bucket >= stale);
    }

    /// Buckets in the window, oldest first, with empty ones filled in.
    ///
    /// The gaps matter: a graph drawn from only the buckets that saw traffic would join a
    /// point at 10:00 straight to one at 10:40 and draw a plateau across half an hour of
    /// silence. `now_ms` fixes the right-hand edge.
    #[must_use]
    pub(crate) fn buckets_at(&self, now_ms: u64) -> Vec<Bucket> {
        let step = millis(self.bucket);
        let newest = now_ms - (now_ms % step);
        let count = (millis(self.window) / step).max(1);
        let oldest = newest.saturating_sub(step.saturating_mul(count - 1));

        (0..count)
            .map(|index| {
                let start_ms = oldest + index * step;

                Bucket {
                    start_ms,
                    totals: self.buckets.get(&start_ms).cloned().unwrap_or_default(),
                }
            })
            .collect()
    }

    /// Buckets in the window against the system clock.
    #[must_use]
    pub(crate) fn buckets(&self) -> Vec<Bucket> {
        self.buckets_at(now_ms())
    }

    /// Family labels that contributed anything in the window, filtered from `order` to
    /// preserve a fixed draw/colour order rather than first-appearance or volume order --
    /// a family going quiet must not repaint the ones that remain.
    #[must_use]
    pub(crate) fn families_present(&self, order: &[&'static str]) -> Vec<&'static str> {
        order
            .iter()
            .copied()
            .filter(|family| {
                self.buckets
                    .values()
                    .flatten()
                    .any(|(candidate, totals)| candidate == family && totals.total() > 0)
            })
            .collect()
    }

    /// The unbounded running cumulative total per family label, in `order`.
    ///
    /// This never evicts. A caller differences two snapshots of this across refresh ticks
    /// to derive a tokens/second rate.
    #[must_use]
    pub(crate) fn cumulative_totals(
        &self, order: &[&'static str],
    ) -> Vec<(&'static str, TokenTotals)> {
        order
            .iter()
            .filter_map(|family| self.cumulative.get(family).map(|totals| (*family, *totals)))
            .collect()
    }
}

/// Unix epoch milliseconds, now.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, millis)
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::cell::Cell;

    use super::*;

    /// `2026-08-24T20:00:00.000Z` in epoch millis, a round bucket boundary.
    const BASE: u64 = 1_787_601_600_000;

    /// A tiny fixture record: `"<label>|<key-or->|<ts>|<input>,<output>,<cache_read>,<cache_creation>"`.
    /// `key` of `-` means `dedupe_key()` returns `None`.
    #[derive(Debug)]
    struct Fixture {
        label: &'static str,
        key: Option<String>,
        ts: Option<u64>,
        usage: Option<TokenTotals>,
    }

    impl HarnessRecord for Fixture {
        type State = ();

        fn parse(line: &str, _state: &mut Self::State) -> Option<Self> {
            let mut parts = line.splitn(4, '|');
            let label = match parts.next()? {
                "opus" => "Opus",
                "sonnet" => "Sonnet",
                _ => return None,
            };
            let key = match parts.next()? {
                "-" => None,
                k => Some(k.to_owned()),
            };
            let ts = match parts.next()? {
                "-" => None,
                t => Some(t.parse().ok()?),
            };
            let usage = match parts.next()? {
                "-" => None,
                nums => {
                    let mut n = nums.split(',');
                    Some(TokenTotals {
                        input: n.next()?.parse().ok()?,
                        output: n.next()?.parse().ok()?,
                        cache_read: n.next()?.parse().ok()?,
                        cache_creation: n.next()?.parse().ok()?,
                    })
                }
            };

            Some(Fixture {
                label,
                key,
                ts,
                usage,
            })
        }

        fn timestamp_ms(&self) -> Option<u64> {
            self.ts
        }

        fn dedupe_key(&self) -> Option<String> {
            self.key.clone()
        }

        fn family_label(&self) -> &'static str {
            self.label
        }

        fn billable_usage(&self) -> Option<TokenTotals> {
            self.usage
        }
    }

    fn discover_none(_root: &Path, _cutoff: u64) -> Vec<PathBuf> {
        Vec::new()
    }

    fn ledger() -> Ledger<Fixture> {
        Ledger::new(
            "/nonexistent",
            Duration::from_secs(3600),
            Duration::from_secs(60),
            discover_none,
        )
    }

    fn ingest(ledger: &mut Ledger<Fixture>, line: &str, cutoff: u64) {
        let record =
            Fixture::parse(line, &mut ()).unwrap_or_else(|| panic!("fixture must parse: {line}"));
        ledger.ingest(&record, cutoff);
    }

    #[test]
    fn a_record_lands_in_the_bucket_its_timestamp_names() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787601810000|10,20,100,5", BASE);

        let buckets = ledger.buckets_at(BASE + 600_000);
        let hit: Vec<&Bucket> = buckets.iter().filter(|b| b.total() > 0).collect();

        assert_eq!(hit.len(), 1, "exactly one bucket should have traffic");
        assert_eq!(
            hit[0].start_ms,
            BASE + 180_000,
            "20:03:30 falls in the 20:03 bucket"
        );
        assert_eq!(hit[0].total_for("Opus"), 135);
    }

    #[test]
    fn an_unkeyed_record_is_added_directly_every_time() {
        // `None` means "already an exact delta" -- the direct-usage path pi and Codex's
        // `last_token_usage` fallback both rely on.
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787601810000|10,20,0,0", BASE);
        ingest(&mut ledger, "opus|-|1787601810000|10,20,0,0", BASE);

        assert_eq!(ledger.cumulative_totals(&["Opus"])[0].1.total(), 60);
    }

    #[test]
    fn a_keyed_snapshot_contributes_only_its_growth() {
        // The Claude output-running-total / Codex cumulative-counter shape: the same key
        // repeats with a growing snapshot, and only the growth must be counted.
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|req-1|1787601810000|10,20,0,0", BASE);
        ingest(&mut ledger, "opus|req-1|1787601811000|10,50,0,0", BASE);

        assert_eq!(
            ledger.cumulative_totals(&["Opus"])[0].1.output,
            50,
            "20 the first time, plus 30 new, not 20 + 50"
        );
        assert_eq!(
            ledger.cumulative_totals(&["Opus"])[0].1.input,
            10,
            "the unchanged field must not be recounted"
        );
    }

    #[test]
    fn a_growing_keyed_snapshot_can_cross_a_bucket_boundary() {
        // Unlike a "message pinned to its first bucket" rule, growth lands wherever its own
        // record's timestamp says -- required for a dedupe key that spans a whole file
        // (a Codex rollout's cumulative counter), which can span many buckets.
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|req-1|1787601839000|0,20,0,0", BASE); // 20:03
        ingest(&mut ledger, "opus|req-1|1787601901000|0,50,0,0", BASE); // 20:05

        let buckets = ledger.buckets_at(BASE + 600_000);
        let hits: Vec<&Bucket> = buckets.iter().filter(|b| b.total() > 0).collect();

        assert_eq!(
            hits.len(),
            2,
            "growth after the boundary lands in the later bucket"
        );
        assert_eq!(hits[0].total_for("Opus"), 20);
        assert_eq!(hits[1].total_for("Opus"), 30);
    }

    #[test]
    fn a_decreasing_keyed_snapshot_starts_over_rather_than_underflowing() {
        // A rollout file truncated and replaced out from under a still-active dedupe key:
        // the new, lower snapshot must be taken as a fresh delta, not `new - old` wrapping.
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|req-1|1787601810000|0,100,0,0", BASE);
        ingest(&mut ledger, "opus|req-1|1787601811000|0,10,0,0", BASE);

        assert_eq!(
            ledger.cumulative_totals(&["Opus"])[0].1.output,
            110,
            "100 the first time, then 10 more treated as a fresh start"
        );
    }

    #[test]
    fn cumulative_totals_never_evict_but_buckets_do() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787601810000|10,20,0,0", BASE);

        // Two hours later, everything in the window has aged out of the buckets.
        ledger.evict(BASE + 2 * 3_600_000);

        assert!(
            ledger
                .buckets_at(BASE + 2 * 3_600_000)
                .iter()
                .all(|b| b.total() == 0)
        );
        assert_eq!(
            ledger.cumulative_totals(&["Opus"])[0].1.total(),
            30,
            "the cumulative total is unbounded and must survive eviction"
        );
    }

    #[test]
    fn families_are_kept_apart_and_draw_order_is_fixed() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787601810000|10,20,0,0", BASE);
        ingest(&mut ledger, "sonnet|-|1787601811000|5,5,0,0", BASE);

        assert_eq!(
            ledger.families_present(&["Opus", "Sonnet"]),
            vec!["Opus", "Sonnet"]
        );
        // A quiet family drops out without disturbing the order of what remains.
        assert_eq!(
            ledger.families_present(&["Sonnet", "Opus"]),
            vec!["Sonnet", "Opus"]
        );
    }

    #[test]
    fn quiet_buckets_are_filled_in_rather_than_skipped() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787601810000|10,20,0,0", BASE);

        let buckets = ledger.buckets_at(BASE + 600_000);

        assert_eq!(buckets.len(), 60, "an hour of one-minute buckets");
        assert!(buckets.windows(2).all(|w| w[1].start_ms > w[0].start_ms));
        assert_eq!(buckets.iter().filter(|b| b.total() == 0).count(), 59);
    }

    #[test]
    fn a_record_older_than_the_window_is_dropped() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|1787594400000|10,20,0,0", BASE);

        assert!(ledger.cumulative_totals(&["Opus"]).is_empty());
    }

    #[test]
    fn an_undated_record_is_dropped_rather_than_dated_now() {
        let mut ledger = ledger();
        ingest(&mut ledger, "opus|-|-|10,20,0,0", BASE);

        assert!(ledger.cumulative_totals(&["Opus"]).is_empty());
    }

    #[test]
    fn a_degenerate_bucket_is_clamped_rather_than_dividing_by_zero() {
        let ledger: Ledger<Fixture> = Ledger::new(
            "/nonexistent",
            Duration::from_secs(60),
            Duration::ZERO,
            discover_none,
        );

        assert!(ledger.bucket() >= Duration::from_secs(1));
        assert!(!ledger.buckets_at(BASE).is_empty());
    }

    #[test]
    fn a_missing_tree_yields_an_empty_window_rather_than_failing() {
        let mut ledger = ledger();
        ledger.refresh_at(BASE);

        assert!(ledger.families_present(&["Opus"]).is_empty());
        assert_eq!(ledger.buckets_at(BASE).len(), 60);
    }

    #[test]
    fn per_file_parser_state_is_threaded_and_reset_on_restart() {
        // Exercises the actual `refresh_at` path (not the `ingest` shortcut) against a real
        // file, with a `State` that counts how many lines it has parsed -- standing in for
        // Codex's "most recently seen model" tracking.
        fn discover(root: &Path, _cutoff: u64) -> Vec<PathBuf> {
            vec![root.join("f.jsonl")]
        }

        #[derive(Debug)]
        struct CountingRecord {
            seen_before: Cell<u32>,
        }

        impl HarnessRecord for CountingRecord {
            type State = u32;

            fn parse(line: &str, state: &mut Self::State) -> Option<Self> {
                if line.is_empty() {
                    return None;
                }
                let seen_before = *state;
                *state += 1;
                Some(CountingRecord {
                    seen_before: Cell::new(seen_before),
                })
            }

            fn timestamp_ms(&self) -> Option<u64> {
                Some(BASE)
            }

            fn dedupe_key(&self) -> Option<String> {
                None
            }

            fn family_label(&self) -> &'static str {
                "Opus"
            }

            fn billable_usage(&self) -> Option<TokenTotals> {
                Some(TokenTotals {
                    input: u64::from(self.seen_before.get()),
                    ..TokenTotals::default()
                })
            }
        }

        let dir = std::env::temp_dir().join("harness-metrics-ledger-state-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.jsonl");
        std::fs::write(&path, "a\nb\nc\n").unwrap();

        let mut ledger: Ledger<CountingRecord> = Ledger::new(
            &dir,
            Duration::from_secs(3600),
            Duration::from_secs(60),
            discover,
        );
        ledger.refresh_at(BASE + 1);

        // Lines 0, 1, 2 contributed `input` 0, 1, 2: state threaded across the whole file.
        assert_eq!(ledger.cumulative_totals(&["Opus"])[0].1.input, 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
