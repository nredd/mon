//! Drawing the Claude token-history stats graph.
//!
//! Claude Code's own `/status` -> Stats -> Models screen bars token spend by day. This is
//! the same graph over a selectable span, from half an hour up to thirty days, so the shape
//! of a working session is visible rather than collapsed into a single bar.
//!
//! Everything that this and the token-rate graph draw the same way lives in
//! [`crate::canvas::widgets::claude_series`]; what is here is what differs.
//!
//! Note that the totals will read roughly *half* what `/status` shows, and this side is the
//! correct one. Claude Code writes one transcript record per content block -- thinking,
//! text, tool use -- each repeating the same cumulative `usage`, and its own rollup sums
//! every record. `claude_metrics` counts a message's request fields once and tracks its
//! output as a high-water mark. See `docs/content/usage/widgets/claude.md`.

use ratatui::{Frame, layout::Rect};

use crate::{
    app::App,
    canvas::{
        Painter,
        drawing_utils::should_hide_x_label,
        widgets::claude_series::{self, BandFrame, BandSpec},
    },
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

            // The range travels with the buckets rather than being read off the widget: a
            // range switch takes a tick to reach the collection thread and come back, and
            // labelling the old buckets with the new span would misreport them for a frame.
            let range = shared_data.claude_history_range;

            let use_log = widget_state.use_log;
            let hide_x_labels = should_hide_x_label(
                app_state.app_config_fields.hide_time,
                app_state.app_config_fields.autohide_time,
                widget_state.graph.state_mut().autohide_timer_mut(),
                draw_loc,
            );

            let frame = BandFrame {
                hide_x_labels,
                is_selected: app_state.current_widget.widget_id == widget_id,
                is_expanded: app_state.is_expanded,
                marker: self.get_marker(app_state.app_config_fields.marker),
            };

            let spec = BandSpec {
                title: " Claude Stats ",
                buckets: &shared_data.claude_history,
                families: &shared_data.claude_history_families,
                bucket: range.bucket(),
                // Token counts, not a rate.
                divisor: 1.0,
                unit: "",
                log_floor: LOG_FLOOR,
                spans_days: range.spans_days(),
                range: Some(range),
                use_log,
                scan_note: shared_data
                    .claude_history_progress
                    .map(claude_series::scan_note),
            };

            self.draw_claude_bands(f, draw_loc, &mut widget_state.graph, &frame, &spec);
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
