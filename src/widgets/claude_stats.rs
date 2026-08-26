//! Code around a Claude token-history stats widget.

use std::time::Instant;

use claude_metrics::StatsRange;

use crate::components::time_series::{AutoYAxisTimeGraph, TimeseriesConfig};

/// A history of token spend by model family, over a selectable range.
///
/// The series themselves are not held here. `claude-metrics` reconstructs its rolling
/// window from the transcripts on disk rather than accumulating it live, so the history is
/// already complete the moment this widget first appears, and a second copy kept here could
/// only ever drift from it. What this does own is the graph's own state: the selected
/// range, the autohide timer, and the pixel-path image cache.
pub struct ClaudeStatsWidgetState {
    /// The underlying time-series graph, which carries the image cache across frames.
    pub graph: AutoYAxisTimeGraph,
    /// Whether the y-axis uses a logarithmic scale.
    ///
    /// Off by default. Bands are compared by height, and on a log axis a bar twice as tall
    /// is not twice the tokens.
    pub use_log: bool,
    /// The range being asked for.
    ///
    /// This is the *request*. What gets drawn is whatever range came back attached to the
    /// buckets, which lags by a tick after a change -- see
    /// `crate::app::data::store::InnerData::claude_history_range`.
    range: StatsRange,
    /// The configured range, which the reset-zoom key returns to.
    default_range: StatsRange,
}

impl ClaudeStatsWidgetState {
    pub fn new(
        config: TimeseriesConfig, autohide_timer: Option<Instant>, use_log: bool, range: StatsRange,
    ) -> Self {
        let mut state = ClaudeStatsWidgetState {
            graph: AutoYAxisTimeGraph::new(config, autohide_timer),
            use_log,
            range,
            default_range: range,
        };

        state.set_range(range);
        state
    }

    /// Ask for a different range, and pin the graph's x-axis to match.
    ///
    /// The two have to move together. The axis span comes from the graph's own display
    /// time while the points come from the collector's buckets, so leaving the graph on its
    /// old span would draw a day of buckets across a two-hour axis.
    pub fn set_range(&mut self, range: StatsRange) {
        self.range = range;

        let window_ms = u64::try_from(range.window().as_millis()).unwrap_or(u64::MAX);
        self.graph.state_mut().set_window(window_ms);
    }

    /// Move to the next range, wrapping.
    pub fn cycle_range(&mut self) -> StatsRange {
        self.set_range(self.range.next());
        self.range
    }

    /// Narrow the range, stopping at the shortest.
    pub fn shorten_range(&mut self) -> StatsRange {
        self.set_range(self.range.shorter());
        self.range
    }

    /// Widen the range, stopping at the widest.
    pub fn lengthen_range(&mut self) -> StatsRange {
        self.set_range(self.range.longer());
        self.range
    }

    /// Return to the configured range.
    pub fn reset_range(&mut self) -> StatsRange {
        self.set_range(self.default_range);
        self.range
    }

    /// Flip between the linear and logarithmic y-axis.
    pub fn toggle_log(&mut self) {
        self.use_log = !self.use_log;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: TimeseriesConfig = TimeseriesConfig {
        time_interval: 15_000,
        retention_ms: 300_000,
        autohide_time: false,
        default_time_value: 60_000,
    };

    fn state(range: StatsRange) -> ClaudeStatsWidgetState {
        ClaudeStatsWidgetState::new(CONFIG, None, false, range)
    }

    #[test]
    fn constructing_pins_the_axis_to_the_range_not_to_the_config_default() {
        // `TimeseriesConfig` carries the app-wide default window, which has nothing to do
        // with how far back this graph's buckets reach. Left unpinned, a `24h` range would
        // draw a day of buckets squashed into a one-minute axis.
        let state = state(StatsRange::OneDay);

        assert_eq!(
            state.graph.state().current_display_time(),
            24 * 60 * 60 * 1000
        );
    }

    #[test]
    fn changing_the_range_moves_the_axis_with_it() {
        let mut state = state(StatsRange::ThirtyMinutes);

        state.set_range(StatsRange::SevenDays);

        assert_eq!(
            state.graph.state().current_display_time(),
            7 * 24 * 60 * 60 * 1000
        );
    }

    #[test]
    fn resetting_returns_to_the_configured_range_not_the_shortest() {
        let mut state = state(StatsRange::EightHours);

        state.cycle_range();
        state.cycle_range();

        assert_eq!(state.reset_range(), StatsRange::EightHours);
    }

    #[test]
    fn zoom_clamps_at_both_ends() {
        let mut state = state(StatsRange::ThirtyMinutes);

        assert_eq!(state.shorten_range(), StatsRange::ThirtyMinutes);

        state.set_range(StatsRange::ThirtyDays);
        assert_eq!(state.lengthen_range(), StatsRange::ThirtyDays);
    }
}
