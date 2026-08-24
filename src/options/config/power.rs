use serde::Deserialize;

/// Power graph configuration.
///
/// Every channel is opt-out rather than opt-in, but see `hide_unreported`: on most Apple
/// Silicon chips several of these channels are hardwired to zero, and drawing three flat
/// lines at the bottom of the chart is worse than drawing none.
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct PowerConfig {
    /// Whether to show whole-system power. Defaults to true.
    pub(crate) show_system: Option<bool>,

    /// Whether to show CPU package power. Defaults to true.
    pub(crate) show_cpu: Option<bool>,

    /// Whether to show GPU power. Defaults to true.
    pub(crate) show_gpu: Option<bool>,

    /// Whether to show Apple Neural Engine power. Defaults to true.
    pub(crate) show_ane: Option<bool>,

    /// Whether to show DRAM power. Defaults to false -- most chips do not report it.
    pub(crate) show_ram: Option<bool>,

    /// Whether to drop channels that have only ever reported zero. Defaults to true.
    ///
    /// Not every chip reports every channel. On an M4, `cpu`, `ane`, and `ram` sit at a
    /// constant `0.0` while only `system` and `gpu` carry real figures, so leaving this on
    /// keeps the chart to the series that mean something.
    pub(crate) hide_unreported: Option<bool>,

    /// Where to position the legend within the widget.
    pub(crate) legend_position: Option<String>,
}
