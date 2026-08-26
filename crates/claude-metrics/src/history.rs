//! Token usage bucketed by wall-clock time, across every transcript in the tree.
//!
//! # Why this is not built from the live session state
//!
//! [`crate::ClaudeMetrics::refresh`] tails only the sessions currently in the registry, and
//! drops a session's state as soon as it leaves. That is right for "what is running now",
//! and wrong for "what happened recently" -- a session that exited five minutes ago
//! contributed real tokens that would silently vanish from the graph the moment it closed.
//!
//! So this walks `<root>/projects` itself.
//!
//! # Why the records carry their own time
//!
//! Every billable record has an ISO-8601 `timestamp`, so a bucket is attributed from the
//! record rather than from when this happened to read it. That means history is correct on
//! the very first refresh -- the past is already on disk -- instead of having to be
//! accumulated live before the graph says anything.
//!
//! # Why the buckets are kept per file
//!
//! A transcript can be replaced or truncated under us, and when it is, everything it
//! contributed has to be taken back. Holding one merged map would make that impossible
//! without re-reading the whole tree; holding each file's own buckets makes a retraction a
//! single map removal. The merged view is produced on demand by [`TokenHistory::aggregate`],
//! which is cheap because only buckets that saw traffic exist at all -- a real month holds
//! on the order of ten thousand across the whole tree, not one per interval.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

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
    /// Which tracked file claimed it. A replaced file has to forget its own dedupe state,
    /// and this is what makes that a targeted retraction rather than a global one.
    source: SourceId,
}

/// Identifies one tracked transcript for the lifetime of a [`TokenHistory`].
type SourceId = u32;

/// One transcript being tailed, and everything it contributed.
///
/// Its buckets live here rather than in a merged map so that dropping this struct is a
/// complete retraction of the file's contribution -- which is exactly what a replaced or
/// truncated file needs.
#[derive(Debug)]
struct Tracked {
    id: SourceId,
    tailer: Tailer,
    buckets: BTreeMap<u64, Vec<(ModelFamily, TokenTotals)>>,
}

/// How much of a cold read is still outstanding.
#[derive(Clone, Copy, Debug)]
struct Warmup {
    /// Bytes to read when the warm-up started.
    total: u64,
    /// Bytes still to read.
    remaining: u64,
}

/// Token usage over a rolling window, bucketed by time.
///
/// Refreshing is incremental: each transcript keeps a checkpoint, so a refresh only parses
/// what has been appended since the last one. [`TokenHistory::save`] persists those
/// checkpoints along with the buckets they produced, so a restart does not have to re-read
/// the tree either.
#[derive(Debug)]
pub struct TokenHistory {
    root: PathBuf,
    window: Duration,
    bucket: Duration,
    /// Keyed by id rather than by path so that a dedupe hit, which names the file that
    /// *claimed* the message rather than the one being read, is a direct lookup.
    files: HashMap<SourceId, Tracked>,
    by_path: HashMap<PathBuf, SourceId>,
    next_source: SourceId,
    /// Dedupe key mapped to what it contributed. Retries and resumed sessions replay
    /// identical messages, and a message can appear in more than one transcript, so this
    /// spans files rather than living inside [`Tracked`].
    seen: HashMap<u64, Counted>,
    warmup: Option<Warmup>,
}

/// How many bytes one [`TokenHistory::refresh_bounded`] call will read.
///
/// Sized so a pass costs tens of milliseconds rather than seconds. The point is not to make
/// the cold read faster -- it is the same total work -- but to let the caller publish a
/// partially-filled window and a progress figure instead of going quiet until the whole
/// tree is done. On a real tree the first read is hundreds of megabytes.
pub const REFRESH_CHUNK_BYTES: u64 = 32 * 1024 * 1024;

/// The checkpoint format version. Bumped whenever the shape below changes; a file written
/// by any other version is discarded and rebuilt rather than guessed at.
const CHECKPOINT_VERSION: u32 = 1;

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
            files: HashMap::new(),
            by_path: HashMap::new(),
            next_source: 0,
            seen: HashMap::new(),
            warmup: None,
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

    /// How far through a cold read this is, or `None` once it has caught up.
    ///
    /// Worth surfacing: the first read of a large tree takes several passes, and a graph
    /// that is quietly showing a tenth of the data looks the same as one showing all of it.
    #[must_use]
    pub fn warmup_progress(&self) -> Option<f64> {
        let warmup = self.warmup?;

        if warmup.total == 0 {
            return None;
        }

        // Computed in integers and converted once. A byte count can exceed what an `f64`
        // represents exactly, and two lossy casts either side of a division is a needlessly
        // sloppy way to arrive at two significant figures.
        let done = warmup.total.saturating_sub(warmup.remaining);
        let permille = done.saturating_mul(1000) / warmup.total;

        Some(f64::from(u32::try_from(permille).unwrap_or(1000)) / 1000.0)
    }

    /// Re-read whatever the transcripts have appended and drop anything now out of window.
    ///
    /// `now_ms` is passed in rather than read from the clock so this is testable without
    /// waiting for real time to pass.
    pub fn refresh_at(&mut self, now_ms: u64) {
        self.refresh_bounded_at(now_ms, u64::MAX);
    }

    /// Refresh against the system clock.
    pub fn refresh(&mut self) {
        self.refresh_at(now_ms());
    }

    /// Refresh, reading at most `max_bytes` this pass.
    ///
    /// Nothing is lost by stopping early -- each tailer's offset advances over exactly what
    /// it consumed, so the next call resumes there. Returns whether the tree is fully
    /// caught up.
    pub fn refresh_bounded(&mut self, max_bytes: u64) -> bool {
        self.refresh_bounded_at(now_ms(), max_bytes)
    }

    /// [`Self::refresh_bounded`] against a supplied clock.
    pub fn refresh_bounded_at(&mut self, now_ms: u64, max_bytes: u64) -> bool {
        let cutoff = now_ms.saturating_sub(millis(self.window));
        let candidates = transcripts_modified_since(&self.root, cutoff);

        self.forget_missing(&candidates);

        let mut budget = max_bytes;

        for (path, _) in &candidates {
            let path = path.clone();
            let source = if let Some(id) = self.by_path.get(&path) {
                *id
            } else {
                let id = self.next_source;
                self.next_source = self.next_source.wrapping_add(1);

                self.by_path.insert(path.clone(), id);
                self.files.insert(
                    id,
                    Tracked {
                        id,
                        tailer: Tailer::new(path),
                        buckets: BTreeMap::new(),
                    },
                );

                id
            };

            let Some(tracked) = self.files.get_mut(&source) else {
                continue;
            };

            let before = tracked.tailer.offset();
            let (lines, kind) = tracked.tailer.read_filtered(budget, carries_usage);
            let read = tracked.tailer.offset().saturating_sub(before);

            // A replaced file means the checkpoint described a file that is no longer
            // there. Everything it contributed goes with it -- those buckets describe
            // tokens that were counted from a file that no longer exists, and the
            // replacement is about to be read from the top and count them again.
            //
            // Only this file's dedupe keys are dropped. Clearing the whole map, which is
            // what this did originally, is survivable at a handful of tracked files and
            // wrong at a few hundred: every other file's messages become re-countable into
            // buckets that already hold them.
            if kind == ReadKind::Restarted {
                tracked.buckets.clear();
                self.seen.retain(|_, counted| counted.source != source);
            }

            for line in lines {
                let Some(record) = Record::parse(&line) else {
                    continue;
                };

                self.ingest(&record, cutoff, source);
            }

            budget = budget.saturating_sub(read);
            if budget == 0 {
                break;
            }
        }

        self.evict(cutoff);
        self.update_warmup(&candidates);

        self.warmup.is_none()
    }

    /// Drop files that have gone from the tree, along with what they contributed.
    ///
    /// A transcript can be deleted, or simply age out of the modification-time filter. In
    /// either case it must stop contributing -- otherwise a deleted session's tokens sit in
    /// the window forever, since nothing else would ever revisit them.
    fn forget_missing(&mut self, candidates: &[(PathBuf, u64)]) {
        let present: std::collections::HashSet<&PathBuf> =
            candidates.iter().map(|(path, _)| path).collect();
        let dropped: std::collections::HashSet<SourceId> = self
            .by_path
            .iter()
            .filter(|(path, _)| !present.contains(path))
            .map(|(_, id)| *id)
            .collect();

        if dropped.is_empty() {
            return;
        }

        self.by_path.retain(|path, _| present.contains(path));
        self.files.retain(|id, _| !dropped.contains(id));
        self.seen
            .retain(|_, counted| !dropped.contains(&counted.source));
    }

    /// Recompute how much of a cold read is left.
    ///
    /// Counted over every *candidate*, not just the files already tracked. A bounded pass
    /// stops partway down the list, so the files it never reached have not been opened at
    /// all -- and summing only what is tracked reported "caught up" while most of the tree
    /// was still unread, which silently truncated the window to whatever the first pass
    /// happened to cover.
    ///
    /// Lengths come from the directory walk that produced the candidates, so this costs no
    /// extra `stat` calls.
    fn update_warmup(&mut self, candidates: &[(PathBuf, u64)]) {
        let remaining: u64 = candidates
            .iter()
            .map(|(path, len)| match self.by_path.get(path) {
                Some(id) => self
                    .files
                    .get(id)
                    .map_or(*len, |tracked| len.saturating_sub(tracked.tailer.offset())),
                // Never opened, so all of it is outstanding.
                None => *len,
            })
            .sum();

        self.warmup = match (remaining, self.warmup) {
            (0, _) => None,
            // Already warming: keep the original total so the fraction only ever climbs.
            (remaining, Some(warmup)) => Some(Warmup {
                total: warmup.total.max(remaining),
                remaining,
            }),
            (remaining, None) => Some(Warmup {
                total: remaining,
                remaining,
            }),
        };
    }

    /// Fold one record into its bucket, attributed to the transcript it came from.
    fn ingest(&mut self, record: &Record, cutoff: u64, source: SourceId) {
        let Some(usage) = record.billable_usage() else {
            return;
        };

        let Some(key) = record.dedupe_hash() else {
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
            let (family, bucket, owner) = (counted.family, counted.bucket, counted.source);
            counted.output = usage.output;

            if let Some(totals) = self.totals_for_mut(owner, bucket, family) {
                totals.output = totals.output.saturating_add(delta);
            }
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

        let Some(totals) = self.totals_for_mut(source, bucket, family) else {
            return;
        };

        totals.input = totals.input.saturating_add(usage.input);
        totals.cache_read = totals.cache_read.saturating_add(usage.cache_read);
        totals.cache_creation = totals.cache_creation.saturating_add(usage.cache_creation);
        totals.output = totals.output.saturating_add(usage.output);
    }

    /// The totals slot for one family in one bucket of one file, created if absent.
    ///
    /// `None` when `source` names no tracked file. That is unreachable from
    /// [`Self::ingest`] -- the owning file is always still tracked, since dropping one also
    /// drops the dedupe keys pointing at it -- but it is returned rather than asserted
    /// because this sits in a refresh loop, where a panic is a far worse failure than
    /// losing one record.
    fn totals_for_mut(
        &mut self, source: SourceId, bucket: u64, family: ModelFamily,
    ) -> Option<&mut TokenTotals> {
        let tracked = self.files.get_mut(&source)?;
        let entry = tracked.buckets.entry(bucket).or_default();

        if let Some(index) = entry.iter().position(|(candidate, _)| *candidate == family) {
            return Some(&mut entry[index].1);
        }

        entry.push((family, TokenTotals::default()));
        entry.last_mut().map(|(_, totals)| totals)
    }

    /// Drop buckets, and the dedupe keys pointing at them, that have aged out.
    fn evict(&mut self, cutoff: u64) {
        let stale = cutoff - (cutoff % millis(self.bucket));

        for tracked in self.files.values_mut() {
            tracked.buckets.retain(|start, _| *start >= stale);
        }

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
    #[must_use]
    pub fn aggregate_at(&self, now_ms: u64, window: Duration, bucket: Duration) -> Vec<Bucket> {
        let step = millis(bucket).max(1);
        let newest = now_ms - (now_ms % step);
        let count = (millis(window) / step).max(1);
        let oldest = newest.saturating_sub(step.saturating_mul(count - 1));

        let mut slots: Vec<Bucket> = (0..count)
            .map(|index| Bucket {
                start_ms: oldest + index * step,
                totals: Vec::new(),
            })
            .collect();

        // Walked per file and scattered into slots, rather than per slot and gathered from
        // files: a slot-major loop would range-query every file's map for every slot, which
        // at a hundred and twenty slots and two hundred files is twenty-four thousand
        // lookups to place ten thousand entries.
        for tracked in self.files.values() {
            let upper = newest.saturating_add(step);

            for (start, entries) in tracked.buckets.range(oldest..upper) {
                let Ok(index) = usize::try_from((start - oldest) / step) else {
                    continue;
                };

                let Some(slot) = slots.get_mut(index) else {
                    continue;
                };

                for (family, add) in entries {
                    match slot.totals.iter_mut().find(|(seen, _)| seen == family) {
                        Some((_, into)) => into.merge(*add),
                        None => slot.totals.push((*family, *add)),
                    }
                }
            }
        }

        slots
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
                self.files
                    .values()
                    .flat_map(|tracked| tracked.buckets.values())
                    .flatten()
                    .any(|(candidate, totals)| candidate == family && totals.total() > 0)
            })
            .collect()
    }

    /// Write the checkpoint to `path`.
    ///
    /// This is what makes a second start instant. Without it every launch re-reads whatever
    /// the modification-time filter lets through, which at a month-wide window is the whole
    /// tree -- long-lived sessions keep old transcripts freshly modified, so the filter
    /// stops filtering well before a month.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O or serialisation error. Callers should treat a failure as
    /// "no checkpoint", not as fatal: the history is still correct in memory, and the next
    /// start just pays for a cold read.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut sources: Vec<SourceId> = Vec::with_capacity(self.files.len());
        let files: Vec<CheckpointFile> = self
            .files
            .values()
            .map(|tracked| {
                sources.push(tracked.id);

                CheckpointFile {
                    path: tracked.tailer.path().to_path_buf(),
                    inode: tracked.tailer.inode(),
                    offset: tracked.tailer.offset(),
                    buckets: tracked
                        .buckets
                        .iter()
                        .flat_map(|(start, entries)| {
                            entries.iter().map(move |(family, totals)| {
                                (
                                    *start,
                                    family_index(*family),
                                    totals.input,
                                    totals.output,
                                    totals.cache_read,
                                    totals.cache_creation,
                                )
                            })
                        })
                        .collect(),
                }
            })
            .collect();

        // The saved source id is the file's index in the vector above, so ids stay dense
        // and the loader does not have to carry a separate mapping.
        let seen: Vec<(u64, u8, u64, u64, u32)> = self
            .seen
            .iter()
            .filter_map(|(key, counted)| {
                let index = sources.iter().position(|id| *id == counted.source)?;

                Some((
                    *key,
                    family_index(counted.family),
                    counted.output,
                    counted.bucket,
                    u32::try_from(index).unwrap_or(u32::MAX),
                ))
            })
            .collect();

        let checkpoint = Checkpoint {
            version: CHECKPOINT_VERSION,
            bucket_ms: millis(self.bucket),
            files,
            seen,
        };

        let encoded = serde_json::to_vec(&checkpoint)?;

        // Written beside the target and renamed, so a crash or a concurrent reader never
        // sees a half-written checkpoint -- which would be discarded on load anyway, but
        // only after the cold read it was supposed to avoid.
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, encoded)?;
        std::fs::rename(&temporary, path)
    }

    /// Restore a checkpoint written by [`Self::save`].
    ///
    /// A missing, unreadable, corrupt, or differently-versioned file is not an error --
    /// it just means a cold read. So is a checkpoint taken at a different bucket width,
    /// since its buckets cannot be re-cut onto the current grid.
    ///
    /// The tailers restored here are not trusted: each re-stats its file on the next read
    /// and starts over if the inode changed or the file shrank, exactly as it would for a
    /// checkpoint taken a second ago.
    pub fn load(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };

        let Ok(checkpoint) = serde_json::from_slice::<Checkpoint>(&bytes) else {
            return;
        };

        if checkpoint.version != CHECKPOINT_VERSION || checkpoint.bucket_ms != millis(self.bucket) {
            return;
        }

        self.files.clear();
        self.by_path.clear();
        self.seen.clear();
        self.next_source = 0;

        let mut ids: Vec<SourceId> = Vec::with_capacity(checkpoint.files.len());

        for file in checkpoint.files {
            let id = self.next_source;
            self.next_source = self.next_source.wrapping_add(1);
            ids.push(id);

            let mut buckets: BTreeMap<u64, Vec<(ModelFamily, TokenTotals)>> = BTreeMap::new();

            for (start, family, input, output, cache_read, cache_creation) in file.buckets {
                let Some(family) = family_at(family) else {
                    continue;
                };

                buckets.entry(start).or_default().push((
                    family,
                    TokenTotals {
                        input,
                        output,
                        cache_read,
                        cache_creation,
                    },
                ));
            }

            self.by_path.insert(file.path.clone(), id);
            self.files.insert(
                id,
                Tracked {
                    id,
                    tailer: Tailer::restore(file.path, file.offset, file.inode),
                    buckets,
                },
            );
        }

        for (key, family, output, bucket, index) in checkpoint.seen {
            let (Some(family), Some(source)) =
                (family_at(family), ids.get(index as usize).copied())
            else {
                continue;
            };

            self.seen.insert(
                key,
                Counted {
                    family,
                    output,
                    bucket,
                    source,
                },
            );
        }
    }
}

/// The on-disk checkpoint.
///
/// Tuples rather than named structs for the bulk arrays: there are tens of thousands of
/// them and the field names would be most of the file.
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    /// The grid the buckets were cut on. A checkpoint at a different width is discarded,
    /// since its buckets cannot be re-cut without the records that made them.
    bucket_ms: u64,
    files: Vec<CheckpointFile>,
    /// `(key hash, family, output high-water, bucket start, file index)`.
    seen: Vec<(u64, u8, u64, u64, u32)>,
}

#[derive(Serialize, Deserialize)]
struct CheckpointFile {
    path: PathBuf,
    inode: Option<u64>,
    offset: u64,
    /// `(bucket start, family, input, output, cache read, cache creation)`.
    buckets: Vec<(u64, u8, u64, u64, u64, u64)>,
}

fn family_index(family: ModelFamily) -> u8 {
    u8::try_from(
        ModelFamily::ALL
            .iter()
            .position(|candidate| *candidate == family)
            .unwrap_or(0),
    )
    .unwrap_or(0)
}

fn family_at(index: u8) -> Option<ModelFamily> {
    ModelFamily::ALL.get(index as usize).copied()
}

/// Whether a raw transcript line could carry token usage.
///
/// Applied to the bytes before any UTF-8 conversion or JSON parse. Transcripts are
/// overwhelmingly user turns, tool results, and system records, none of which have a usage
/// object -- on a real tree this rejects most of the bytes for the cost of a substring
/// search, and the JSON parser only ever sees lines that might matter.
fn carries_usage(line: &[u8]) -> bool {
    memfind(line, b"\"usage\"")
}

fn memfind(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() {
        return false;
    }

    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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

/// Every `*.jsonl` transcript under `<root>/projects` touched at or after `cutoff`.
///
/// Two levels matter, not one:
///
/// - `<root>/projects/<project>/<session>.jsonl` -- the main session transcript.
/// - `<root>/projects/<project>/<session>/subagents/<agent>.jsonl` -- one per subagent.
///
/// The subagent files are not optional detail. On a real tree they outnumber the session
/// transcripts four to one and hold more bytes than all of them together, and their
/// messages are almost entirely *disjoint* from the parent's -- a subagent's turns are
/// billed to the account but written only here. Walking one level, which is what this did
/// originally, undercounted a busy day by nearly an order of magnitude.
///
/// The modification-time filter keeps the walk affordable on a tree with a long history: a
/// file last written before the window opened cannot contain a record inside it, so it is
/// never opened at all. Note that it stops filtering much beyond a week in practice --
/// long-lived sessions keep old transcripts freshly modified.
fn transcripts_modified_since(root: &Path, cutoff: u64) -> Vec<(PathBuf, u64)> {
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

            if path.extension().is_some_and(|ext| ext == "jsonl") {
                push_if_fresh(&mut found, &child, path, cutoff);
                continue;
            }

            // A session directory. Its subagent transcripts sit one level further down.
            let Ok(agents) = std::fs::read_dir(path.join("subagents")) else {
                continue;
            };

            for agent in agents.flatten() {
                let path = agent.path();

                if path.extension().is_some_and(|ext| ext == "jsonl") {
                    push_if_fresh(&mut found, &agent, path, cutoff);
                }
            }
        }
    }

    found
}

/// Keep `path` if it was touched inside the window, along with its length.
///
/// The length rides along because the caller needs it to size a cold read, and this is the
/// one place the file is already being stat'd.
fn push_if_fresh(
    found: &mut Vec<(PathBuf, u64)>, entry: &std::fs::DirEntry, path: PathBuf, cutoff: u64,
) {
    let metadata = entry.metadata().ok();
    let len = metadata.as_ref().map_or(0, std::fs::Metadata::len);

    let modified = metadata
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(millis);

    // A file with no readable mtime is read rather than skipped. Being wrong in the cheap
    // direction costs one parse; being wrong the other way loses data.
    if modified.is_none_or(|stamp| stamp >= cutoff) {
        found.push((path, len));
    }
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

    /// Ingest as if `source` were a tracked transcript.
    ///
    /// Buckets live inside the file that produced them, so a record can only land if its
    /// source is registered -- the refresh loop does that before it reads a line.
    fn ingest_from(history: &mut TokenHistory, line: &str, cutoff: u64, source: u32) {
        history.files.entry(source).or_insert_with(|| Tracked {
            id: source,
            tailer: Tailer::new(format!("/nonexistent/{source}.jsonl")),
            buckets: BTreeMap::new(),
        });

        let Some(record) = Record::parse(line) else {
            panic!("fixture must parse: {line}");
        };
        history.ingest(&record, cutoff, source);
    }

    /// Every bucket entry the history is holding, across all tracked files.
    fn stored(history: &TokenHistory) -> usize {
        history
            .files
            .values()
            .map(|tracked| tracked.buckets.len())
            .sum()
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

        assert_eq!(stored(&history), 0, "stale buckets must go");
        assert!(history.seen.is_empty(), "and so must their dedupe keys");
    }

    // ---- tree walking, warm-up, and checkpointing ----

    /// A throwaway `~/.claude` with a project, a session transcript, and subagent files.
    fn tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("claude-metrics-history-{tag}"));
        let _ = std::fs::remove_dir_all(&root);

        let project = root.join("projects").join("-Users-someone-code-thing");
        std::fs::create_dir_all(project.join("session-1").join("subagents")).unwrap();

        root
    }

    fn write_lines(path: &Path, lines: &[String]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }

        let mut body = String::new();
        for line in lines {
            body.push_str(line);
            body.push('\n');
        }

        std::fs::write(path, body).unwrap();
    }

    fn tree_history(root: &Path) -> TokenHistory {
        TokenHistory::new(root, Duration::from_hours(24), Duration::from_secs(60))
    }

    #[test]
    fn subagent_transcripts_are_walked_not_just_session_ones() {
        // The bug this exists to stop coming back. Subagent files sit a level deeper, and
        // on a real tree they outnumber session transcripts four to one and hold more bytes
        // than all of them together -- their turns are billed but written only there.
        // Walking one level undercounted a busy day by nearly an order of magnitude.
        let root = tree("subagents");
        let project = root.join("projects").join("-Users-someone-code-thing");

        write_lines(
            &project.join("session-1.jsonl"),
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "main",
                20,
            )],
        );
        write_lines(
            &project
                .join("session-1")
                .join("subagents")
                .join("agent-a.jsonl"),
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "sub",
                20,
            )],
        );

        let mut history = tree_history(&root);
        history.refresh_at(BASE + 60_000);

        let total: u64 = history
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();

        assert_eq!(
            total,
            2 * PER_MESSAGE,
            "both the session transcript and its subagent must count"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_bounded_refresh_does_not_claim_to_be_done_with_files_it_never_opened() {
        // A bounded pass stops partway down the candidate list. Counting outstanding bytes
        // over only the files already tracked reported "caught up" while most of the tree
        // was unread, which silently truncated the window to whatever the first pass
        // happened to reach -- and did it differently run to run, since directory order
        // decides which files those are.
        let root = tree("unopened");
        let project = root.join("projects").join("-Users-someone-code-thing");

        for index in 0..6 {
            let lines: Vec<String> = (0..20)
                .map(|i| {
                    assistant(
                        "2026-08-24T20:00:00.000Z",
                        "claude-opus-5",
                        &format!("f{index}m{i}"),
                        20,
                    )
                })
                .collect();

            write_lines(&project.join(format!("session-{index}.jsonl")), &lines);
        }

        let mut history = tree_history(&root);

        // Small enough that a pass cannot reach every file.
        let mut passes = 0;
        while !history.refresh_bounded_at(BASE + 60_000, 1024) {
            passes += 1;
            assert!(passes < 200, "a bounded read must terminate");
        }

        let total: u64 = history
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();

        assert_eq!(
            total,
            6 * 20 * PER_MESSAGE,
            "every file has to be read before the scan calls itself done"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_bounded_refresh_reads_the_rest_on_the_next_pass() {
        // Nothing may be lost by stopping early: each tailer's offset advances over exactly
        // what it consumed, so a cold read can be spread over several passes.
        let root = tree("bounded");
        let project = root.join("projects").join("-Users-someone-code-thing");

        let lines: Vec<String> = (0..40)
            .map(|i| {
                assistant(
                    "2026-08-24T20:00:00.000Z",
                    "claude-opus-5",
                    &format!("m{i}"),
                    20,
                )
            })
            .collect();
        write_lines(&project.join("session-1.jsonl"), &lines);

        let mut history = tree_history(&root);

        let done = history.refresh_bounded_at(BASE + 60_000, 512);
        assert!(!done, "512 bytes cannot cover forty records");
        assert!(history.warmup_progress().is_some_and(|p| p < 1.0));

        let partial: u64 = history
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();
        assert!(partial > 0, "a bounded pass still contributes what it read");

        while !history.refresh_bounded_at(BASE + 60_000, 512) {}

        let total: u64 = history
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();
        assert_eq!(total, 40 * PER_MESSAGE, "and the rest arrives afterwards");
        assert!(
            history.warmup_progress().is_none(),
            "caught up means no progress bar"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_checkpoint_round_trips_without_re_reading() {
        let root = tree("checkpoint");
        let project = root.join("projects").join("-Users-someone-code-thing");
        let checkpoint = root.join("history.json");

        write_lines(
            &project.join("session-1.jsonl"),
            &[
                assistant("2026-08-24T20:00:00.000Z", "claude-opus-5", "a", 20),
                assistant("2026-08-24T20:00:30.000Z", "claude-sonnet-5", "b", 20),
            ],
        );

        let mut original = tree_history(&root);
        original.refresh_at(BASE + 60_000);
        original.save(&checkpoint).unwrap();

        let mut restored = tree_history(&root);
        restored.load(&checkpoint);

        assert_eq!(
            restored.buckets_at(BASE + 60_000),
            original.buckets_at(BASE + 60_000),
            "a restored history must draw the same graph"
        );
        assert_eq!(restored.families(), original.families());

        // And the whole point: a refresh after restoring re-reads nothing, so the totals do
        // not double.
        restored.refresh_at(BASE + 60_000);

        assert_eq!(
            restored.buckets_at(BASE + 60_000),
            original.buckets_at(BASE + 60_000),
            "resuming from the checkpoint must not re-count what it already holds"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_checkpoint_from_a_different_grid_is_discarded_rather_than_misread() {
        // Buckets cut at one width cannot be re-cut at another without the records that
        // made them, so the only safe move is a cold read.
        let root = tree("regrid");
        let project = root.join("projects").join("-Users-someone-code-thing");
        let checkpoint = root.join("history.json");

        write_lines(
            &project.join("session-1.jsonl"),
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "a",
                20,
            )],
        );

        let mut original = tree_history(&root);
        original.refresh_at(BASE + 60_000);
        original.save(&checkpoint).unwrap();

        let mut other = TokenHistory::new(&root, Duration::from_hours(24), Duration::from_secs(10));
        other.load(&checkpoint);

        assert_eq!(stored(&other), 0, "nothing may be adopted across grids");

        // ...and it still works, by reading the tree.
        other.refresh_at(BASE + 60_000);
        let total: u64 = other
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();
        assert_eq!(total, PER_MESSAGE);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_corrupt_or_missing_checkpoint_is_not_an_error() {
        let root = tree("corrupt");
        let mut history = tree_history(&root);

        history.load(&root.join("nope.json"));
        assert_eq!(stored(&history), 0);

        std::fs::write(root.join("junk.json"), b"{not json").unwrap();
        history.load(&root.join("junk.json"));
        assert_eq!(stored(&history), 0);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_deleted_transcript_stops_contributing() {
        // Otherwise a removed session's tokens sit in the window forever -- nothing else
        // would ever revisit them.
        let root = tree("deleted");
        let project = root.join("projects").join("-Users-someone-code-thing");
        let transcript = project.join("session-1.jsonl");

        write_lines(
            &transcript,
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "a",
                20,
            )],
        );

        let mut history = tree_history(&root);
        history.refresh_at(BASE + 60_000);
        assert!(stored(&history) > 0);

        std::fs::remove_file(&transcript).unwrap();
        history.refresh_at(BASE + 60_000);

        assert_eq!(stored(&history), 0, "its buckets go with it");
        assert!(history.seen.is_empty(), "and so do its dedupe keys");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_replaced_transcript_retracts_what_it_contributed() {
        // The file is read from the top again, so its old buckets have to go first or the
        // replacement's records land on top of the originals.
        let root = tree("replaced");
        let project = root.join("projects").join("-Users-someone-code-thing");
        let transcript = project.join("session-1.jsonl");

        write_lines(
            &transcript,
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "a",
                20,
            )],
        );

        let mut history = tree_history(&root);
        history.refresh_at(BASE + 60_000);

        // Same length, different inode: only the identity check catches this.
        let replacement = project.join("other.jsonl");
        write_lines(
            &replacement,
            &[assistant(
                "2026-08-24T20:00:00.000Z",
                "claude-opus-5",
                "a",
                20,
            )],
        );
        std::fs::rename(&replacement, &transcript).unwrap();

        history.refresh_at(BASE + 60_000);

        let total: u64 = history
            .buckets_at(BASE + 60_000)
            .iter()
            .map(Bucket::total)
            .sum();
        assert_eq!(total, PER_MESSAGE, "counted once, not twice");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_line_prefilter_keeps_every_billable_record() {
        // It runs on raw bytes before the JSON parse, so a false negative silently drops
        // real tokens.
        let record = assistant("2026-08-24T20:00:00.000Z", "claude-opus-5", "a", 20);
        assert!(carries_usage(record.as_bytes()));

        assert!(!carries_usage(
            br#"{"type":"user","message":{"content":"hi"}}"#
        ));
        assert!(!carries_usage(b""));
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
