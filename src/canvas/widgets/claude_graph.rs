use std::borrow::Cow;

use claude_metrics::ModelFamily;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
};

use crate::{
    app::App,
    canvas::{
        Painter,
        components::time_series::{AxisBound, ChartScaling, GraphData, LegendConstraints},
        drawing_utils::should_hide_x_label,
    },
    components::time_series::GraphDrawCtx,
};

/// Model families in a fixed draw order.
///
/// The order is also the index into the theme's colour list. Fixed rather than sorted or
/// rank-ordered on purpose: a family that goes quiet and drops out must not repaint the
/// ones that remain.
const FAMILIES: [ModelFamily; 5] = [
    ModelFamily::Opus,
    ModelFamily::Sonnet,
    ModelFamily::Haiku,
    ModelFamily::Fable,
    ModelFamily::Other,
];

impl Painter {
    pub fn draw_claude_graph(
        &self, f: &mut Frame<'_>, app_state: &mut App, draw_loc: Rect, widget_id: u64,
    ) {
        if let Some(widget_state) = app_state
            .states
            .claude_graph_state
            .get_mut_widget_state(widget_id)
        {
            let shared_data = app_state.data_store.get_data();
            let token_data = &shared_data.time_series_data.claude_tokens;
            let times = &shared_data.time_series_data.time;

            let border_style = self.get_border_style(widget_id, app_state.current_widget.widget_id);
            let graph_state = widget_state.graph.state_mut();
            let hide_x_labels = should_hide_x_label(
                app_state.app_config_fields.hide_time,
                app_state.app_config_fields.autohide_time,
                graph_state.autohide_timer_mut(),
                draw_loc,
            );

            let use_log = widget_state.use_log;

            // Only families that have actually produced a series. A family nobody has used
            // would otherwise take a legend slot to say nothing.
            let present: Vec<ModelFamily> = FAMILIES
                .into_iter()
                .filter(|family| token_data.contains_key(family.label()))
                .collect();

            let visible = present
                .iter()
                .filter_map(|family| token_data.get(family.label()));
            let y_max = widget_state.graph.y_max(visible, times);

            let (adjusted_y_max, y_labels) = if use_log {
                adjust_tokens_log(y_max)
            } else {
                adjust_tokens_linear(y_max)
            };

            let colours = &self.styles.claude_colour_styles;

            let graph_data: Vec<GraphData<'_, f64>> = present
                .iter()
                .filter_map(|family| {
                    let values = token_data.get(family.label())?;

                    // Index by the family's fixed position, not by its position among the
                    // present ones, so colours stay put as families come and go.
                    let index = FAMILIES.iter().position(|f| f == family).unwrap_or(0);
                    let style = if colours.is_empty() {
                        Style::default()
                    } else {
                        colours[index % colours.len()]
                    };

                    let rate = values.last().copied().unwrap_or(0.0);

                    Some(
                        GraphData::default()
                            .name(format!("{:<7}{}", family.label(), format_rate(rate)).into())
                            .style(style)
                            .time(times)
                            .values(values),
                    )
                })
                .collect();

            let legend_constraints = LegendConstraints {
                width: Constraint::Ratio(3, 4),
                height: Constraint::Ratio(1, 2),
            };

            let marker = self.get_marker(app_state.app_config_fields.use_dot);
            let y_labels: Vec<Cow<'_, str>> = y_labels.into_iter().map(Into::into).collect();
            let scaling = if use_log {
                ChartScaling::Log2
            } else {
                ChartScaling::Linear
            };

            widget_state.graph.draw(
                f,
                draw_loc,
                GraphDrawCtx {
                    title: " Claude Tokens ".into(),
                    border_style,
                    title_style: self.styles.widget_title_style,
                    graph_style: self.styles.graph_style,
                    general_widget_style: self.styles.general_widget_style,
                    border_type: self.styles.border_type,
                    marker,
                    hide_x_labels,
                    is_selected: app_state.current_widget.widget_id == widget_id,
                    is_expanded: app_state.is_expanded,
                    legend_position: app_state.app_config_fields.claude_legend_position,
                    legend_constraints: Some(legend_constraints),
                },
                AxisBound::Max(adjusted_y_max),
                &y_labels,
                scaling,
                graph_data,
            );
        }

        if app_state.should_get_widget_bounds()
            && let Some(widget) = app_state.widget_map.get_mut(&widget_id)
        {
            widget.top_left_corner = Some((draw_loc.x, draw_loc.y));
            widget.bottom_right_corner =
                Some((draw_loc.x + draw_loc.width, draw_loc.y + draw_loc.height));
        }
    }
}

/// Render a tokens-per-second rate compactly and at a fixed width, so the legend box does
/// not resize between frames.
fn format_rate(tokens_per_sec: f64) -> String {
    let rendered = if tokens_per_sec >= 1_000_000.0 {
        format!("{:.1}M/s", tokens_per_sec / 1_000_000.0)
    } else if tokens_per_sec >= 1_000.0 {
        format!("{:.1}k/s", tokens_per_sec / 1_000.0)
    } else {
        format!("{tokens_per_sec:.0}/s")
    };

    format!("{rendered:>9}")
}

fn adjust_tokens_linear(max_entry: f64) -> (f64, Vec<String>) {
    // A flat-zero window would otherwise collapse the axis onto itself.
    let ceiling = if max_entry <= 0.0 {
        1.0
    } else {
        max_entry * 1.25
    };

    let labels = [0.0, 0.5, 1.0]
        .into_iter()
        .map(|fraction| format!("{:>9}", format_rate(ceiling * fraction).trim()))
        .collect();

    (ceiling, labels)
}

/// Token rates span several orders of magnitude in one session -- cache reads run into the
/// millions per second while fresh input tokens are single digits -- so the log axis is the
/// one that shows all of them at once.
fn adjust_tokens_log(max_entry: f64) -> (f64, Vec<String>) {
    use crate::utils::general::saturating_log2;

    let log_max = saturating_log2(max_entry);

    // 2^10 ~ 1k, 2^20 ~ 1M, 2^30 ~ 1G.
    if log_max < 10.0 {
        (10.0, vec!["      0/s".into(), "     1k/s".into()])
    } else if log_max < 20.0 {
        (
            20.0,
            vec!["      0/s".into(), "     1k/s".into(), "     1M/s".into()],
        )
    } else {
        (
            30.0,
            vec![
                "      0/s".into(),
                "     1k/s".into(),
                "     1M/s".into(),
                "     1G/s".into(),
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_render_at_a_stable_width() {
        // A legend box that resizes every frame is worse than one that wastes a column.
        for rate in [0.0, 9.0, 999.0, 1_234.0, 5_000_000.0] {
            assert_eq!(
                format_rate(rate).len(),
                9,
                "rate {rate} rendered at the wrong width"
            );
        }
    }

    #[test]
    fn rates_are_abbreviated_by_magnitude() {
        assert_eq!(format_rate(999.0).trim(), "999/s");
        assert_eq!(format_rate(1_234.0).trim(), "1.2k/s");
        assert_eq!(format_rate(5_000_000.0).trim(), "5.0M/s");
    }

    #[test]
    fn a_flat_zero_window_still_gets_a_usable_axis() {
        let (ceiling, labels) = adjust_tokens_linear(0.0);
        assert!(ceiling > 0.0, "a zero ceiling would collapse the y-axis");
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn families_keep_a_stable_colour_index() {
        // The whole point of indexing by family rather than by draw position: a family
        // going quiet must not recolour the ones still on screen.
        assert_eq!(
            FAMILIES.iter().position(|f| *f == ModelFamily::Opus),
            Some(0)
        );
        assert_eq!(
            FAMILIES.iter().position(|f| *f == ModelFamily::Fable),
            Some(3)
        );
    }
}
