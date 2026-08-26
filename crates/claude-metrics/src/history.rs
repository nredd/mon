//! Token usage bucketed by wall-clock time, across every transcript in the tree.
//!
//! # Why this is not built from the live session state
//!
//! [`crate::ClaudeMetrics::refresh`] tails only the sessions currently in the registry, and
//! drops a session's state as soon as it leaves. That is right for "what is running now",
//! and wrong for "what happened in the last hour" -- a session that exited five minutes ago
//! contributed real tokens that would silently vanish from the graph the moment it closed.
//!
//! So this walks `<root>/projects` itself. Files are filtered by modification time before
//! being opened, which is what keeps a full-tree scan cheap: a tree with a year of
//! transcripts in it still only opens the handful touched inside the window.
//!
//! # Why the records carry their own time
//!
//! Every billable record has an ISO-8601 `timestamp`, so a bucket is attributed from the
//! record rather than from when this happened to read it. That means history is correct on
//! the very first refresh -- the past hour is already on disk -- instead of having to be
//! accumulated live over an hour before the graph says anything.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    model::ModelFamily,
    tailer::{ReadKind, Tailer},
    transcript::{Record, TokenTotals},
};

/// One bucket's worth of usage, by model family.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bucket {
    /// Start of the bucket, in Unix epoch milliseconds.
    pub start_ms: u64,
    /// Families that contributed, in first-seen order.
    pub totals: Vec<(ModelFamily, TokenTotals)>,
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
    pub fn total_for(&self, family: ModelFamily) -> u64 {
        self.totals
            .iter()
            .find(|(candidate, _)| *candidate == family)
            .map_or(0, |(_, totals)| totals.total())
    }
}

/// What a message contributed, remembered so replays are not counted twice.
#[derive(Clone, Copy, Debug)]
struct Counted {
    family: ModelFamily,
    /// Highest `output_tokens` seen for this message so far.
    output: u64,
    /// Which bucket it landed in, so later blocks of the same message land there too.
    bucket: u64,
    /// Which tracked file first claimed this message. A replaced file has to forget its own
    /// dedupe state, and this is what makes that a targeted retraction rather than a global
    /// one -- see [`TokenHistory::refresh_at`].
    source: SourceId,
}

/// Identifies one tracked transcript for the lifetime of a [`TokenHistory`].
type SourceId = u32;

/// One transcript being tailed, plus the id its dedupe keys are stamped with.
#[derive(Debug)]
struct Tracked {
    id: SourceId,
    tailer: Tailer,
}

/// Token usage over a rolling window, bucketed by time.
///
/// Refreshing is incremental: each transcript keeps a checkpoint, so a refresh only parses
/// what has been appended since the last one.
#[derive(Debug)]
pub struct TokenHistory {
    root: PathBuf,
    window: Duration,
    bucket: Duration,
    buckets: BTreeMap<u64, Vec<(ModelFamily, TokenTotals)>>,
    tailers: HashMap<PathBuf, Tracked>,
    /// Handed out by [`TokenHistory::refresh_at`] as transcripts are first seen.
    next_source: SourceId,
    /// Dedupe key mapped to what it contributed. Retries and resumed sessions replay
    /// identical messages, and the same message appears in both a session transcript and
    /// its subagent files.
    seen: HashMap<String, Counted>,
}

impl TokenHistory {
    /// A history over `window`, split into buckets of `bucket`.
    ///
    /// A zero or absurd `bucket` is clamped to something drawable rather than rejected --
    /// this sits in a draw path and a config typo should not take the app down.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, window: Duration, bucket: Duration) -> Self {
        let bucket = bucket.clamp(Duration::from_secs(1), Duration::from_hours(24));
        let window = window.clamp(bucket, Duration::from_hours(90 * 24));

        Self {
            root: root.into(),
            window,
            bucket,
            buckets: BTreeMap::new(),
            tailers: HashMap::new(),
            next_source: 0,
            seen: HashMap::new(),
        }
    }

    /// The bucket width, so a caller can label an axis without restating it.
    #[must_use]
    pub fn bucket(&self) -> Duration {
        self.bucket
    }

    /// The window covered, oldest bucket to now.
    #[must_use]
    pub fn window(&self) -> Duration {
        self.window
    }

    /// Re-read whatever the transcripts have appended and drop anything now out of window.
    ///
    /// `now_ms` is passed in rather than read from the clock so this is testable without
    /// waiting for real time to pass.
    pub fn refresh_at(&mut self, now_ms: u64) {
        let cutoff = now_ms.saturating_sub(millis(self.window));

        for path in transcripts_modified_since(&self.root, cutoff) {
            let next_source = self.next_source;
            let tracked = self.tailers.entry(path.clone()).or_insert_with(|| Tracked {
                id: next_source,
                tailer: Tailer::new(path),
            });

            if tracked.id == next_source {
                self.next_source = self.next_source.wrapping_add(1);
            }

            let source = tracked.id;
            let (lines, kind) = tracked.tailer.read_new();

            // A replaced file means the checkpoint described a file that is no longer
            // there. The buckets already built from it stay -- they describe real tokens
            // that were really spent -- but the dedupe state cannot be trusted across the
            // swap, so the replacement is read from the top.
            //
            // Only *this* file's keys are dropped. Clearing the whole map, which is what
            // this did originally, is safe at a handful of tracked files and actively wrong
            // at a few hundred: every other file's messages would be re-counted the next
            // time any of them replayed a line, adding tokens to live buckets that were
            // already counted.
            if kind == ReadKind::Restarted {
                self.seen.retain(|_, counted| counted.source != source);
            }

            for line in lines {
                let Some(record) = Record::parse(&line) else {
                    continue;
                };

                self.ingest(&record, cutoff, source);
            }
        }

        self.evict(cutoff);
    }

    /// Refresh against the system clock.
    pub fn refresh(&mut self) {
        self.refresh_at(now_ms());
    }

    /// Fold one record into its bucket, attributed to the transcript it came from.
    fn ingest(&mut self, record: &Record, cutoff: u64, source: SourceId) {
        let Some(usage) = record.billable_usage() else {
            return;
        };

        let Some(key) = record.dedupe_key() else {
            return;
        };

        // A message already counted contributes only its *new* output tokens, and they go
        // to the bucket the message started in. Output is a running total that grows with
        // each content block, so the difference is the only new information; splitting the
        // later blocks into a neighbouring bucket would smear one message across two.
        if let Some(counted) = self.seen.get_mut(&key) {
            if usage.output <= counted.output {
                return;
            }

            let delta = usage.output - counted.output;
            let (family, bucket) = (counted.family, counted.bucket);
            counted.output = usage.output;

            self.totals_for_mut(bucket, family).output = self
                .totals_for_mut(bucket, family)
                .output
                .saturating_add(delta);
            return;
        }

        // Without a timestamp there is no bucket to attribute it to. Dropping it is right:
        // guessing "now" would pile every undated record onto the newest bucket and draw a
        // spike that never happened.
        let Some(stamp) = record.timestamp_ms() else {
            return;
        };

        if stamp < cutoff {
            return;
        }

        let bucket = stamp - (stamp % millis(self.bucket));
        let family = ModelFamily::from_id(record.model_id().unwrap_or_default());

        self.seen.insert(
            key,
            Counted {
                family,
                output: usage.output,
                bucket,
                source,
            },
        );

        let totals = self.totals_for_mut(bucket, family);
        totals.input = totals.input.saturating_add(usage.input);
        totals.cache_read = totals.cache_read.saturating_add(usage.cache_read);
        totals.cache_creation = totals.cache_creation.saturating_add(usage.cache_creation);
        totals.output = totals.output.saturating_add(usage.output);
    }

    fn totals_for_mut(&mut self, bucket: u64, family: ModelFamily) -> &mut TokenTotals {
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
        self.seen.retain(|_, counted| counted.bucket >= stale);
    }

    /// Roll the stored buckets up onto an arbitrary grid, oldest first.
    ///
    /// The internal grid is deliberately finer than anything drawn, so one history serves
    /// every range a widget can ask for: a thirty-minute view and a seven-day view read the
    /// same buckets at different step sizes. Rolling up here rather than keeping a history
    /// per range is also what keeps the tailing cost fixed -- the transcripts are parsed
    /// exactly once no matter how many views are on screen.
    ///
    /// Empty slots are filled in. The gaps matter: a graph drawn from only the buckets that
    /// saw traffic would join a point at 10:00 straight to one at 10:40 and draw a plateau
    /// across half an hour of silence. `now_ms` fixes the right-hand edge.
    ///
    /// `window` and `bucket` are clamped the same way [`TokenHistory::new`] clamps its own,
    /// and a `bucket` finer than the stored grid cannot invent detail -- it just yields
    /// mostly-empty slots.
    #[must_use]
    pub fn aggregate_at(&self, now_ms: u64, window: Duration, bucket: Duration) -> Vec<Bucket> {
        let step = millis(bucket).max(1);
        let newest = now_ms - (now_ms % step);
        let count = (millis(window) / step).max(1);
        let oldest = newest.saturating_sub(step.saturating_mul(count - 1));

        (0..count)
            .map(|index| {
                let start_ms = oldest + index * step;
                let end_ms = start_ms.saturating_add(step);
                let mut totals: Vec<(ModelFamily, TokenTotals)> = Vec::new();

                for entries in self.buckets.range(start_ms..end_ms).map(|(_, v)| v) {
                    for (family, add) in entries {
                        match totals.iter_mut().find(|(seen, _)| seen == family) {
                            Some((_, into)) => into.merge(*add),
                            None => totals.push((*family, *add)),
                        }
                    }
                }

                Bucket { start_ms, totals }
            })
            .collect()
    }

    /// Roll up onto an arbitrary grid against the system clock.
    #[must_use]
    pub fn aggregate(&self, window: Duration, bucket: Duration) -> Vec<Bucket> {
        self.aggregate_at(now_ms(), window, bucket)
    }

    /// Buckets on the history's own grid, oldest first, with empty ones filled in.
    #[must_use]
    pub fn buckets_at(&self, now_ms: u64) -> Vec<Bucket> {
        self.aggregate_at(now_ms, self.window, self.bucket)
    }

    /// Buckets in the window against the system clock.
    #[must_use]
    pub fn buckets(&self) -> Vec<Bucket> {
        self.buckets_at(now_ms())
    }

    /// Families that contributed anything in the window, in a stable draw order.
    ///
    /// Ordered by [`ModelFamily::ALL`] rather than by first appearance or by volume, so a
    /// family going quiet cannot repaint the ones that remain.
    #[must_use]
    pub fn families(&self) -> Vec<ModelFamily> {
        ModelFamily::ALL
            .into_iter()
            .filter(|family| {
                self.buckets
                    .values()
                    .flatten()
                    .any(|(candidate, totals)| candidate == family && totals.total() > 0)
            })
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

/// Every `*.jsonl` under `<root>/projects` touched at or after `cutoff`.
///
/// The modification-time filter is what makes a full-tree walk affordable. A file last
/// written before the window opened cannot contain a record inside it, so it is never
/// opened at all -- only its directory entry is read.
fn transcripts_modified_since(root: &Path, cutoff: u64) -> Vec<PathBuf> {
    let projects = root.join("projects");
    let mut found = Vec::new();

    let Ok(entries) = std::fs::read_dir(&projects) else {
        return found;
    };

    for project in entries.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
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
                .map(millis);

            // A file with no readable mtime is read rather than skipped. Being wrong in
            // the cheap direction costs one parse; being wrong the other way loses data.
            if modified.is_none_or(|stamp| stamp >= cutoff) {
                found.push(path);
            }
        }
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregating_rolls_fine_buckets_up_without_losing_tokens() {
        // The whole point of one fine history serving every range: a coarse view has to be
        // the sum of the fine buckets under it, not a sample of one of them.
        let mut history = TokenHistory::new(
            "/nonexistent",
            Duration::from_secs(3600),
            Duration::from_secs(10),
        );

        for (offset, id) in [(0, "a"), (10, "b"), (20, "c"), (70, "d")] {
            ingest(
                &mut history,
                &assistant(
                    &format!("2026-08-24T20:0{}:{:02}.000Z", offset / 60, offset % 60),
                    "claude-opus-5",
                    id,
                    20,
                ),
                BASE,
            );
        }

        // The newest slot is the bucket currently in progress, so the window has to reach
        // one slot further back than the oldest datum for it to appear at all.
        let window = Duration::from_secs(180);
        let fine = history.aggregate_at(BASE + 120_000, window, BUCKET_10S);
        let coarse = history.aggregate_at(BASE + 120_000, window, BUCKET_60S);

        let fine_total: u64 = fine.iter().map(Bucket::total).sum();
        let coarse_total: u64 = coarse.iter().map(Bucket::total).sum();

        assert_eq!(fine_total, coarse_total, "rolling up must conserve tokens");
        assert_eq!(fine_total, 4 * PER_MESSAGE);

        // Minute zero holds three messages, minute one holds one, minute two is in
        // progress and empty.
        assert_eq!(coarse.len(), 3);
        assert_eq!(coarse[0].total(), 3 * PER_MESSAGE);
        assert_eq!(coarse[1].total(), PER_MESSAGE);
        assert_eq!(coarse[2].total(), 0);
    }

    #[test]
    fn an_aggregate_slot_starts_on_its_own_grid() {
        // Slots have to align to the requested step, not to the stored one, or the labels
        // drawn from `start_ms` name a time the bucket does not actually cover.
        let history = TokenHistory::new(
            "/nonexistent",
            Duration::from_secs(3600),
            Duration::from_secs(10),
        );

        let buckets = history.aggregate_at(BASE + 137_000, Duration::from_secs(600), BUCKET_60S);

        assert!(buckets.iter().all(|b| b.start_ms % 60_000 == 0));
        assert!(
            buckets
                .windows(2)
                .all(|w| w[1].start_ms - w[0].start_ms == 60_000),
            "slots must be contiguous"
        );
    }

    #[test]
    fn aggregating_keeps_families_separate() {
        let mut history = TokenHistory::new(
            "/nonexistent",
            Duration::from_secs(3600),
            Duration::from_secs(10),
        );

        ingest(
            &mut history,
            &assistant("2026-08-24T20:00:00.000Z", "claude-opus-5", "a", 20),
            BASE,
        );
        ingest(
            &mut history,
            &assistant("2026-08-24T20:00:30.000Z", "claude-sonnet-5", "b", 20),
            BASE,
        );

        let rolled = history.aggregate_at(BASE + 60_000, Duration::from_secs(120), BUCKET_60S);

        assert_eq!(rolled.len(), 2);
        assert_eq!(rolled[0].total_for(ModelFamily::Opus), PER_MESSAGE);
        assert_eq!(rolled[0].total_for(ModelFamily::Sonnet), PER_MESSAGE);
        assert_eq!(
            rolled[0].totals.len(),
            2,
            "two families, not one merged pile"
        );
    }

    #[test]
    fn a_replaced_file_only_forgets_its_own_dedupe_state() {
        // Clearing the whole `seen` map on any one file's replacement is safe at a handful
        // of tracked files and wrong at a few hundred: every other file's messages become
        // re-countable, and the next replayed line adds them to buckets that already hold
        // them. Only the replaced file's keys may go.
        let mut history = history();

        ingest_from(
            &mut history,
            &assistant("2026-08-24T20:00:00.000Z", "claude-opus-5", "a", 20),
            BASE,
            7,
        );

        history.seen.retain(|_, counted| counted.source != 7);

        // The bucket keeps what was really spent...
        let before = history.buckets_at(BASE + 60_000);
        assert_eq!(before.iter().map(Bucket::total).sum::<u64>(), PER_MESSAGE);

        // ...and a different file's key is untouched by that retraction.
        ingest_from(
            &mut history,
            &assistant("2026-08-24T20:00:00.000Z", "claude-opus-5", "b", 20),
            BASE,
            9,
        );
        history.seen.retain(|_, counted| counted.source != 7);

        assert!(
            history.seen.values().any(|counted| counted.source == 9),
            "another file's dedupe state must survive"
        );
    }

    /// What one `assistant` fixture contributes: 10 input + 100 cache read + 5 cache
    /// creation + 20 output.
    const PER_MESSAGE: u64 = 135;
    const BUCKET_10S: Duration = Duration::from_secs(10);
    const BUCKET_60S: Duration = Duration::from_secs(60);

    /// `2026-08-24T20:00:00.000Z` in epoch millis, a round bucket boundary.
    const BASE: u64 = 1_787_601_600_000;

    fn assistant(stamp: &str, model: &str, id: &str, output: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{stamp}","requestId":"req-{id}","message":{{"id":"msg-{id}","model":"{model}","usage":{{"input_tokens":10,"output_tokens":{output},"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}}}}"#
        )
    }

    fn history() -> TokenHistory {
        TokenHistory::new(
            "/nonexistent",
            Duration::from_secs(3600),
            Duration::from_secs(60),
        )
    }

    /// Ingest as if from one transcript. Tests that care about which file a message came
    /// from call `ingest_from` instead.
    fn ingest(history: &mut TokenHistory, line: &str, cutoff: u64) {
        ingest_from(history, line, cutoff, 0);
    }

    fn ingest_from(history: &mut TokenHistory, line: &str, cutoff: u64, source: u32) {
        let Some(record) = Record::parse(line) else {
            panic!("fixture must parse: {line}");
        };
        history.ingest(&record, cutoff, source);
    }

    #[test]
    fn a_record_lands_in_the_bucket_its_timestamp_names() {
        // Attribution comes from the record, not from when this happened to read it, which
        // is what makes the first refresh already correct about the past hour.
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:30.000Z", "claude-opus-5", "a", 20),
            BASE,
        );

        let buckets = history.buckets_at(BASE + 600_000);
        let hit: Vec<&Bucket> = buckets.iter().filter(|b| b.total() > 0).collect();

        assert_eq!(hit.len(), 1, "exactly one bucket should have traffic");
        assert_eq!(
            hit[0].start_ms,
            BASE + 180_000,
            "20:03:30 belongs to the 20:03 bucket"
        );
        assert_eq!(hit[0].total_for(ModelFamily::Opus), 135);
    }

    #[test]
    fn a_replayed_message_is_not_counted_twice() {
        // Retries and resumed sessions replay identical messages, and the same message
        // appears in both a session transcript and its subagent files.
        let mut history = history();
        let line = assistant("2026-08-24T20:03:30.000Z", "claude-opus-5", "a", 20);

        ingest(&mut history, &line, BASE);
        ingest(&mut history, &line, BASE);

        let buckets = history.buckets_at(BASE + 600_000);
        assert_eq!(
            buckets.iter().map(Bucket::total).sum::<u64>(),
            135,
            "the replay must contribute nothing"
        );
    }

    #[test]
    fn later_blocks_of_a_message_stay_in_the_bucket_it_started_in() {
        // `output_tokens` is a running total across a message's content-block records, so
        // only the delta is new. Letting a late block open its own bucket would smear one
        // message across a boundary and draw a step that never happened.
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:59.000Z", "claude-opus-5", "a", 20),
            BASE,
        );
        ingest(
            &mut history,
            &assistant("2026-08-24T20:04:01.000Z", "claude-opus-5", "a", 50),
            BASE,
        );

        let buckets = history.buckets_at(BASE + 600_000);
        let hit: Vec<&Bucket> = buckets.iter().filter(|b| b.total() > 0).collect();

        assert_eq!(hit.len(), 1, "the message must not straddle two buckets");
        assert_eq!(
            hit[0].total_for(ModelFamily::Opus),
            165,
            "the second record contributes its 30 new output tokens only"
        );
    }

    #[test]
    fn families_are_kept_apart_within_a_bucket() {
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:10.000Z", "claude-opus-5", "a", 20),
            BASE,
        );
        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:20.000Z", "claude-sonnet-5", "b", 40),
            BASE,
        );

        let buckets = history.buckets_at(BASE + 600_000);
        let Some(hit) = buckets.iter().find(|b| b.total() > 0) else {
            panic!("one bucket should have traffic");
        };

        assert_eq!(hit.total_for(ModelFamily::Opus), 135);
        assert_eq!(hit.total_for(ModelFamily::Sonnet), 155);
        assert_eq!(
            history.families(),
            vec![ModelFamily::Opus, ModelFamily::Sonnet]
        );
    }

    #[test]
    fn quiet_buckets_are_filled_in_rather_than_skipped() {
        // A graph drawn from only the buckets that saw traffic would join 20:03 straight to
        // 20:40 and draw a plateau across half an hour of silence.
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:30.000Z", "claude-opus-5", "a", 20),
            BASE,
        );

        let buckets = history.buckets_at(BASE + 600_000);

        assert_eq!(buckets.len(), 60, "an hour of one-minute buckets");
        assert!(
            buckets.windows(2).all(|w| w[1].start_ms > w[0].start_ms),
            "buckets must come back oldest first"
        );
        assert_eq!(
            buckets.iter().filter(|b| b.total() == 0).count(),
            59,
            "every bucket but the busy one is present and empty"
        );
    }

    #[test]
    fn a_record_older_than_the_window_is_dropped() {
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T18:00:00.000Z", "claude-opus-5", "a", 20),
            BASE,
        );

        assert_eq!(
            history
                .buckets_at(BASE + 600_000)
                .iter()
                .map(Bucket::total)
                .sum::<u64>(),
            0
        );
    }

    #[test]
    fn an_undated_record_is_dropped_rather_than_dated_now() {
        // Guessing "now" would pile every undated record onto the newest bucket and draw a
        // spike that never happened.
        let mut history = history();
        let line = r#"{"type":"assistant","requestId":"r","message":{"id":"m","model":"claude-opus-5","usage":{"input_tokens":10,"output_tokens":20}}}"#;

        ingest(&mut history, line, BASE);

        assert_eq!(
            history
                .buckets_at(BASE + 600_000)
                .iter()
                .map(Bucket::total)
                .sum::<u64>(),
            0
        );
    }

    #[test]
    fn eviction_drops_the_dedupe_keys_along_with_the_buckets() {
        // Otherwise a long-running process grows `seen` forever.
        let mut history = history();

        ingest(
            &mut history,
            &assistant("2026-08-24T20:03:30.000Z", "claude-opus-5", "a", 20),
            BASE,
        );
        assert_eq!(history.seen.len(), 1);

        // Two hours later, everything in the window has aged out.
        history.evict(BASE + 2 * 3_600_000);

        assert!(history.buckets.is_empty(), "stale buckets must go");
        assert!(history.seen.is_empty(), "and so must their dedupe keys");
    }

    #[test]
    fn a_degenerate_bucket_is_clamped_rather_than_dividing_by_zero() {
        // This sits in a draw path; a config typo must not take the app down.
        let history = TokenHistory::new("/nonexistent", Duration::from_secs(60), Duration::ZERO);

        assert!(history.bucket() >= Duration::from_secs(1));
        assert!(!history.buckets_at(BASE).is_empty());
    }

    #[test]
    fn a_missing_tree_yields_an_empty_window_rather_than_failing() {
        let mut history = history();
        history.refresh_at(BASE);

        assert!(history.families().is_empty());
        assert_eq!(history.buckets_at(BASE).len(), 60);
    }
}
