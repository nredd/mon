use serde::{Deserialize, Serialize};

use super::ColourStr;

/// Styling specific to the Claude token-rate graph widget.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct ClaudeStyle {
    /// Colour of each model family's graph line. Read in family order: Opus, Sonnet, Haiku,
    /// Fable, then Other.
    #[serde(alias = "colours")]
    pub(crate) colours: Option<Vec<ColourStr>>,
}
