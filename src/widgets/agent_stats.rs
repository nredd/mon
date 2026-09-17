//! Code around an agent token-history stats widget.

use std::time::Instant;

use crate::{
    collection::agent::AgentSource,
    components::time_series::{AutoYAxisTimeGraph, TimeseriesConfig},
};

/// A stacked history of token spend, over a rolling window, grouped by whichever series
/// set its `source` resolves to.
///
/// The series themselves are not held here. `harness-metrics` reconstructs its rolling
/// window from the transcripts on disk rather than accumulating it live, so the history is
/// already complete the moment this widget first appears, and a second copy kept here could
/// only ever drift from it. What this does own is the graph's own state: the display window,
/// the autohide timer, and the pixel-path image cache.
pub struct AgentStatsWidgetState {
    /// The underlying time-series graph, which carries the image cache across frames.
    pub graph: AutoYAxisTimeGraph,
    /// Whether the y-axis uses a logarithmic scale.
    ///
    /// Off by default, unlike the token-rate graph. A bucketed total spans a far narrower
    /// range than an instantaneous rate -- a busy minute and a quiet one differ by a factor
    /// of ten, not by five orders of magnitude -- and stacked bands only add up to the
    /// total on a linear axis.
    pub use_log: bool,
    /// Which harness (or harnesses) this widget instance reads.
    pub source: AgentSource,
    /// The fixed, ordered set of series labels this widget draws, resolved once from
    /// `source` at construction time rather than recomputed every frame.
    pub family_labels: Vec<&'static str>,
}

impl AgentStatsWidgetState {
    pub fn new(
        config: TimeseriesConfig, autohide_timer: Option<Instant>, use_log: bool,
        source: AgentSource,
    ) -> Self {
        let family_labels = source.family_labels();
        AgentStatsWidgetState {
            graph: AutoYAxisTimeGraph::new(config, autohide_timer),
            use_log,
            source,
            family_labels,
        }
    }
}
