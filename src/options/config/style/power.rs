use serde::{Deserialize, Serialize};

use super::ColourStr;

/// Styling specific to the power graph widget.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct PowerStyle {
    /// Colour of each power channel's graph line. Read in channel order: system, CPU, GPU,
    /// ANE, then RAM.
    #[serde(alias = "colours")]
    pub(crate) colours: Option<Vec<ColourStr>>,
}
