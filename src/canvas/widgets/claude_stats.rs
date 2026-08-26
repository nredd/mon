//! Drawing the Claude token-history stats graph.
//!
//! Claude Code's own `/status` -> Stats -> Models screen bars token spend by day. This is
//! the same graph over a selectable span, from half an hour up to thirty days, so the shape
//! of a working session is visible rather than collapsed into a single bar.
//!
//! Note that the totals here will read roughly *half* what `/status` shows, and this side
//! is the correct one. Claude Code writes one transcript record per content block --
//! thinking, text, tool use -- each repeating the same cumulative `usage`, and its own
//! rollup sums every record. `claude_metrics` counts a message's request fields once and
//! tracks its output as a high-water mark. See `docs/content/usage/widgets/claude.md`.

use ratatui::{Frame, layout::Rect};

use crate::{
    app::App,
    canvas::{
        Painter,
        components::time_series::{AxisBound, ChartScaling, GraphData},
        drawing_utils::should_hide_x_label,
        widgets::claude_series::{self, BucketSeries},
    },
    components::time_series::GraphDrawCtx,
};

/// Where the log axis stops tracking the data: `2^10` is about a thousand tokens. Below
/// that the window is effectively idle, and scaling to it magnifies noise to full height.
const LOG_FLOOR: f64 = 10.0;

impl Painter {
    pub fn draw_claude_stats(
        &self, f: &mut Frame<'_>, app_state: &mut App, draw_loc: Rect, widget_id: u64,
    ) {
        if let Some(widget_state) = app_state
            .states
            .claude_stats_state
            .get_mut_widget_state(widget_id)
        {
            let shared_data = app_state.data_store.get_data();
            let buckets = shared_data.claude_history.clone();
            let families = shared_data.claude_history_families.clone();

            // The range travels with the buckets rather than being read off the widget: a
            // range switch takes a tick to reach the collection thread and come back, and
            // labelling the old buckets with the new span would misreport them for a frame.
            let range = shared_data.claude_history_range;

            let border_style = self.get_border_style(widget_id, app_state.current_widget.widget_id);
            let graph_state = widget_state.graph.state_mut();
            let hide_x_labels = should_hide_x_label(
                app_state.app_config_fields.hide_time,
                app_state.app_config_fields.autohide_time,
                graph_state.autohide_timer_mut(),
                draw_loc,
            );

            let use_log = widget_state.use_log;
            let footer_rows = claude_series::footer_rows(draw_loc);
            let scan_note = shared_data
                .claude_history_progress
                .map(claude_series::scan_note);

            let series = BucketSeries::build(&buckets, &families, range.bucket(), 1.0);

            let (y_max, y_labels) = claude_series::axis(
                series.peak,
                use_log,
                LOG_FLOOR,
                claude_series::y_label_count(claude_series::plot_height(draw_loc, footer_rows)),
                "",
            );

            let x_labels = series.x_labels(
                claude_series::x_label_count(claude_series::plot_width(draw_loc)),
                range.bucket(),
                range.spans_days(),
            );

            let colours = &self.styles.claude_colour_styles;

            let graph_data: Vec<GraphData<'_, f64>> = series
                .bands
                .iter()
                .map(|band| {
                    let style = colours
                        .get(band.colour_index % colours.len().max(1))
                        .copied()
                        .unwrap_or_default();

                    GraphData::default()
                        .style(style)
                        .time(&series.times)
                        .values(&band.values)
                        // Overlaid outlines, not stacked fills. Each family is drawn from
                        // the baseline in its own colour, which is what the native screen
                        // does and what makes one family's own volume readable rather than
                        // only its share of a total.
                        .filled(false)
                        // A bucket is a total over its width, not a reading at an instant.
                        // Sloping between bucket centres would spread one busy minute over
                        // three and understate its peak; a staircase holds each bucket flat
                        // across the span it covers.
                        .stepped(true)
                })
                .collect();

            let marker = self.get_marker(app_state.app_config_fields.marker);
            let scaling = if use_log {
                ChartScaling::Log2
            } else {
                ChartScaling::Linear
            };

            widget_state.graph.draw(
                f,
                draw_loc,
                GraphDrawCtx {
                    title: " Claude Stats ".into(),
                    border_style,
                    title_style: self.styles.widget_title_style,
                    graph_style: self.styles.graph_style,
                    general_widget_style: self.styles.general_widget_style,
                    border_type: self.styles.border_type,
                    marker,
                    hide_x_labels,
                    is_selected: app_state.current_widget.widget_id == widget_id,
                    is_expanded: app_state.is_expanded,
                    // The legend lives in the footer below the plot, not in a box floating
                    // inside it.
                    legend_position: None,
                    legend_constraints: None,
                    x_labels: (!hide_x_labels).then_some(x_labels.as_slice()),
                    footer_rows,
                    pixel_renderer: self.pixel_renderer(),
                    last_time: series.times.last().copied(),
                    style_epoch: self.style_epoch(),
                },
                AxisBound::Max(y_max),
                &y_labels,
                scaling,
                graph_data,
            );

            claude_series::draw_footer(
                f,
                draw_loc,
                &series.bands,
                colours,
                Some(range),
                self.styles.widget_title_style,
                self.styles.graph_legend_style,
                scan_note.as_deref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_floor_is_a_thousand_tokens_not_a_thousand_per_second() {
        // The rate graph's floor is four (sixteen tokens a second); this one is ten,
        // because a bucket total spans a far narrower range than an instantaneous rate.
        assert!((LOG_FLOOR - 10.0).abs() < f64::EPSILON);
        assert_eq!(2f64.powf(LOG_FLOOR) as u64, 1024);
    }
}
