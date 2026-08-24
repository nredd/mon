//! Code around a Claude token-rate graph widget.

use std::time::Instant;

use crate::components::time_series::{AutoYAxisTimeGraph, TimeseriesConfig};

/// A time series graph widget displaying Claude token throughput by model family.
pub struct ClaudeGraphWidgetState {
    /// The underlying time-series graph with automatic y-axis scaling.
    pub graph: AutoYAxisTimeGraph,
    /// Whether the y-axis uses a logarithmic scale.
    ///
    /// Worth having: cache reads run four to five orders of magnitude above fresh input
    /// tokens, so on a linear axis the smaller series sit flat on the floor.
    pub use_log: bool,
}

impl ClaudeGraphWidgetState {
    pub fn new(config: TimeseriesConfig, autohide_timer: Option<Instant>, use_log: bool) -> Self {
        ClaudeGraphWidgetState {
            graph: AutoYAxisTimeGraph::new(config, autohide_timer),
            use_log,
        }
    }
}
