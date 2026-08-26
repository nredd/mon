//! Shared pieces of the two Claude token graphs.
//!
//! Both graphs draw the same thing at different scales -- per-family token volume over a
//! window, as overlaid stepped outlines with an inline legend underneath. The stats graph
//! plots bucket totals over a selectable span; the rate graph divides by the bucket width
//! and fixes the span short. Everything that does not differ between them lives here.
//!
//! The visual target is Claude Code's own `/status` -> Stats -> Models screen: unfilled
//! staircases from the baseline, a tall linear y-axis, absolute dates or clock times on the
//! x-axis, and a `● Sonnet · ● Fable · ● Opus` legend below the plot rather than a boxed
//! one floating inside it.

use std::{
    borrow::Cow,
    time::{Duration, Instant},
};

use chrono::{Local, TimeZone as _};
use claude_metrics::{Bucket, ModelFamily, StatsRange};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
};
use timeless::data::ChunkedData;

/// One model family's outline.
pub(crate) struct Band {
    pub(crate) family: ModelFamily,
    /// This family's own value per bucket. Not a running total: the bands are drawn
    /// overlaid from the baseline rather than stacked, so each one has to carry what that
    /// family actually spent.
    pub(crate) values: ChunkedData<f64>,
    /// This family's total across the window, for the legend.
    pub(crate) total: u64,
    /// Position in [`ModelFamily::ALL`], so a family that goes quiet cannot repaint the
    /// colours of the ones that remain.
    pub(crate) colour_index: usize,
}

/// A window of buckets, turned into something the time-series graph can draw.
pub(crate) struct BucketSeries {
    /// One instant per bucket, which is the x-axis the graph plots against.
    pub(crate) times: Vec<Instant>,
    /// Each bucket's start in Unix epoch milliseconds, parallel to `times`.
    ///
    /// `times` cannot answer "what wall-clock time is this" -- an [`Instant`] has no epoch
    /// -- and the axis labels are absolute, so the two run side by side.
    pub(crate) starts: Vec<u64>,
    pub(crate) bands: Vec<Band>,
    /// The tallest *single family* value in the window.
    ///
    /// Overlaid outlines each start at the baseline, so the axis only has to clear the
    /// largest one. Scaling to the sum, which a stacked graph would need, would leave every
    /// outline squashed into the bottom of the plot.
    pub(crate) peak: f64,
}

impl BucketSeries {
    /// Turn buckets into one outline per family.
    ///
    /// `divisor` converts a bucket total into the plotted value: one for token counts, the
    /// bucket's width in seconds for a rate.
    ///
    /// The graph plots against [`Instant`]s while the history is keyed by epoch
    /// milliseconds. Rather than convert each bucket's wall-clock time -- which cannot be
    /// done exactly, since `Instant` has no epoch -- the buckets are laid out backwards
    /// from now at their known spacing. They are contiguous and evenly spaced by
    /// construction, so this reproduces the real geometry without needing the mapping.
    pub(crate) fn build(
        buckets: &[Bucket], families: &[ModelFamily], bucket: Duration, divisor: f64,
    ) -> Self {
        let now = Instant::now();
        let divisor = if divisor > 0.0 { divisor } else { 1.0 };

        let times: Vec<Instant> = (0..buckets.len())
            .map(|index| {
                let back = buckets.len() - 1 - index;
                now.checked_sub(bucket.saturating_mul(back as u32))
                    .unwrap_or(now)
            })
            .collect();

        let starts: Vec<u64> = buckets.iter().map(|bucket| bucket.start_ms).collect();

        let mut peak = 0.0f64;
        let mut bands = Vec::with_capacity(families.len());

        for family in families {
            let mut values = ChunkedData::default();
            let mut total = 0u64;

            for bucket in buckets {
                let own = bucket.total_for(*family);
                total = total.saturating_add(own);

                let plotted = own as f64 / divisor;
                peak = peak.max(plotted);
                values.push(plotted);
            }

            bands.push(Band {
                family: *family,
                values,
                total,
                colour_index: ModelFamily::ALL
                    .iter()
                    .position(|candidate| candidate == family)
                    .unwrap_or(0),
            });
        }

        Self {
            times,
            starts,
            bands,
            peak,
        }
    }

    /// Absolute x-axis labels, oldest first, evenly spaced across the plotted span.
    ///
    /// `count` labels are sampled across `[first bucket start, right edge]`, where the
    /// right edge is one bucket past the newest start -- the newest bucket is the one
    /// currently in progress, so the axis reaches to now rather than to when now's bucket
    /// began.
    pub(crate) fn x_labels(
        &self, count: usize, bucket: Duration, spans_days: bool,
    ) -> Vec<Cow<'static, str>> {
        let count = count.max(2);

        let (Some(&first), Some(&last)) = (self.starts.first(), self.starts.last()) else {
            return Vec::new();
        };

        let right_edge = last.saturating_add(millis(bucket));
        let span = right_edge.saturating_sub(first);

        (0..count)
            .map(|index| {
                let offset = span * index as u64 / (count as u64 - 1);
                format_clock(first.saturating_add(offset), spans_days).into()
            })
            .collect()
    }
}

/// Render an epoch-millisecond instant as a local-time axis label.
///
/// Dates above a day, clock times below it: two labels an hour apart share a date and are
/// only told apart by the time, and two labels a week apart are the reverse.
fn format_clock(epoch_ms: u64, spans_days: bool) -> String {
    let seconds = i64::try_from(epoch_ms / 1000).unwrap_or(0);

    let Some(local) = Local.timestamp_opt(seconds, 0).single() else {
        // Out of range, or an ambiguous local time across a DST fold. Neither is worth
        // drawing a wrong label for.
        return String::new();
    };

    if spans_days {
        local.format("%b %-d").to_string()
    } else {
        local.format("%H:%M").to_string()
    }
}

/// Evenly spaced y-axis labels from zero to `ceiling`, bottom first.
///
/// The native screen fills the axis with labels rather than showing only the endpoints,
/// which is what makes a bar's height readable at a glance instead of merely comparable.
pub(crate) fn y_labels(ceiling: f64, count: usize, unit: &str) -> Vec<Cow<'static, str>> {
    let count = count.max(2);

    (0..count)
        .map(|index| {
            let value = ceiling * index as f64 / (count as f64 - 1.0);
            let rendered = if index == 0 {
                // The bottom of the plot is "nothing spent". Rendering it as `0/s` on the
                // rate graph and a bare `0` on the stats graph keeps the unit visible
                // without repeating it on every label.
                format!("0{unit}")
            } else {
                format!("{}{unit}", format_tokens(value))
            };

            Cow::Owned(rendered)
        })
        .collect()
}

/// A y-axis ceiling and its labels.
///
/// Linear is the honest default for token volume: the bands are compared by height, and on
/// a log axis a bar twice as tall is not twice the tokens. The log option stays for the
/// case it was added for -- a window where one family dwarfs the rest badly enough that the
/// others sit flat on the floor.
///
/// `log_floor` is the `log2` value below which the axis stops tracking the data. Without
/// one, a window holding a couple of tokens magnifies that noise to full height and reads
/// as a busy session.
pub(crate) fn axis(
    peak: f64, use_log: bool, log_floor: f64, count: usize, unit: &str,
) -> (f64, Vec<Cow<'static, str>>) {
    if !use_log {
        // A window with no traffic would otherwise collapse the axis onto itself.
        let ceiling = if peak > 0.0 { peak * 1.1 } else { 1.0 };

        return (ceiling, y_labels(ceiling, count, unit));
    }

    let ceiling = (crate::utils::general::saturating_log2(peak) * 1.1).max(log_floor);
    let count = count.max(2);

    let labels = (0..count)
        .map(|index| {
            if index == 0 {
                // A log axis cannot reach zero, but the bottom of the plot is where "no
                // tokens at all" is drawn, and labelling it `1` would be a lie.
                Cow::Owned(format!("0{unit}"))
            } else {
                let value = ceiling * index as f64 / (count as f64 - 1.0);
                Cow::Owned(format!("{}{unit}", format_tokens(value.exp2())))
            }
        })
        .collect();

    (ceiling, labels)
}

/// Rows the plot itself will get, so the y-axis can pick a label count that fits.
///
/// The chart works this out for real in its own `layout()`, but that runs after the labels
/// are handed to it, so this reproduces the arithmetic: two rows of border, the reserved
/// footer, and a row each for the x-labels and the x-axis line.
pub(crate) fn plot_height(draw_loc: Rect, footer_rows: u16) -> u16 {
    draw_loc.height.saturating_sub(2 + footer_rows + 2)
}

/// Columns the plot itself will get, so the x-axis can pick a label count that fits.
///
/// Two of border, one for the y-axis line, and the y-label gutter, which
/// [`format_tokens`] holds to at most seven characters.
pub(crate) fn plot_width(draw_loc: Rect) -> u16 {
    draw_loc.width.saturating_sub(2 + 1 + 7)
}

/// How many y-axis labels fit in a plot of this height.
///
/// Capped at the native screen's nine. Below three the axis stops saying anything useful,
/// so that is the floor even on a short widget.
pub(crate) fn y_label_count(plot_height: u16) -> usize {
    usize::from(plot_height.saturating_sub(1)).clamp(3, 9)
}

/// How many x-axis labels a plot of this width can carry without them colliding.
///
/// Each label is at most six columns (`Aug 21`), and they need visible air between them.
pub(crate) fn x_label_count(plot_width: u16) -> usize {
    usize::from(plot_width / 20).clamp(2, 5)
}

/// Render a token count compactly.
pub(crate) fn format_tokens(tokens: f64) -> String {
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

/// The swatch drawn beside each family in the legend.
const SWATCH: &str = "●";

/// What separates entries in both footer rows.
const SEPARATOR: &str = " · ";

/// How many rows [`draw_footer`] wants, given the space it has.
///
/// Both rows are worth having, but neither is worth stealing the plot for. Below the
/// thresholds here the graph itself is already too short to read, so the footer gives way
/// first -- the selector row before the legend, since the range is also recoverable from
/// the axis labels while the colour mapping is not.
pub(crate) fn footer_rows(draw_loc: Rect) -> u16 {
    match draw_loc.height {
        0..=7 => 0,
        8..=10 => 1,
        _ => 2,
    }
}

/// Paint the inline legend and the range selector into the rows [`footer_rows`] reserved.
///
/// Called *after* the graph draws. The rows are reserved through the chart's own block
/// padding, so nothing has been drawn into them and this does not have to fight the plot
/// for cells.
pub(crate) fn draw_footer(
    f: &mut Frame<'_>, draw_loc: Rect, bands: &[Band], colours: &[Style],
    range: Option<StatsRange>, active_style: Style, muted_style: Style, note: Option<&str>,
) {
    let rows = footer_rows(draw_loc);
    if rows == 0 || draw_loc.width <= 2 {
        return;
    }

    // Inside the border, and above the bottom edge.
    let inner_width = draw_loc.width.saturating_sub(2);
    let bottom = draw_loc.y + draw_loc.height.saturating_sub(1);

    let legend_y = bottom.saturating_sub(rows);
    let legend_area = Rect::new(draw_loc.x + 1, legend_y, inner_width, 1);

    match note {
        // A scan in progress outranks the legend: the graph is showing partial data and
        // saying so matters more than saying what colour Opus is.
        Some(note) => {
            f.render_widget(Line::styled(note.to_owned(), muted_style), legend_area);
        }
        None => {
            f.render_widget(
                legend_line(bands, colours, muted_style, inner_width),
                legend_area,
            );
        }
    }

    if rows < 2 {
        return;
    }

    if let Some(range) = range {
        let selector_area = Rect::new(draw_loc.x + 1, legend_y + 1, inner_width, 1);
        f.render_widget(
            selector_line(range, active_style, muted_style),
            selector_area,
        );
    }
}

/// `● Opus 1.2M · ● Sonnet 840k`, dropping the totals if they do not fit.
///
/// The totals are the first thing to go rather than the last entry, because a legend that
/// silently omits a family that is actually on the plot is worse than one that only names
/// the families.
fn legend_line<'a>(bands: &[Band], colours: &[Style], muted_style: Style, width: u16) -> Line<'a> {
    let with_totals = legend_spans(bands, colours, muted_style, true);

    if line_width(&with_totals) <= usize::from(width) {
        return Line::from(with_totals);
    }

    Line::from(legend_spans(bands, colours, muted_style, false))
}

fn legend_spans<'a>(
    bands: &[Band], colours: &[Style], muted_style: Style, totals: bool,
) -> Vec<Span<'a>> {
    let mut spans = Vec::with_capacity(bands.len() * 3);

    for band in bands {
        if !spans.is_empty() {
            spans.push(Span::styled(SEPARATOR, muted_style));
        }

        let colour = if colours.is_empty() {
            Style::default()
        } else {
            colours[band.colour_index % colours.len()]
        };

        spans.push(Span::styled(SWATCH, colour));

        let text = if totals {
            format!(
                " {} {}",
                band.family.label(),
                format_tokens(band.total as f64)
            )
        } else {
            format!(" {}", band.family.label())
        };

        spans.push(Span::styled(text, muted_style));
    }

    spans
}

/// `30m · 2h · 8h · 24h · 7d · 30d`, with the active one picked out.
fn selector_line<'a>(range: StatsRange, active_style: Style, muted_style: Style) -> Line<'a> {
    let mut spans = Vec::with_capacity(StatsRange::ALL.len() * 2);

    for candidate in StatsRange::ALL {
        if !spans.is_empty() {
            spans.push(Span::styled(SEPARATOR, muted_style));
        }

        let style = if candidate == range {
            active_style
        } else {
            muted_style
        };

        spans.push(Span::styled(candidate.label(), style));
    }

    Line::from(spans)
}

fn line_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(ratatui::text::Span::width).sum()
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn bands_carry_their_own_value_not_a_running_total() {
        // Overlaid outlines each start at the baseline, so a band has to plot what its
        // family actually spent. Plotting a running total, which is what a stacked fill
        // needs, would draw every family at the height of the whole stack.
        let buckets = [
            bucket(0, &[(ModelFamily::Opus, 100), (ModelFamily::Sonnet, 20)]),
            bucket(
                60_000,
                &[(ModelFamily::Opus, 50), (ModelFamily::Sonnet, 30)],
            ),
        ];

        let series = BucketSeries::build(
            &buckets,
            &[ModelFamily::Opus, ModelFamily::Sonnet],
            MINUTE,
            1.0,
        );

        assert_eq!(values(&series.bands[0].values), vec![100.0, 50.0]);
        assert_eq!(values(&series.bands[1].values), vec![20.0, 30.0]);
    }

    #[test]
    fn the_peak_is_the_tallest_family_not_the_tallest_stack() {
        // The axis only has to clear the largest outline. Scaling to the sum would squash
        // every band into the bottom of the plot.
        let buckets = [bucket(
            0,
            &[(ModelFamily::Opus, 100), (ModelFamily::Sonnet, 60)],
        )];

        let series = BucketSeries::build(
            &buckets,
            &[ModelFamily::Opus, ModelFamily::Sonnet],
            MINUTE,
            1.0,
        );

        assert_eq!(series.peak, 100.0);
    }

    #[test]
    fn a_divisor_turns_totals_into_a_rate() {
        let buckets = [bucket(0, &[(ModelFamily::Opus, 600)])];

        let series = BucketSeries::build(&buckets, &[ModelFamily::Opus], MINUTE, 60.0);

        assert_eq!(values(&series.bands[0].values), vec![10.0]);
        assert_eq!(
            series.bands[0].total, 600,
            "the legend still reports tokens, not a rate"
        );
    }

    #[test]
    fn a_zero_divisor_cannot_produce_infinities() {
        // A degenerate bucket width reaching this would otherwise plot `inf` and take the
        // whole axis with it.
        let buckets = [bucket(0, &[(ModelFamily::Opus, 600)])];

        let series = BucketSeries::build(&buckets, &[ModelFamily::Opus], MINUTE, 0.0);

        assert!(series.peak.is_finite());
    }

    #[test]
    fn a_family_keeps_its_colour_as_others_come_and_go() {
        // Indexing by position among the *present* families would repaint every remaining
        // band the moment a quiet family dropped out of the window.
        let buckets = [bucket(0, &[(ModelFamily::Sonnet, 10)])];

        let alone = BucketSeries::build(&buckets, &[ModelFamily::Sonnet], MINUTE, 1.0);
        let together = BucketSeries::build(
            &buckets,
            &[ModelFamily::Opus, ModelFamily::Sonnet],
            MINUTE,
            1.0,
        );

        assert_eq!(alone.bands[0].colour_index, 1);
        assert_eq!(together.bands[1].colour_index, 1);
    }

    #[test]
    fn times_run_oldest_to_newest_one_bucket_apart() {
        let buckets = [bucket(0, &[]), bucket(60_000, &[]), bucket(120_000, &[])];

        let series = BucketSeries::build(&buckets, &[], MINUTE, 1.0);

        assert_eq!(series.times.len(), 3);
        assert!(
            series.times.windows(2).all(|w| w[1] > w[0]),
            "the graph plots left to right in time order"
        );
        assert_eq!(series.times[2] - series.times[1], MINUTE);
        assert_eq!(series.starts, vec![0, 60_000, 120_000]);
    }

    #[test]
    fn x_labels_span_the_plot_and_reach_the_right_edge() {
        // The newest bucket is the one in progress, so the axis has to reach one bucket
        // past its start -- otherwise the rightmost label names a time a minute stale.
        let buckets = [
            bucket(1_787_601_600_000, &[]),
            bucket(1_787_601_660_000, &[]),
            bucket(1_787_601_720_000, &[]),
        ];

        let series = BucketSeries::build(&buckets, &[], MINUTE, 1.0);
        let labels = series.x_labels(3, MINUTE, false);

        assert_eq!(labels.len(), 3);
        assert!(labels.iter().all(|label| label.contains(':')));

        let spans_days = series.x_labels(3, MINUTE, true);
        assert!(
            spans_days.iter().all(|label| !label.contains(':')),
            "a multi-day range wants dates, not clock times: {spans_days:?}"
        );
    }

    #[test]
    fn an_empty_window_produces_no_x_labels_rather_than_wrong_ones() {
        let series = BucketSeries::build(&[], &[], MINUTE, 1.0);

        assert!(series.x_labels(3, MINUTE, false).is_empty());
        assert_eq!(series.peak, 0.0);
    }

    #[test]
    fn a_linear_axis_clears_the_peak_without_burying_it() {
        let (ceiling, labels) = axis(2_000_000.0, false, 10.0, 5, "");

        assert!(ceiling > 2_000_000.0, "the peak must not touch the border");
        assert!(
            2_000_000.0 / ceiling > 0.85,
            "and must still land near the top, got {:.0}%",
            200_000_000.0 / ceiling
        );
        assert_eq!(labels[0], "0");
    }

    #[test]
    fn an_empty_window_still_gets_a_usable_axis() {
        // A layout can hold this widget on a machine that has never run Claude Code.
        let (ceiling, labels) = axis(0.0, false, 10.0, 3, "");

        assert!(ceiling > 0.0, "a zero ceiling would collapse the y-axis");
        assert_eq!(labels.len(), 3);
    }

    #[test]
    fn a_log_axis_tracks_the_peak_instead_of_snapping_to_a_decade() {
        // The empty-space bug: snapping a 1.2k peak up to the 1M decade drew it at half the
        // plot height and left the top of the widget permanently blank.
        let (ceiling, _) = axis(1_200.0, true, 4.0, 3, "/s");
        let peak = crate::utils::general::saturating_log2(1_200.0);

        assert!(
            peak / ceiling > 0.85,
            "a peak should land near the top, got {:.0}%",
            (peak / ceiling) * 100.0
        );
    }

    #[test]
    fn a_log_axis_keeps_a_floor_so_a_quiet_window_stays_quiet() {
        // Without it, two stray tokens draw the same shape as a busy session.
        let (ceiling, labels) = axis(2.0, true, 4.0, 3, "/s");

        assert!(ceiling >= 4.0, "got {ceiling}");
        assert_eq!(labels[0], "0/s");
    }

    #[test]
    fn y_labels_run_from_zero_to_the_ceiling() {
        let labels = y_labels(2_000_000.0, 5, "");

        assert_eq!(labels.len(), 5);
        assert_eq!(labels[0], "0");
        assert_eq!(labels[4], "2.0M");
        assert_eq!(labels[2], "1.0M");
    }

    #[test]
    fn y_labels_carry_the_unit_including_on_the_zero() {
        let labels = y_labels(1_000.0, 3, "/s");

        assert_eq!(labels[0], "0/s");
        assert_eq!(labels[2], "1.0k/s");
    }

    #[test]
    fn label_counts_stay_inside_what_the_widget_can_draw() {
        assert_eq!(
            y_label_count(0),
            3,
            "a degenerate height still gets an axis"
        );
        assert_eq!(y_label_count(4), 3);
        assert_eq!(y_label_count(100), 9, "capped at the native screen's nine");

        assert_eq!(x_label_count(0), 2, "the chart needs at least two");
        assert_eq!(x_label_count(200), 5);
    }

    #[test]
    fn the_plot_area_leaves_room_for_everything_around_it() {
        // Borders, the footer, the x-label row, and the x-axis line all come out of the
        // widget before the plot gets any. Overstating the plot picks a label count that
        // does not fit, and the chart then drops labels off the bottom without saying so.
        let loc = Rect::new(0, 0, 80, 20);

        assert_eq!(plot_height(loc, 2), 14);
        assert_eq!(plot_width(loc), 70);
    }

    #[test]
    fn a_widget_too_small_to_plot_does_not_underflow() {
        let tiny = Rect::new(0, 0, 4, 3);

        assert_eq!(plot_height(tiny, 2), 0);
        assert_eq!(plot_width(tiny), 0);
    }

    #[test]
    fn the_footer_gives_way_before_the_plot_does() {
        // A graph too short to read is worse than one without a legend.
        let at = |height| footer_rows(Rect::new(0, 0, 40, height));

        assert_eq!(at(6), 0);
        assert_eq!(at(9), 1, "the legend outlives the selector");
        assert_eq!(at(20), 2);
    }

    /// Paint a footer into a fixed buffer and hand back its rows.
    fn render_footer(width: u16, height: u16, note: Option<&str>) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend, style::Color};

        let buckets = [bucket(
            0,
            &[
                (ModelFamily::Opus, 1_200_000),
                (ModelFamily::Sonnet, 840_000),
            ],
        )];
        let series = BucketSeries::build(
            &buckets,
            &[ModelFamily::Opus, ModelFamily::Sonnet],
            MINUTE,
            1.0,
        );

        let colours = [
            Style::default().fg(Color::Blue),
            Style::default().fg(Color::Red),
        ];
        let area = Rect::new(0, 0, width, height);

        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
        terminal
            .draw(|f| {
                draw_footer(
                    f,
                    area,
                    &series.bands,
                    &colours,
                    Some(StatsRange::TwoHours),
                    Style::default().fg(Color::Yellow),
                    Style::default().fg(Color::Gray),
                    note,
                );
            })
            .expect("draw");

        let buffer = terminal.backend().buffer().clone();

        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_footer_lands_in_the_rows_the_graph_reserved() {
        // Two rows above the bottom border, inset by one column so the legend does not sit
        // on top of the border itself.
        let rows = render_footer(60, 16, None);

        assert_eq!(rows[13], " ● Opus 1.2M · ● Sonnet 840.0k");
        assert_eq!(rows[14], " 30m · 2h · 8h · 24h · 7d · 30d");
        assert!(rows[15].is_empty(), "the bottom row is the border's");
        assert!(
            rows[..13].iter().all(|row| row.is_empty()),
            "nothing above the reserved rows may be touched"
        );
    }

    #[test]
    fn a_narrow_footer_drops_the_totals_before_it_drops_a_family() {
        // A legend that silently omits a family that is on the plot is worse than one that
        // only names the families.
        let rows = render_footer(24, 16, None);

        assert_eq!(rows[13], " ● Opus · ● Sonnet");
    }

    #[test]
    fn a_scan_note_takes_the_legend_row() {
        // Partial data is worth saying out loud; what colour Opus is can wait.
        let rows = render_footer(60, 16, Some("scanning transcripts... 62%"));

        assert_eq!(rows[13], " scanning transcripts... 62%");
        assert_eq!(rows[14], " 30m · 2h · 8h · 24h · 7d · 30d");
    }

    #[test]
    fn a_short_widget_keeps_the_legend_and_drops_the_selector() {
        let rows = render_footer(60, 9, None);

        assert_eq!(rows[7], " ● Opus 1.2M · ● Sonnet 840.0k");
        assert!(rows[8].is_empty());
    }

    #[test]
    fn token_counts_are_abbreviated_by_magnitude() {
        assert_eq!(format_tokens(999.0), "999");
        assert_eq!(format_tokens(1_234.0), "1.2k");
        assert_eq!(format_tokens(5_000_000.0), "5.0M");
        assert_eq!(format_tokens(2_000_000_000.0), "2.0B");
    }
}
