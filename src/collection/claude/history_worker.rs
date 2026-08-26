//! The transcript scan, on a thread of its own.
//!
//! # Why this is not on the collection thread
//!
//! The stats graph reaches back thirty days, and thirty days is not a subset of the tree --
//! long-lived sessions keep old transcripts freshly modified, so the modification-time
//! filter stops filtering well before a month and the window is effectively everything.
//! Measured on a real tree that is around sixteen hundred files and six hundred megabytes
//! on a cold start, which is seconds of parsing. Spending that on the collection thread
//! stalls every other widget in the layout, and doing it in bounded slices there instead
//! stretches the same work across minutes of ticks.
//!
//! So the scan gets its own thread and publishes snapshots. A harvest reads the latest one
//! and never blocks, which also takes the steady-state cost -- a couple of thousand `stat`
//! calls a second across the tracked files -- off the collection path.
//!
//! # Why the cold read is still chunked
//!
//! Even here it is worth reading in slices: each slice publishes, so the graph fills in
//! visibly and can report how far along it is, rather than staying empty and then appearing
//! all at once with no way to tell the difference from "you have never used Claude Code".

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel},
    },
    thread,
    time::{Duration, Instant},
};

use claude_metrics::{Bucket, ModelFamily, REFRESH_CHUNK_BYTES, StatsRange, TokenHistory};

/// How long the worker waits between refreshes once it has caught up.
///
/// Faster than this buys nothing: transcripts are appended a message at a time, and the
/// graph's finest bucket is ten seconds wide.
const IDLE_INTERVAL: Duration = Duration::from_millis(500);

/// How often the checkpoint is rewritten.
///
/// Writing it every pass would put a few megabytes through the disk twice a second for a
/// file that only has to be roughly current -- anything it misses is re-read on the next
/// start, which is a bounded cost, not a wrong answer.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(60);

/// What the worker publishes for a harvest to pick up.
#[derive(Clone, Debug, Default)]
pub(super) struct HistorySnapshot {
    /// Buckets rolled up onto `range`.
    pub(super) history: Vec<Bucket>,
    /// The range `history` was rolled up onto, so the painter labels what it is drawing
    /// rather than what has since been asked for.
    pub(super) range: StatsRange,
    /// Families that contributed anything in the retained window.
    pub(super) families: Vec<ModelFamily>,
    /// How far through a cold read, or `None` once caught up.
    pub(super) progress: Option<f64>,
}

/// A handle to the scanning thread.
///
/// Dropping this disconnects the request channel, which is how the worker learns to write
/// its checkpoint and stop.
#[derive(Debug)]
pub(super) struct HistoryWorker {
    requests: Sender<StatsRange>,
    snapshot: Arc<Mutex<HistorySnapshot>>,
    /// The last range asked for, so an unchanged range does not send on every tick.
    last_sent: StatsRange,
}

impl HistoryWorker {
    /// Start scanning `root`, rolling up onto `range` until told otherwise.
    pub(super) fn spawn(
        root: PathBuf, checkpoint: Option<PathBuf>, window: Duration, bucket: Duration,
        range: StatsRange,
    ) -> Self {
        let (requests, receiver) = channel();
        let snapshot = Arc::new(Mutex::new(HistorySnapshot {
            range,
            ..HistorySnapshot::default()
        }));

        let published = Arc::clone(&snapshot);

        // Detached. There is nothing to join on: the worker holds no resource that outlives
        // the process, and a harvest never waits on it, so a shutdown that does not get to
        // write its checkpoint costs one cold read rather than anything worse.
        thread::spawn(move || {
            let mut worker = Scanner {
                history: TokenHistory::new(root, window, bucket),
                checkpoint,
                range,
                published,
                last_saved: None,
            };

            worker.run(&receiver);
        });

        Self {
            requests,
            snapshot,
            last_sent: range,
        }
    }

    /// The most recent snapshot. Never blocks on the scan.
    pub(super) fn latest(&mut self, range: StatsRange) -> HistorySnapshot {
        if range != self.last_sent {
            // A failed send means the worker is gone, which the next `latest` will show as
            // a stale snapshot. There is nothing useful to do about it here.
            if self.requests.send(range).is_ok() {
                self.last_sent = range;
            }
        }

        // A poisoned lock means the worker panicked mid-publish. Recovering the value is
        // right: the snapshot is plain data with no invariant a panic could have broken,
        // and drawing the last good one beats taking the app down.
        match self.snapshot.lock() {
            Ok(snapshot) => snapshot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// The worker's own state, which never leaves its thread.
struct Scanner {
    history: TokenHistory,
    checkpoint: Option<PathBuf>,
    range: StatsRange,
    published: Arc<Mutex<HistorySnapshot>>,
    last_saved: Option<Instant>,
}

impl Scanner {
    /// Read, publish, and checkpoint until the request channel closes.
    fn run(&mut self, requests: &Receiver<StatsRange>) {
        if let Some(path) = self.checkpoint.clone() {
            self.history.load(&path);
        }

        loop {
            let caught_up = self.history.refresh_bounded(REFRESH_CHUNK_BYTES);
            self.publish();

            if caught_up {
                self.save_if_due();
            }

            // Two different waits, and the distinction is load-bearing. Caught up, this
            // blocks for `IDLE_INTERVAL`. Mid-read it must not block at all, but it still
            // has to *notice* a range change queued between slices -- and
            // `recv_timeout(Duration::ZERO)` does not reliably see one, which is how a key
            // pressed during the first scan of a large tree got silently dropped. Draining
            // with `try_recv` says exactly what is meant instead of leaning on the timing
            // edge of a zero-length timeout.
            let disconnected = if caught_up {
                match requests.recv_timeout(IDLE_INTERVAL) {
                    Ok(range) => {
                        self.range = range;
                        false
                    }
                    Err(RecvTimeoutError::Timeout) => false,
                    Err(RecvTimeoutError::Disconnected) => true,
                }
            } else {
                self.drain(requests)
            };

            if disconnected {
                break;
            }
        }

        self.save();
    }

    /// Take every queued request without blocking. Returns whether the channel has closed.
    fn drain(&mut self, requests: &Receiver<StatsRange>) -> bool {
        loop {
            match requests.try_recv() {
                Ok(range) => self.range = range,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => return true,
            }
        }
    }

    /// Roll the history up onto the current range and hand it to the collection thread.
    fn publish(&self) {
        let snapshot = HistorySnapshot {
            history: self
                .history
                .aggregate(self.range.window(), self.range.bucket()),
            range: self.range,
            families: self.history.families(),
            progress: self.history.warmup_progress(),
        };

        match self.published.lock() {
            Ok(mut published) => *published = snapshot,
            Err(poisoned) => *poisoned.into_inner() = snapshot,
        }
    }

    /// Checkpoint, but no more often than [`CHECKPOINT_INTERVAL`].
    fn save_if_due(&mut self) {
        let due = self
            .last_saved
            .is_none_or(|at| at.elapsed() >= CHECKPOINT_INTERVAL);

        if due {
            self.save();
        }
    }

    /// Write the checkpoint, ignoring a failure.
    ///
    /// A checkpoint that cannot be written is not worth surfacing: the history in memory is
    /// still correct, and the next start just pays for a cold read.
    fn save(&mut self) {
        let Some(path) = self.checkpoint.as_ref() else {
            return;
        };

        if self.history.save(path).is_ok() {
            self.last_saved = Some(Instant::now());
        }
    }
}
