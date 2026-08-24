use serde::Deserialize;

/// Claude widget configuration.
///
/// Covers both the sessions table (`claude`) and the token-rate graph (`claude_graph`).
#[derive(Clone, Debug, Default, Deserialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct ClaudeConfig {
    /// Whether the token-rate graph uses a logarithmic y-axis. Defaults to true.
    ///
    /// On by default because the series genuinely span orders of magnitude: cache reads run
    /// into the millions of tokens per second while fresh input tokens are single digits,
    /// and on a linear axis everything but the largest series sits flat on the floor.
    pub(crate) use_log: Option<bool>,

    /// Where to position the graph legend within the widget.
    pub(crate) legend_position: Option<String>,
}
