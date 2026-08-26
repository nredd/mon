//! Drawing the Claude token-rate graph.
//!
//! The same picture as the stats graph next to it, over a fixed short window and divided by
//! the bucket width: what is being spent right now, by model family. Everything they draw
//! the same way lives in [`crate::canvas::widgets::claude_series`].
//!
//! # Why this does not difference live session totals
//!
//! It used to. `ClaudeMetrics::totals_by_model` reports cumulative tokens across the
//! sessions currently in the registry, and this graph differenced that against the previous
//! tick keyed by model family. Two things made that wrong rather than merely coarse:
//!
//! - The "first sighting, do not compute a rate" guard keyed on the *family*, which is
//!   already present from every other session. So a new or resumed session -- whose
//!   accumulator is rebuilt from zero with its tailer at offset zero, re-reading the whole
//!   transcript -- put its entire lifetime token count into one tick's delta. Every family
//!   in that transcript spiked together, and since the totals are cache-inclusive that was
//!   routinely millions of tokens per second.
//! - The session registry is written by a running process and can be read mid-write. A
//!   single torn read dropped a session for one tick and brought it back backfilled on the
//!   next, which is the same spike on a loop.
//!
//! [`claude_metrics::TokenHistory`] attributes every record to a bucket by the record's own
//! ISO-8601 timestamp and drops undated records rather than piling them onto "now", so a
//! backfill lands in the past where it belongs and changes nothing about the present.

use ratatui::{Frame, layout::Rect};

use crate::{
    app::App,
    canvas::{
        Painter,
        drawing_utils::should_hide_x_label,
        widgets::claude_series::{self, BandFrame, BandSpec},
    },
    collection::claude::RATE_BUCKET,
};

/// Where the log axis stops tracking the data: `2^4` is sixteen tokens a second. Without a
/// floor, a window that only ever saw a couple of tokens magnifies that to full height.
const LOG_FLOOR: f64 = 4.0;

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
                title: " Claude Tokens ",
                buckets: &shared_data.claude_rate_history,
                families: &shared_data.claude_history_families,
                bucket: RATE_BUCKET,
                // Tokens in a bucket over the seconds that bucket covers.
                divisor: RATE_BUCKET.as_secs_f64(),
                unit: "/s",
                log_floor: LOG_FLOOR,
                // Always clock times: the window is ten minutes, so a date would repeat.
                spans_days: false,
                // No selector row -- this graph's window is fixed, and the stats graph
                // beside it is the one that answers longer spans.
                range: None,
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
    use crate::collection::claude::RATE_WINDOW;

    #[test]
    fn the_rate_window_divides_into_a_drawable_number_of_buckets() {
        // Same constraint the selectable ranges are held to: a terminal graph has a couple
        // of hundred columns, so a window that yielded thousands of buckets would allocate
        // them all every tick to draw a couple of hundred.
        let buckets = RATE_WINDOW.as_secs() / RATE_BUCKET.as_secs();

        assert!((60..=180).contains(&buckets), "{buckets} buckets");
        assert_eq!(RATE_WINDOW.as_secs() % RATE_BUCKET.as_secs(), 0);
    }
}
