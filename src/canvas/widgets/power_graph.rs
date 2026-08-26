use std::borrow::Cow;

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
    widgets::PowerChannel,
};

impl Painter {
    pub fn draw_power_graph(
        &self, f: &mut Frame<'_>, app_state: &mut App, draw_loc: Rect, widget_id: u64,
    ) {
        if let Some(widget_state) = app_state
            .states
            .power_graph_state
            .get_mut_widget_state(widget_id)
        {
            let shared_data = app_state.data_store.get_data();
            let power_data = &shared_data.time_series_data.power;
            let reported = &shared_data.time_series_data.power_reported;
            let times = &shared_data.time_series_data.time;

            let border_style = self.get_border_style(widget_id, app_state.current_widget.widget_id);
            let graph_state = widget_state.graph.state_mut();
            let hide_x_labels = should_hide_x_label(
                app_state.app_config_fields.hide_time,
                app_state.app_config_fields.autohide_time,
                graph_state.autohide_timer_mut(),
                draw_loc,
            );

            // Drop channels this machine never wires up. Without this an M4 draws three
            // flat lines pinned to zero for CPU, ANE, and RAM, which reads as "idle"
            // rather than "not measured".
            let hide_unreported = widget_state.hide_unreported;
            let channels: Vec<PowerChannel> = widget_state
                .shown
                .iter()
                .copied()
                .filter(|channel| !hide_unreported || reported.contains(channel.label()))
                .collect();

            let visible = channels
                .iter()
                .filter_map(|channel| power_data.get(channel.label()));
            let y_max = widget_state.graph.y_max(visible, times);
            let (adjusted_y_max, y_labels) = adjust_power(y_max);

            let colours = &self.styles.power_colour_styles;

            let graph_data: Vec<GraphData<'_, f64>> = channels
                .iter()
                .filter_map(|channel| {
                    let values = power_data.get(channel.label())?;

                    // Index by the channel's fixed position, not by its position among the
                    // visible ones, so a channel keeps its colour when others drop out.
                    let style = colours
                        .is_empty()
                        .then(Style::default)
                        .unwrap_or_else(|| colours[channel_index(*channel) % colours.len()]);

                    let latest = values.last().copied().unwrap_or(0.0);

                    Some(
                        GraphData::default()
                            .name(format!("{:<6} {latest:>6.2}W", channel.label()).into())
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

            let marker = self.get_marker(app_state.app_config_fields.marker);
            let y_labels: Vec<Cow<'_, str>> = y_labels.into_iter().map(Into::into).collect();

            widget_state.graph.draw(
                f,
                draw_loc,
                GraphDrawCtx {
                    title: " Power ".into(),
                    border_style,
                    title_style: self.styles.widget_title_style,
                    graph_style: self.styles.graph_style,
                    general_widget_style: self.styles.general_widget_style,
                    border_type: self.styles.border_type,
                    marker,
                    hide_x_labels,
                    is_selected: app_state.current_widget.widget_id == widget_id,
                    is_expanded: app_state.is_expanded,
                    legend_position: app_state.app_config_fields.power_legend_position,
                    legend_constraints: Some(legend_constraints),
                    x_labels: None,
                    footer_rows: 0,
                    pixel_renderer: self.pixel_renderer(),
                    last_time: times.last().copied(),
                    style_epoch: self.style_epoch(),
                },
                AxisBound::Max(adjusted_y_max),
                &y_labels,
                ChartScaling::Linear,
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

/// A channel's fixed index, used to pin it to one colour in the theme's list.
fn channel_index(channel: PowerChannel) -> usize {
    PowerChannel::ALL
        .iter()
        .position(|c| *c == channel)
        .unwrap_or(0)
}

/// Pick a y-axis ceiling and its labels for a Watt-valued chart.
///
/// Headroom is 25% rather than the 50% the byte-rate charts use: power on a laptop sits in
/// a narrow band, and half the chart being empty wastes the little vertical space a graph
/// widget usually gets.
fn adjust_power(max_entry: f64) -> (f64, Vec<String>) {
    // A flat-zero window would otherwise collapse the axis onto itself.
    let ceiling = if max_entry <= 0.0 {
        1.0
    } else {
        max_entry * 1.25
    };

    let labels = [0.0, 0.5, 1.0]
        .into_iter()
        .map(|fraction| format!("{:>7}", format!("{:.1}W", ceiling * fraction)))
        .collect();

    (ceiling, labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_zero_window_still_gets_a_usable_axis() {
        let (ceiling, labels) = adjust_power(0.0);
        assert!(ceiling > 0.0, "a zero ceiling would collapse the y-axis");
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn axis_leaves_headroom_above_the_peak() {
        let (ceiling, _) = adjust_power(20.0);
        assert!(
            ceiling > 20.0,
            "the peak must not touch the top of the chart"
        );
    }

    #[test]
    fn channels_keep_a_stable_colour_index() {
        // The whole point of indexing by channel rather than by draw position: dropping
        // an unreported channel must not recolour the ones that remain.
        assert_eq!(channel_index(PowerChannel::System), 0);
        assert_eq!(channel_index(PowerChannel::Gpu), 2);
        assert_eq!(channel_index(PowerChannel::Ram), 4);
    }
}
