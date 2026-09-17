//! Code around an agent token-rate graph widget.

use std::time::Instant;

use crate::{
    collection::agent::AgentSource,
    components::time_series::{AutoYAxisTimeGraph, TimeseriesConfig},
};

/// A time series graph widget displaying coding-agent token throughput, grouped by
/// whichever series set its `source` resolves to.
pub struct AgentGraphWidgetState {
    /// The underlying time-series graph with automatic y-axis scaling.
    pub graph: AutoYAxisTimeGraph,
    /// Whether the y-axis uses a logarithmic scale.
    ///
    /// Worth having: cache reads run four to five orders of magnitude above fresh input
    /// tokens, so on a linear axis the smaller series sit flat on the floor.
    pub use_log: bool,
    /// Which harness (or harnesses) this widget instance reads.
    pub source: AgentSource,
    /// The fixed, ordered set of series labels this widget draws, resolved once from
    /// `source` at construction time rather than recomputed every frame.
    pub family_labels: Vec<&'static str>,
}

impl AgentGraphWidgetState {
    pub fn new(
        config: TimeseriesConfig, autohide_timer: Option<Instant>, use_log: bool,
        source: AgentSource,
    ) -> Self {
        let family_labels = source.family_labels();
        AgentGraphWidgetState {
            graph: AutoYAxisTimeGraph::new(config, autohide_timer),
            use_log,
            source,
            family_labels,
        }
    }
}
