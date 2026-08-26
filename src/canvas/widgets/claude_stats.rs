//! Drawing the Claude token-history stats graph.
//!
//! Claude Code's own `/status` screen bars token spend by day. This is the same idea at a
//! far finer grain -- one bucket a minute over the last hour -- so the shape of a working
//! session is visible rather than collapsed into a single bar.

use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use claude_metrics::{Bucket, ModelFamily};
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
};
use timeless::data::ChunkedData;

use crate::{
    app::App,
    canvas::{
        Painter,
        components::time_series::{AxisBound, ChartScaling, GraphData, LegendConstraints},
        drawing_utils::should_hide_x_label,
    },
    components::time_series::GraphDrawCtx,
};

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

            let border_style = self.get_border_style(widget_id, app_state.current_widget.widget_id);
            let graph_state = widget_state.graph.state_mut();
            let hide_x_labels = should_hide_x_label(
                app_state.app_config_fields.hide_time,
                app_state.app_config_fields.autohide_time,
                graph_state.autohide_timer_mut(),
                draw_loc,
            );

            let use_log = widget_state.use_log;

            let stacked = Stacked::build(&buckets, &families);
            let (y_max, y_labels) = if use_log {
                adjust_tokens_log(stacked.peak)
            } else {
                adjust_tokens_linear(stacked.peak)
            };

            let colours = &self.styles.claude_colour_styles;

            // Largest running total first. Each fill reaches down to the axis, so drawing
            // in descending order lets every later band paint over the lower part of the
            // one before it, leaving exactly one visible band per family. Drawing the other
            // way round would bury every family under the tallest one's fill.
            let graph_data: Vec<GraphData<'_, f64>> = stacked
                .layers
                .iter()
                .rev()
                .map(|layer| {
                    let style = if colours.is_empty() {
                        Style::default()
                    } else {
                        colours[layer.colour_index % colours.len()]
                    };

                    GraphData::default()
                        .name(
                            format!(
                                "{:<7}{}",
                                layer.family.label(),
                                format_tokens_padded(layer.own_total)
                            )
                            .into(),
                        )
                        .style(style)
                        .time(&stacked.times)
                        .values(&layer.running)
                        .filled(true)
                        // A bucket is a total over a minute, not a reading at an instant.
                        // Sloping between bucket centres would spread one busy minute over
                        // three and understate its peak; a staircase holds each bucket flat
                        // across the minute it covers, which is what `/status` draws.
                        .stepped(true)
                })
                .collect();

            let legend_constraints = LegendConstraints {
                width: Constraint::Ratio(3, 4),
                height: Constraint::Ratio(1, 2),
            };

            let marker = self.get_marker(app_state.app_config_fields.marker);
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
                    legend_position: app_state.app_config_fields.claude_legend_position,
                    legend_constraints: Some(legend_constraints),
                    pixel_renderer: self.pixel_renderer(),
                    last_time: stacked.times.last().copied(),
                    style_epoch: self.style_epoch(),
                },
                AxisBound::Max(y_max),
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

/// One family's band: its running total, and what it contributed on its own.
struct Layer {
    family: ModelFamily,
    /// This family's tokens plus every family below it, per bucket. Plotting running totals
    /// rather than raw values is what turns overlapping filled areas into a stack.
    running: ChunkedData<f64>,
    /// This family's own tokens across the window, for the legend.
    own_total: u64,
    /// Position in [`ModelFamily::ALL`], so a family that goes quiet cannot repaint the
    /// colours of the ones that remain.
    colour_index: usize,
}

/// The whole window, turned into something the time-series graph can draw.
struct Stacked {
    /// One instant per bucket, which is the x-axis the graph plots against.
    times: Vec<Instant>,
    layers: Vec<Layer>,
    /// Tallest total across all buckets, which sets the y-axis.
    peak: f64,
}

impl Stacked {
    /// Turn buckets into stacked running totals.
    ///
    /// The graph plots against [`Instant`]s, while the history is keyed by epoch
    /// milliseconds. Rather than convert each bucket's wall-clock time -- which cannot be
    /// done exactly, since `Instant` has no epoch -- the buckets are laid out backwards
    /// from now at their known spacing. They are contiguous and evenly spaced by
    /// construction, so this reproduces the real geometry without needing the mapping.
    fn build(buckets: &[Bucket], families: &[ModelFamily]) -> Self {
        let now = Instant::now();

        let times: Vec<Instant> = (0..buckets.len())
            .map(|index| {
                let back = buckets.len() - 1 - index;
                now.checked_sub(BUCKET.saturating_mul(back as u32))
                    .unwrap_or(now)
            })
            .collect();

        let mut running_totals = vec![0.0f64; buckets.len()];
        let mut layers = Vec::with_capacity(families.len());

        for family in families {
            let mut running = ChunkedData::default();
            let mut own_total = 0u64;

            for (index, bucket) in buckets.iter().enumerate() {
                let own = bucket.total_for(*family);
                own_total = own_total.saturating_add(own);

                running_totals[index] += own as f64;
                running.push(running_totals[index]);
            }

            layers.push(Layer {
                family: *family,
                running,
                own_total,
                colour_index: ModelFamily::ALL
                    .iter()
                    .position(|candidate| candidate == family)
                    .unwrap_or(0),
            });
        }

        let peak = running_totals.iter().copied().fold(0.0f64, f64::max);

        Self {
            times,
            layers,
            peak,
        }
    }
}

/// Bucket width, matching what the collector asks `claude-metrics` for.
const BUCKET: Duration = Duration::from_secs(60);

/// Render a token count compactly.
fn format_tokens(tokens: f64) -> String {
    if tokens >= 1_000_000_000.0 {
        format!("{:.1}B", tokens / 1_000_000_000.0)
    } else if tokens >= 1_000_000.0 {
        format!("{:.1}M", tokens / 1_000_000.0)
    } else if tokens >= 1_000.0 {
        format!("{:.1}k", tokens / 1_000.0)
    } else {
        format!("{tokens:.0}")
    }
}

/// The same, at a fixed width, so the legend box does not resize between frames.
fn format_tokens_padded(tokens: u64) -> String {
    format!("{:>9}", format_tokens(tokens as f64))
}

/// A linear axis, which is the honest one for stacked bands: they only add up to the total
/// if the scale is linear.
fn adjust_tokens_linear(peak: f64) -> (f64, Vec<String>) {
    // A window with no traffic would otherwise collapse the axis onto itself.
    let ceiling = if peak <= 0.0 { 1.0 } else { peak * 1.1 };

    let labels = [0.0, 0.5, 1.0]
        .into_iter()
        .map(|fraction| format!("{:>9}", format_tokens(ceiling * fraction)))
        .collect();

    (ceiling, labels)
}

/// A log axis, for a window where one family dwarfs the rest.
///
/// Stacked bands stop summing to the visible total here, which is why this is not the
/// default -- but a session that ran Opus for an hour and Haiku for a minute is unreadable
/// without it.
fn adjust_tokens_log(peak: f64) -> (f64, Vec<String>) {
    use crate::utils::general::saturating_log2;

    // 2^10 is about a thousand tokens. Below that the window is effectively idle, and
    // scaling to it would magnify noise into a full-height graph.
    const FLOOR: f64 = 10.0;

    let ceiling = (saturating_log2(peak) * 1.1).max(FLOOR);

    let labels = (0..3)
        .map(|step| {
            if step == 0 {
                // A log axis cannot reach zero, but the bottom of the plot is where "no
                // tokens at all" is drawn.
                format!("{:>9}", "0")
            } else {
                let value = ceiling * f64::from(step) / 2.0;
                format!("{:>9}", format_tokens(value.exp2()))
            }
        })
        .collect();

    (ceiling, labels)
}

#[cfg(test)]
mod tests {
    use claude_metrics::TokenTotals;

    use super::*;

    fn bucket(start_ms: u64, entries: &[(ModelFamily, u64)]) -> Bucket {
        Bucket {
            start_ms,
            totals: entries
                .iter()
                .map(|(family, tokens)| {
                    (
                        *family,
                        TokenTotals {
                            input: *tokens,
                            ..TokenTotals::default()
                        },
                    )
                })
                .collect(),
        }
    }

    fn values(data: &ChunkedData<f64>) -> Vec<f64> {
        data.iter().copied().collect()
    }

    #[test]
    fn layers_carry_running_totals_so_filled_areas_stack() {
        // Each band is drawn as a fill down to the axis, so the value plotted has to be the
        // running total. Plotting raw values would draw every family from the axis and bury
        // all but the largest.
        let buckets = [
            bucket(0, &[(ModelFamily::Opus, 100), (ModelFamily::Sonnet, 20)]),
            bucket(
                60_000,
                &[(ModelFamily::Opus, 50), (ModelFamily::Sonnet, 30)],
            ),
        ];

        let stacked = Stacked::build(&buckets, &[ModelFamily::Opus, ModelFamily::Sonnet]);

        assert_eq!(values(&stacked.layers[0].running), vec![100.0, 50.0]);
        assert_eq!(
            values(&stacked.layers[1].running),
            vec![120.0, 80.0],
            "the second layer must include the first"
        );
    }

    #[test]
    fn the_peak_is_the_tallest_stack_not_the_tallest_family() {
        // The y-axis has to clear the top of the stack. Scaling to the largest single
        // family would clip the band above it straight off the plot.
        let buckets = [bucket(
            0,
            &[(ModelFamily::Opus, 100), (ModelFamily::Sonnet, 60)],
        )];

        let stacked = Stacked::build(&buckets, &[ModelFamily::Opus, ModelFamily::Sonnet]);

        assert_eq!(stacked.peak, 160.0);
    }

    #[test]
    fn a_family_keeps_its_colour_as_others_come_and_go() {
        // Indexing by position among the *present* families would repaint every remaining
        // band the moment a quiet family dropped out of the window.
        let buckets = [bucket(0, &[(ModelFamily::Sonnet, 10)])];

        let alone = Stacked::build(&buckets, &[ModelFamily::Sonnet]);
        let together = Stacked::build(&buckets, &[ModelFamily::Opus, ModelFamily::Sonnet]);

        assert_eq!(alone.layers[0].colour_index, 1);
        assert_eq!(together.layers[1].colour_index, 1);
    }

    #[test]
    fn times_run_oldest_to_newest_one_bucket_apart() {
        let buckets = [bucket(0, &[]), bucket(60_000, &[]), bucket(120_000, &[])];

        let stacked = Stacked::build(&buckets, &[]);

        assert_eq!(stacked.times.len(), 3);
        assert!(
            stacked.times.windows(2).all(|w| w[1] > w[0]),
            "the graph plots left to right in time order"
        );
        assert_eq!(stacked.times[2] - stacked.times[1], BUCKET);
    }

    #[test]
    fn an_empty_window_still_gets_a_usable_axis() {
        // A layout can hold this widget on a machine that has never run Claude Code.
        let stacked = Stacked::build(&[], &[]);

        assert_eq!(stacked.peak, 0.0);

        let (ceiling, labels) = adjust_tokens_linear(stacked.peak);
        assert!(ceiling > 0.0, "a zero ceiling would collapse the y-axis");
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn token_counts_render_at_a_stable_width() {
        // A legend box that resizes every frame is worse than one that wastes a column.
        for tokens in [0, 999, 1_234, 5_000_000, 2_000_000_000] {
            assert_eq!(
                format_tokens_padded(tokens).len(),
                9,
                "{tokens} rendered at the wrong width"
            );
        }
    }

    #[test]
    fn token_counts_are_abbreviated_by_magnitude() {
        assert_eq!(format_tokens(999.0), "999");
        assert_eq!(format_tokens(1_234.0), "1.2k");
        assert_eq!(format_tokens(5_000_000.0), "5.0M");
        assert_eq!(format_tokens(2_000_000_000.0), "2.0B");
    }
}
