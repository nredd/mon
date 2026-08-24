use serde::{Deserialize, Serialize};

use super::StringOrNum;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TableGap {
    None,
    #[default]
    Space,
    Line,
}

impl TableGap {
    /// Returns the height in rows that this gap occupies.
    pub const fn height(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Space | Self::Line => 1,
        }
    }
}

/// Which glyph family a graph is plotted with.
///
/// Every one of these is already implemented by the vendored chart renderer -- they were
/// simply unreachable, because the marker was picked from a hardcoded braille/dot pair.
/// This resolves upstream's own `TODO` in `options/args.rs`.
///
/// Resolution goes from finest to coarsest: braille packs 2x4 dots into a cell, octant and
/// sextant 2x4 and 2x3 blocks, quadrant 2x2, half-block 1x2, and dot/block/bar one glyph
/// per cell. Fonts vary in what they actually render, which is the whole reason this is
/// configurable.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GraphMarker {
    /// 2x4 braille dots per cell. The finest available, and the default.
    #[default]
    Braille,
    /// A single `•` per cell.
    Dot,
    /// A solid block per cell, coloured on the background.
    Block,
    /// A `▄` per cell.
    Bar,
    /// 1x2 half blocks per cell.
    HalfBlock,
    /// 2x2 quadrant blocks per cell.
    Quadrant,
    /// 2x3 sextant blocks per cell.
    Sextant,
    /// 2x4 octant blocks per cell.
    Octant,
}

impl GraphMarker {
    /// Every marker name, for help text and error messages.
    pub const NAMES: [&'static str; 8] = [
        "braille",
        "dot",
        "block",
        "bar",
        "half_block",
        "quadrant",
        "sextant",
        "octant",
    ];
}

impl std::str::FromStr for GraphMarker {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept both spellings of the two-word names, since the CLI conventionally uses
        // dashes and the config file conventionally uses underscores.
        match s.to_lowercase().replace('-', "_").as_str() {
            "braille" => Ok(GraphMarker::Braille),
            "dot" => Ok(GraphMarker::Dot),
            "block" => Ok(GraphMarker::Block),
            "bar" => Ok(GraphMarker::Bar),
            "half_block" => Ok(GraphMarker::HalfBlock),
            "quadrant" => Ok(GraphMarker::Quadrant),
            "sextant" => Ok(GraphMarker::Sextant),
            "octant" => Ok(GraphMarker::Octant),
            _ => Err(format!(
                "'{s}' is not a valid marker. Expected one of: {}",
                GraphMarker::NAMES.join(", ")
            )),
        }
    }
}

// TODO: Break this up.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[cfg_attr(test, serde(deny_unknown_fields), derive(PartialEq, Eq))]
pub(crate) struct GeneralConfig {
    /// Deprecated in favour of `marker`. Kept so old configs keep working.
    pub(crate) dot_marker: Option<bool>,
    /// Which glyph family graphs are plotted with.
    pub(crate) marker: Option<GraphMarker>,
    pub(crate) rate: Option<StringOrNum>,
    pub(crate) basic: Option<bool>,
    pub(crate) default_time_value: Option<StringOrNum>,
    pub(crate) time_delta: Option<StringOrNum>,
    pub(crate) autohide_time: Option<bool>,
    pub(crate) hide_time: Option<bool>,
    pub(crate) default_widget_type: Option<String>,
    pub(crate) default_widget_count: Option<u64>,
    pub(crate) expanded: Option<bool>,
    pub(crate) use_old_network_legend: Option<bool>,
    #[serde(default)]
    pub(crate) table_gap: TableGap,
    pub(crate) battery: Option<bool>,
    pub(crate) disable_click: Option<bool>,
    pub(crate) disable_keys: Option<bool>,
    pub(crate) no_write: Option<bool>,
    pub(crate) show_table_scroll_position: Option<bool>,
    pub(crate) show_table_scroll_bar: Option<bool>,
    pub(crate) read_only: Option<bool>,
    pub(crate) disable_gpu: Option<bool>,
    pub(crate) retention: Option<StringOrNum>,
    pub(crate) temperature_type: Option<String>,

    // FIXME: Deprecate these in the future.
    pub(crate) hide_avg_cpu: Option<bool>,
    pub(crate) cpu_left_legend: Option<bool>,
    pub(crate) average_cpu_row: Option<bool>,
    pub(crate) enable_cache_memory: Option<bool>,
    // #[cfg(feature = "zfs")]
    pub(crate) free_arc: Option<bool>,
    pub(crate) network_use_bytes: Option<bool>,
    pub(crate) network_use_log: Option<bool>,
    pub(crate) network_use_binary_prefix: Option<bool>,
    pub(crate) network_legend: Option<String>,
    pub(crate) memory_legend: Option<String>,
    // #[cfg(target_os = "linux")]
    pub(crate) hide_k_threads: Option<bool>,
    pub(crate) tree_collapse: Option<bool>,
    pub(crate) process_command: Option<bool>,
    // This does nothing on Windows, but we leave it enabled to make the config file consistent
    // across platforms.
    //
    // #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    pub(crate) disable_advanced_kill: Option<bool>,
    pub(crate) process_memory_as_value: Option<bool>,
    pub(crate) group_processes: Option<bool>,
    pub(crate) regex: Option<bool>,
    pub(crate) case_sensitive: Option<bool>,
    pub(crate) whole_word: Option<bool>,
    pub(crate) tree: Option<bool>,
    pub(crate) current_usage: Option<bool>,
    pub(crate) unnormalized_cpu: Option<bool>,
}

#[cfg(test)]
mod marker_tests {
    use super::GraphMarker;

    #[test]
    fn every_marker_name_round_trips() {
        // `NAMES` feeds the help text and the error message, so it drifting out of step
        // with `FromStr` would advertise markers that do not parse.
        for name in GraphMarker::NAMES {
            assert!(
                name.parse::<GraphMarker>().is_ok(),
                "'{name}' is advertised but does not parse"
            );
        }
    }

    #[test]
    fn dashes_and_underscores_are_both_accepted() {
        // The CLI conventionally spells it with a dash, the config file with an underscore.
        assert_eq!(
            "half-block".parse::<GraphMarker>().unwrap(),
            GraphMarker::HalfBlock
        );
        assert_eq!(
            "half_block".parse::<GraphMarker>().unwrap(),
            GraphMarker::HalfBlock
        );
        assert_eq!(
            "HALF_BLOCK".parse::<GraphMarker>().unwrap(),
            GraphMarker::HalfBlock
        );
    }

    #[test]
    fn an_unknown_marker_names_the_valid_ones() {
        let err = "spirograph"
            .parse::<GraphMarker>()
            .expect_err("an unknown marker must be an error");

        assert!(err.contains("spirograph"), "the error must quote the input");
        for name in GraphMarker::NAMES {
            assert!(err.contains(name), "the error must list '{name}'");
        }
    }

    #[test]
    fn braille_is_the_default() {
        assert_eq!(GraphMarker::default(), GraphMarker::Braille);
    }
}
