//! Live Claude Code metrics, read via the `claude-metrics` crate.
//!
//! Unlike the power sampler this needs no thread of its own. `ClaudeMetrics::refresh`
//! re-reads a handful of small registry files and consumes only what the transcripts have
//! appended since the last call, so a steady-state tick is cheap.
//!
//! The exception is the *first* tick for a session, which reads its whole transcript. On a
//! long session that is a one-time cost of a few hundred milliseconds on the collection
//! thread. Accepted rather than threaded: it happens once per session, and a thread would
//! buy nothing on every subsequent tick.

mod history_worker;

use std::{path::PathBuf, time::Duration};

use claude_metrics::{Bucket, ClaudeMetrics, ModelFamily, StatsRange, Statusline, TokenTotals};

use history_worker::HistoryWorker;

/// One live session, flattened for display.
#[derive(Clone, Debug, Default)]
pub struct ClaudeSession {
    /// Session UUID.
    pub id: String,
    /// Human-facing session name.
    pub name: String,
    /// Working directory.
    pub cwd: String,
    /// `busy`, `idle`, and others.
    pub status: String,
    /// tmux pane id, e.g. `%28`.
    pub tmux_pane: String,
    /// Model family currently in use, if the statusline cache says.
    pub model: Option<ModelFamily>,
    /// Every token this session has spent, cache included.
    pub tokens: u64,
    /// Subagent messages produced.
    pub agents: u64,
    /// Cost so far in USD, from the statusline cache.
    pub cost_usd: Option<f64>,
    /// Context-window occupancy as a percentage.
    pub context_percent: Option<f64>,
    /// Most recent turn duration, in milliseconds.
    pub last_turn_ms: Option<u64>,
}

/// A snapshot of everything the Claude widgets draw.
#[derive(Clone, Debug, Default)]
pub struct ClaudeData {
    /// Live sessions, newest first.
    pub sessions: Vec<ClaudeSession>,
    /// Cumulative token totals across every live session, by family.
    pub totals: Vec<(ModelFamily, TokenTotals)>,
    /// Rate limits, taken from whichever session reported most recently.
    ///
    /// These are account-wide rather than per-session, so any session's copy will do.
    pub rate_limits: Option<RateLimits>,
    /// Tokens per model family over the selected range, oldest bucket first.
    ///
    /// Empty unless a widget that draws it is on screen -- see
    /// [`crate::app::layout_manager::UsedWidgets::use_claude_stats`]. This is read from the
    /// whole `~/.claude/projects` tree rather than from the live sessions, so it keeps the
    /// tokens of sessions that have since exited.
    pub history: Vec<Bucket>,
    /// Which range `history` was rolled up onto.
    ///
    /// Carried alongside the buckets rather than read back from the widget, so a snapshot
    /// taken just before a range switch cannot be drawn against the new range's axis.
    pub history_range: StatsRange,
    /// Families that contributed anything in the retained window, in a stable draw order.
    pub history_families: Vec<ModelFamily>,
    /// How far through a cold read the scan is, or `None` once it has caught up.
    ///
    /// Worth drawing: a first run on a large tree takes seconds, and a graph quietly
    /// showing a tenth of the data looks exactly like one showing all of it.
    pub history_progress: Option<f64>,
}

/// Account-wide rate-limit consumption.
#[derive(Clone, Copy, Debug, Default)]
pub struct RateLimits {
    /// Rolling 5-hour bucket, as a percentage.
    pub five_hour_percent: f64,
    /// Unix epoch seconds at which the 5-hour bucket resets.
    pub five_hour_resets_at: u64,
    /// Rolling 7-day bucket, as a percentage.
    pub seven_day_percent: f64,
    /// Unix epoch seconds at which the 7-day bucket resets.
    pub seven_day_resets_at: u64,
}

impl From<&Statusline> for RateLimits {
    fn from(statusline: &Statusline) -> Self {
        Self {
            five_hour_percent: statusline.rate_limits.five_hour.used_percentage,
            five_hour_resets_at: statusline.rate_limits.five_hour.resets_at,
            seven_day_percent: statusline.rate_limits.seven_day.used_percentage,
            seven_day_resets_at: statusline.rate_limits.seven_day.resets_at,
        }
    }
}

/// How far back the retained history reaches: whatever the widest selectable range needs.
///
/// This is also the modification-time cutoff the scan filters transcripts by, and so a
/// direct multiplier on a cold read's cost. Measured on a real tree: a day reaches ~200
/// files / ~42MB, a week ~1200 / ~352MB, a month ~1600 / ~600MB. The filter stops filtering
/// much past a week, because long-lived sessions keep old transcripts freshly modified.
/// That is affordable only because the scan runs on its own thread and checkpoints itself
/// -- see [`history_worker`].
fn history_window() -> Duration {
    StatsRange::widest().window()
}

/// How finely that window is divided internally.
///
/// Every range rolls this grid up on demand via `TokenHistory::aggregate`, so the
/// transcripts are parsed once no matter how many views want them, and switching range
/// costs an aggregation rather than a re-scan. Rolling up cannot invent detail, so this has
/// to be at least as fine as the finest range -- `the_internal_grid_can_serve_every_range`
/// holds the two together.
///
/// It is not finer than it needs to be either. Measured over thirty days of a real tree,
/// halving it from ten seconds to five took the stored buckets from 19.6k to 30.6k -- but
/// only the checkpoint from 3.0MB to 3.3MB and its load from 11ms to 13ms, because the
/// per-file records and the dedupe keys dominate both. The cold read did not move at all;
/// it is bound by JSON parsing. Cheap enough for a five-minute view that draws sixty points
/// instead of thirty, and there is nothing finer to buy.
const HISTORY_BUCKET: Duration = Duration::from_secs(5);

/// Where the scan's checkpoint lives.
///
/// A cache directory rather than a config or data one: losing it costs one cold read and
/// nothing else, which is exactly what a cache is for.
fn checkpoint_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("mon").join("claude-history.json"))
}

/// Owns the reader and turns a refresh into a [`ClaudeData`] snapshot.
#[derive(Debug)]
pub struct ClaudeCollector {
    metrics: Option<ClaudeMetrics>,
    /// Started on first use, so a layout without the stats widget never walks the tree.
    history: Option<HistoryWorker>,
}

impl Default for ClaudeCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeCollector {
    /// Build a collector over `$HOME/.claude`.
    ///
    /// With no home directory there is nothing to read and every harvest is empty. That is
    /// a normal state on a machine that has never run Claude Code, not an error.
    pub fn new() -> Self {
        Self {
            metrics: ClaudeMetrics::with_default_root(),
            history: None,
        }
    }

    /// Re-read and snapshot.
    ///
    /// `range` gates the rolling-window scan, which is the only part of a harvest that
    /// touches transcripts outside the live sessions. `None` means no widget that draws it
    /// is on screen, and a layout without one should not pay for it -- nor even start the
    /// thread that does it.
    pub fn harvest(&mut self, range: Option<StatsRange>) -> Option<ClaudeData> {
        let metrics = self.metrics.as_mut()?;
        metrics.refresh();

        let snapshot = range.map(|range| {
            let root = metrics.root().to_path_buf();

            self.history
                .get_or_insert_with(|| {
                    HistoryWorker::spawn(
                        root,
                        checkpoint_path(),
                        history_window(),
                        HISTORY_BUCKET,
                        range,
                    )
                })
                .latest(range)
        });

        let snapshot = snapshot.unwrap_or_default();

        let mut rate_limits = None;

        let sessions = metrics
            .sessions()
            .iter()
            .filter_map(|session| {
                let id = session.session_id.clone()?;
                let statusline = metrics.statusline_for(&id);

                if let Some(statusline) = &statusline
                    && rate_limits.is_none()
                {
                    rate_limits = Some(RateLimits::from(statusline));
                }

                let tokens = metrics
                    .totals_for(&id)
                    .iter()
                    .map(|(_, totals)| totals.total())
                    .sum();

                Some(ClaudeSession {
                    name: session.name.clone().unwrap_or_else(|| id.clone()),
                    cwd: session.cwd.clone().unwrap_or_default(),
                    status: session.status.clone().unwrap_or_default(),
                    tmux_pane: session.tmux_pane().unwrap_or_default().to_owned(),
                    // Prefer the raw model id over the display name -- it is what the
                    // family table is built to fold.
                    model: statusline
                        .as_ref()
                        .and_then(|s| s.model_id().map(ModelFamily::from_id)),
                    tokens,
                    agents: metrics.subagent_messages(&id),
                    cost_usd: statusline.as_ref().map(|s| s.cost.total_cost_usd),
                    context_percent: statusline
                        .as_ref()
                        .map(|s| s.context_window.used_percentage),
                    last_turn_ms: metrics.last_turn_duration_ms(&id),
                    id,
                })
            })
            .collect();

        Some(ClaudeData {
            sessions,
            totals: metrics.totals_by_model(),
            rate_limits,
            history: snapshot.history,
            history_range: snapshot.range,
            history_families: snapshot.families,
            history_progress: snapshot.progress,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_internal_grid_can_serve_every_range() {
        // Aggregation rolls the stored grid *up*. A range asking for a bucket finer than
        // the grid does not get finer data -- it gets the same data spread over slots that
        // alternate empty, which draws as a comb rather than a staircase.
        for range in StatsRange::ALL {
            assert!(
                range.bucket() >= HISTORY_BUCKET,
                "{range} wants {:?} buckets from a {HISTORY_BUCKET:?} grid",
                range.bucket()
            );

            assert_eq!(
                range.bucket().as_secs() % HISTORY_BUCKET.as_secs(),
                0,
                "{range}'s bucket must be a whole number of grid steps, or its slots \
                 straddle stored buckets unevenly"
            );
        }
    }

    #[test]
    fn the_retained_window_covers_every_range() {
        // A range reaching further back than the history retains would draw dead space
        // before the oldest bucket and read as an idle stretch that never happened.
        for range in StatsRange::ALL {
            assert!(
                range.window() <= history_window(),
                "{range} reaches too far back"
            );
        }
    }
}
