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

use std::time::Duration;

use claude_metrics::{
    Bucket, ClaudeMetrics, ModelFamily, StatsRange, Statusline, TokenHistory, TokenTotals,
};

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
    /// The same history at a short window and a fine grid, for the token-rate graph.
    pub rate_history: Vec<Bucket>,
    /// Families that contributed anything in the retained window, in a stable draw order.
    pub history_families: Vec<ModelFamily>,
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

/// How far back the retained history reaches.
///
/// This is the mtime cutoff [`TokenHistory`] filters transcripts by, so it is a direct
/// multiplier on the first refresh's cost. Measured on a real tree: a day reaches ~200
/// files / ~42MB, a week ~1200 files / ~352MB. Long-lived sessions keep old transcripts
/// freshly modified, so past about a week the filter stops filtering and the window is
/// effectively the whole corpus.
///
/// TODO(redd): widen this to [`StatsRange::widest`] once the history is persisted and the
/// cold build runs off the collection thread. Until then the two widest ranges are
/// selectable but only show what fits in here.
const HISTORY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// How finely that window is divided internally.
///
/// Deliberately finer than anything drawn. Every range rolls this grid up on demand via
/// [`TokenHistory::aggregate`], so the transcripts are parsed once no matter how many
/// views want them, and switching range costs an aggregation rather than a re-scan.
const HISTORY_BUCKET: Duration = Duration::from_secs(10);

/// How far back the token-rate graph reaches.
///
/// Fixed rather than selectable: this graph answers "what is being spent right now", and
/// the stats graph next to it already answers the same question over longer spans.
pub const RATE_WINDOW: Duration = Duration::from_secs(10 * 60);

/// The rate graph's grid, which is the finest the history holds.
pub const RATE_BUCKET: Duration = HISTORY_BUCKET;

/// Owns the reader and turns a refresh into a [`ClaudeData`] snapshot.
#[derive(Debug)]
pub struct ClaudeCollector {
    metrics: Option<ClaudeMetrics>,
    /// Built on first use, so a layout without the stats widget never walks the tree.
    history: Option<TokenHistory>,
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
    /// is on screen, and a layout without one should not pay for it.
    pub fn harvest(&mut self, range: Option<StatsRange>) -> Option<ClaudeData> {
        let metrics = self.metrics.as_mut()?;
        metrics.refresh();

        let (history, rate_history, history_families) = if let Some(range) = range {
            let root = metrics.root().to_path_buf();
            let history = self
                .history
                .get_or_insert_with(|| TokenHistory::new(root, HISTORY_WINDOW, HISTORY_BUCKET));

            history.refresh();

            (
                history.aggregate(range.window(), range.bucket()),
                history.aggregate(RATE_WINDOW, RATE_BUCKET),
                history.families(),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };

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
            history,
            history_range: range.unwrap_or_default(),
            rate_history,
            history_families,
        })
    }
}
