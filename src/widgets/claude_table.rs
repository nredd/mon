//! Code around a Claude sessions table widget.

use std::{borrow::Cow, cmp::max, num::NonZeroU16};

use serde::Deserialize;

use crate::{
    app::AppConfigFields,
    canvas::components::data_table::{
        ColumnHeader, DataTableColumn, DataTableProps, DataTableStyling, DataToCell, SortColumn,
        SortDataTable, SortDataTableProps, SortsRow,
    },
    collection::claude::ClaudeSession,
    options::config::style::Styles,
    utils::general::sort_partial_fn,
};

/// The columns a Claude sessions table can show.
#[derive(Debug, Clone)]
#[cfg_attr(
    feature = "generate_schema",
    derive(schemars::JsonSchema, strum::VariantArray)
)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum ClaudeWidgetColumn {
    Session,
    Directory,
    Model,
    Status,
    Tokens,
    Cost,
    Context,
    Agents,
}

impl<'de> Deserialize<'de> for ClaudeWidgetColumn {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?.to_lowercase();
        match value.as_str() {
            "session" | "name" => Ok(ClaudeWidgetColumn::Session),
            "dir" | "directory" | "cwd" => Ok(ClaudeWidgetColumn::Directory),
            "model" => Ok(ClaudeWidgetColumn::Model),
            "status" => Ok(ClaudeWidgetColumn::Status),
            "tokens" => Ok(ClaudeWidgetColumn::Tokens),
            "cost" => Ok(ClaudeWidgetColumn::Cost),
            "context" | "ctx" => Ok(ClaudeWidgetColumn::Context),
            "agents" => Ok(ClaudeWidgetColumn::Agents),
            _ => Err(serde::de::Error::custom(
                "doesn't match any Claude column name",
            )),
        }
    }
}

impl ClaudeWidgetColumn {
    /// An ugly hack to generate the JSON schema.
    #[cfg(feature = "generate_schema")]
    pub fn get_schema_names(&self) -> &[&'static str] {
        match self {
            ClaudeWidgetColumn::Session => &["Session", "Name"],
            ClaudeWidgetColumn::Directory => &["Dir", "Directory", "Cwd"],
            ClaudeWidgetColumn::Model => &["Model"],
            ClaudeWidgetColumn::Status => &["Status"],
            ClaudeWidgetColumn::Tokens => &["Tokens"],
            ClaudeWidgetColumn::Cost => &["Cost"],
            ClaudeWidgetColumn::Context => &["Ctx", "Context"],
            ClaudeWidgetColumn::Agents => &["Agents"],
        }
    }
}

impl ColumnHeader for ClaudeWidgetColumn {
    fn text(&self) -> Cow<'static, str> {
        match self {
            ClaudeWidgetColumn::Session => "Session(s)".into(),
            ClaudeWidgetColumn::Directory => "Dir(d)".into(),
            ClaudeWidgetColumn::Model => "Model(m)".into(),
            ClaudeWidgetColumn::Status => "State".into(),
            ClaudeWidgetColumn::Tokens => "Tokens(t)".into(),
            ClaudeWidgetColumn::Cost => "Cost(c)".into(),
            ClaudeWidgetColumn::Context => "Ctx".into(),
            ClaudeWidgetColumn::Agents => "Agents(a)".into(),
        }
    }
}

/// Render a token count as `1.2M` / `12.3k` / `123`.
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Show the last path component rather than the whole path.
///
/// A session table is mostly a list of repositories, and the leading
/// `/Users/<someone>/code/` on every row is noise that pushes the useful part off the
/// right-hand edge.
fn short_dir(cwd: &str) -> &str {
    // Trim first: a trailing slash would otherwise make the last component empty.
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return cwd;
    }

    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

impl DataToCell<ClaudeWidgetColumn> for ClaudeSession {
    fn to_cell_text(
        &self, column: &ClaudeWidgetColumn, _calculated_width: NonZeroU16,
    ) -> Option<Cow<'static, str>> {
        Some(match column {
            ClaudeWidgetColumn::Session => self.name.clone().into(),
            ClaudeWidgetColumn::Directory => short_dir(&self.cwd).to_owned().into(),
            ClaudeWidgetColumn::Model => match self.model {
                Some(family) => family.label().into(),
                None => "N/A".into(),
            },
            ClaudeWidgetColumn::Status => self.status.clone().into(),
            ClaudeWidgetColumn::Tokens => format_tokens(self.tokens).into(),
            ClaudeWidgetColumn::Cost => match self.cost_usd {
                Some(cost) => format!("${cost:.2}").into(),
                None => "N/A".into(),
            },
            ClaudeWidgetColumn::Context => match self.context_percent {
                Some(percent) => format!("{percent:.0}%").into(),
                None => "N/A".into(),
            },
            ClaudeWidgetColumn::Agents => self.agents.to_string().into(),
        })
    }

    fn column_widths<C: DataTableColumn<ClaudeWidgetColumn>>(
        data: &[ClaudeSession], _columns: &[C],
    ) -> Vec<u16>
    where
        Self: Sized,
    {
        let mut widths = vec![0; 8];

        for row in data {
            widths[0] = max(widths[0], row.name.len() as u16);
            widths[1] = max(widths[1], short_dir(&row.cwd).len() as u16);
            widths[2] = max(widths[2], row.model.map_or(3, |f| f.label().len() as u16));
            widths[3] = max(widths[3], row.status.len() as u16);
            widths[4] = max(widths[4], format_tokens(row.tokens).len() as u16);
            widths[5] = max(widths[5], 8);
            widths[6] = max(widths[6], 4);
            widths[7] = max(widths[7], row.agents.to_string().len() as u16);
        }

        widths
    }
}

impl SortsRow for ClaudeWidgetColumn {
    type DataType = ClaudeSession;

    fn sort_data(&self, data: &mut [Self::DataType], descending: bool) {
        match self {
            ClaudeWidgetColumn::Session => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.name, &b.name));
            }
            ClaudeWidgetColumn::Directory => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.cwd, &b.cwd));
            }
            ClaudeWidgetColumn::Model => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.model, &b.model));
            }
            ClaudeWidgetColumn::Status => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.status, &b.status));
            }
            ClaudeWidgetColumn::Tokens => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.tokens, &b.tokens));
            }
            ClaudeWidgetColumn::Cost => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.cost_usd, &b.cost_usd));
            }
            ClaudeWidgetColumn::Context => {
                data.sort_by(move |a, b| {
                    sort_partial_fn(descending)(&a.context_percent, &b.context_percent)
                });
            }
            ClaudeWidgetColumn::Agents => {
                data.sort_by(move |a, b| sort_partial_fn(descending)(&a.agents, &b.agents));
            }
        }
    }
}

/// Runtime state for a Claude sessions table widget.
pub struct ClaudeWidgetState {
    pub table: SortDataTable<ClaudeSession, ClaudeWidgetColumn>,
    pub force_update_data: bool,
}

impl ClaudeWidgetState {
    pub(crate) fn new(config: &AppConfigFields, palette: &Styles) -> Self {
        let columns = [
            SortColumn::soft(ClaudeWidgetColumn::Session, Some(0.24)),
            SortColumn::soft(ClaudeWidgetColumn::Directory, Some(0.20)),
            SortColumn::soft(ClaudeWidgetColumn::Model, Some(0.10)),
            SortColumn::soft(ClaudeWidgetColumn::Status, Some(0.10)),
            SortColumn::soft(ClaudeWidgetColumn::Tokens, None).default_descending(),
            SortColumn::soft(ClaudeWidgetColumn::Cost, None).default_descending(),
            SortColumn::soft(ClaudeWidgetColumn::Context, None).default_descending(),
            SortColumn::soft(ClaudeWidgetColumn::Agents, None).default_descending(),
        ];

        let props = SortDataTableProps {
            inner: DataTableProps {
                title: Some(" Claude Sessions ".into()),
                table_gap: config.table_gap,
                left_to_right: false,
                is_basic: config.use_basic_mode,
                show_table_scroll_position: config.show_table_scroll_position,
                show_table_scroll_bar: config.show_table_scroll_bar,
                show_current_entry_when_unfocused: false,
            },
            // Tokens: the column that actually distinguishes one session from another.
            sort_index: 4,
            order: config.default_disk_sort_order,
        };

        let styling = DataTableStyling::from_palette(palette);

        Self {
            table: SortDataTable::new_sortable(columns, props, styling),
            force_update_data: false,
        }
    }

    /// Forces an update of the data stored.
    #[inline]
    pub fn force_data_update(&mut self) {
        self.force_update_data = true;
    }

    /// Update the current table data.
    pub fn set_table_data(&mut self, data: &[ClaudeSession]) {
        let mut data = data.to_vec();
        if let Some(column) = self.table.columns.get(self.table.sort_index()) {
            column.sort_by(&mut data, self.table.order());
        }
        self.table.set_data(data);
        self.force_update_data = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_are_abbreviated() {
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(12_345), "12.3k");
        assert_eq!(format_tokens(51_722_228), "51.7M");
    }

    #[test]
    fn only_the_last_path_component_is_shown() {
        // Every row would otherwise start with the same `/Users/<someone>/code/` prefix,
        // pushing the part that distinguishes them off the edge.
        assert_eq!(short_dir("/Users/redd/code/mon"), "mon");
        assert_eq!(short_dir("/Users/redd/code/"), "code");
        assert_eq!(short_dir("mon"), "mon");
        assert_eq!(short_dir(""), "");
        assert_eq!(short_dir("/"), "/", "root has no last component to show");
    }
}
