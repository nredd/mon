//! Code around a power graph widget.

use std::time::Instant;

use crate::{
    collection::power::PowerData,
    components::time_series::{AutoYAxisTimeGraph, TimeseriesConfig},
};

/// One power rail the graph can draw.
///
/// The declaration order here is the draw and legend order, and the index into the theme's
/// colour list. It is deliberately fixed rather than sorted, so a channel keeps its colour
/// as others come and go.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PowerChannel {
    /// Whole-system power draw.
    System,
    /// CPU package power.
    Cpu,
    /// GPU power.
    Gpu,
    /// Apple Neural Engine power.
    Ane,
    /// DRAM power.
    Ram,
}

impl PowerChannel {
    /// Every channel, in draw order.
    pub const ALL: [PowerChannel; 5] = [
        PowerChannel::System,
        PowerChannel::Cpu,
        PowerChannel::Gpu,
        PowerChannel::Ane,
        PowerChannel::Ram,
    ];

    /// The legend label, which doubles as the time-series key.
    pub fn label(self) -> &'static str {
        match self {
            PowerChannel::System => "System",
            PowerChannel::Cpu => "CPU",
            PowerChannel::Gpu => "GPU",
            PowerChannel::Ane => "ANE",
            PowerChannel::Ram => "RAM",
        }
    }

    /// Pull this channel's reading out of a sample, in Watts.
    ///
    /// `None` means the machine did not report it at all this interval, which is distinct
    /// from reporting a genuine zero.
    pub fn watts(self, data: &PowerData) -> Option<f32> {
        match self {
            PowerChannel::System => data.sys_power_w,
            PowerChannel::Cpu => Some(data.cpu_power_w),
            PowerChannel::Gpu => Some(data.gpu_power_w),
            PowerChannel::Ane => Some(data.ane_power_w),
            PowerChannel::Ram => Some(data.ram_power_w),
        }
    }
}

/// A time series graph widget displaying Apple Silicon power draw over time.
pub struct PowerGraphWidgetState {
    /// The underlying time-series graph with automatic y-axis scaling.
    pub graph: AutoYAxisTimeGraph,
    /// Which channels the config asked for, in [`PowerChannel::ALL`] order.
    pub shown: Vec<PowerChannel>,
    /// Whether to drop channels this machine has never reported a nonzero value for.
    pub hide_unreported: bool,
}

impl PowerGraphWidgetState {
    pub fn new(
        config: TimeseriesConfig, autohide_timer: Option<Instant>, shown: Vec<PowerChannel>,
        hide_unreported: bool,
    ) -> Self {
        PowerGraphWidgetState {
            graph: AutoYAxisTimeGraph::new(config, autohide_timer),
            shown,
            hide_unreported,
        }
    }
}
