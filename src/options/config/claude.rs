use serde::Deserialize;

/// Claude widget configuration.
///
/// Covers the sessions table (`claude`), the token-rate graph (`claude_graph`), and the
/// token-history stats graph (`claude_stats`).
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct ClaudeConfig {
    /// Whether the token-rate graph uses a logarithmic y-axis. Defaults to false.
    ///
    /// Off by default. The graph exists to compare families against each other over time,
    /// and on a log axis a band twice as tall is not twice the tokens. Turn it on for a
    /// window where one family genuinely dwarfs the rest badly enough that the others sit
    /// flat on the floor.
    pub(crate) use_log: Option<bool>,

    /// Whether the token-history stats graph uses a logarithmic y-axis. Defaults to false.
    pub(crate) stats_use_log: Option<bool>,

    /// How far back the stats graph reaches: one of `30m`, `2h`, `8h`, `24h`, `7d`, `30d`.
    ///
    /// Defaults to `2h`. Cycled at runtime with `T`.
    pub(crate) stats_range: Option<String>,

    /// Where to position the graph legend within the widget.
    ///
    /// Only the sessions table reads this now -- both graphs draw an inline legend below
    /// the plot instead of a box floating inside it.
    pub(crate) legend_position: Option<String>,
}
