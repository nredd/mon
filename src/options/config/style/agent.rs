use serde::{Deserialize, Serialize};

use super::ColourStr;

/// Styling specific to the agent token-rate and token-history graph widgets.
///
/// The `colours` list is indexed by whichever grouping is active for a given widget
/// instance: model-family labels (e.g. Opus, Sonnet, Haiku, Fable, Other) when that
/// widget's `source` names a single harness, or harness labels (`Claude`, `Codex`, `Pi`)
/// when `source = "all"`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct AgentStyle {
    /// Colour of each series' graph line, indexed by draw order (see the struct docs for
    /// what "draw order" means for a given widget's `source`).
    #[serde(alias = "colours")]
    pub(crate) colours: Option<Vec<ColourStr>>,
}
