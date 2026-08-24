use std::{borrow::Cow, time::Instant};

use concat_string::concat_string;
use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::Style,
    symbols::Marker,
    text::{Line, Span},
    widgets::{BorderType, GraphType},
};
use timeless::data::ChunkedData;

use ratatui::style::Color;

use crate::canvas::{
    components::time_series::{
        pixel::{ImageCache, PixelRenderer, Series, colour_to_rgb, rasterize_smooth},
        *,
    },
    drawing_utils::widget_block,
};

/// Represents the data required by the [`TimeGraph`].
///
/// TODO: We may be able to get rid of this intermediary data structure.
#[derive(Default)]
pub(crate) struct GraphData<'a, F = f64> {
    time: &'a [Instant],
    values: Option<&'a ChunkedData<F>>,
    style: Style,
    name: Option<Cow<'a, str>>,
}

impl<'a, F> GraphData<'a, F> {
    pub fn time(mut self, time: &'a [Instant]) -> Self {
        self.time = time;
        self
    }

    pub fn values(mut self, values: &'a ChunkedData<F>) -> Self {
        self.values = Some(values);
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn name(mut self, name: Cow<'a, str>) -> Self {
        self.name = Some(name);
        self
    }
}

#[derive(Clone, Copy)]
pub struct LegendConstraints {
    /// The width legend constraints.
    pub width: Constraint,

    /// The height legend constraints.
    pub height: Constraint,
}

pub struct TimeGraph<'a> {
    /// The min x value.
    pub x_min: f64,

    /// Whether to hide the time/x-labels.
    pub hide_x_labels: bool,

    /// The min and max y boundaries.
    pub y_bounds: AxisBound,

    /// Any y-labels.
    pub y_labels: &'a [Cow<'a, str>],

    /// The graph style.
    pub graph_style: Style,

    /// The background colour/styling.
    pub general_widget_style: Style,

    /// The border style.
    pub border_style: Style,

    /// The border type.
    pub border_type: BorderType,

    /// The graph title.
    pub title: Cow<'a, str>,

    /// Whether this graph is selected.
    pub is_selected: bool,

    /// Whether this graph is expanded.
    pub is_expanded: bool,

    /// The title style.
    pub title_style: Style,

    /// The legend position.
    pub legend_position: Option<LegendPosition>,

    /// Any legend constraints.
    pub legend_constraints: Option<LegendConstraints>,

    /// The marker type. Unlike ratatui's native charts, we assume
    /// only a single type of marker.
    pub marker: Marker,

    /// The chart scaling.
    pub scaling: ChartScaling,
}

impl TimeGraph<'_> {
    /// Generates the [`Axis`] for the x-axis.
    fn generate_x_axis(&self) -> Axis<'_> {
        // Due to how we display things, we need to adjust the time bound values.
        let adjusted_x_bounds = AxisBound::Min(self.x_min);

        if self.hide_x_labels {
            Axis::default().bounds(adjusted_x_bounds)
        } else {
            let x_bound_left = ((-self.x_min) as u64 / 1000).to_string();
            let x_bound_right = "0s";

            let x_labels = vec![
                Span::styled(concat_string!(x_bound_left, "s"), self.graph_style),
                Span::styled(x_bound_right, self.graph_style),
            ];

            Axis::default()
                .bounds(adjusted_x_bounds)
                .labels(x_labels)
                .style(self.graph_style)
        }
    }

    /// Generates the [`Axis`] for the y-axis.
    fn generate_y_axis(&self) -> Axis<'_> {
        Axis::default()
            .bounds(self.y_bounds)
            .style(self.graph_style)
            .labels(
                self.y_labels
                    .iter()
                    .map(|label| Span::styled(label.clone(), self.graph_style))
                    .collect(),
            )
    }

    /// Assemble the chart around a ready-made set of datasets.
    ///
    /// Split out because `draw` builds the chart twice in the fallback case: once as a
    /// layout probe before the pixel path runs, and again with real point data if that path
    /// declined to draw.
    fn build_chart<'d, F: Copy + Default + Into<f64>>(
        &'d self, data: Vec<Dataset<'d, F>>,
    ) -> TimeChart<'d, F> {
        let block = {
            let mut b = widget_block(
                false,
                self.is_selected,
                self.border_type,
                self.general_widget_style,
            )
            .border_style(self.border_style)
            .title_top(Line::styled(self.title.as_ref(), self.title_style));

            if self.is_expanded {
                b = b.title_top(Line::styled(" Esc to go back ", self.title_style).right_aligned())
            }

            b
        };

        TimeChart::new(data)
            .block(block)
            .x_axis(self.generate_x_axis())
            .y_axis(self.generate_y_axis())
            .marker(self.marker)
            .style(self.general_widget_style)
            .legend_style(self.graph_style)
            .legend_position(self.legend_position)
            .hidden_legend_constraints({
                let constraints = self
                    .legend_constraints
                    .unwrap_or(DEFAULT_LEGEND_CONSTRAINTS);

                (constraints.width, constraints.height)
            })
            .scaling(self.scaling)
    }

    /// Draws a time graph at [`Rect`] location provided by `draw_loc`. A time
    /// graph is used to display data points throughout time in the x-axis.
    ///
    /// This time graph:
    /// - Draws with the higher time value on the left, and lower on the right.
    /// - Expects a [`TimeGraph`] to be passed in, which details how to draw the
    ///   graph.
    /// - Expects `graph_data`, which represents *what* data to draw, and
    ///   various details like style and optional legends.
    pub fn draw<F: Copy + Default + Into<f64>>(
        &self, f: &mut Frame<'_>, draw_loc: Rect, graph_data: Vec<GraphData<'_, F>>,
        mut pixels: Option<PixelDraw<'_>>,
    ) {
        // TODO: (points_rework_v1) can we reduce allocations in the underlying graph by
        // saving some sort of state?

        // Build the chart from legend-only datasets *first*, purely so its own `layout()`
        // can be asked where the plot area actually is. The pixel path must land on exactly
        // those cells: the chart reserves a y-axis column and an x-axis row that a naive
        // "inside the border, past the labels" calculation does not know about, and being
        // one column off puts the image's first cell -- which carries that whole row's
        // escape sequence -- under the y-axis line, which the chart then overwrites, wiping
        // every row of the image. See `TimeChart::graph_rect`.
        //
        // Legend-only is the right probe because `graph_area` does not depend on the
        // datasets at all, and `legend_area` depends only on their names and count, which
        // the legend-only datasets carry verbatim.
        let chart = self.build_chart(graph_data.iter().map(create_legend_only_dataset).collect());

        // `pixel_drawn` reflects whether the image is actually now sitting in the frame
        // buffer -- rasterising successfully is not enough, encoding to the terminal's
        // protocol can still fail on a degenerate size, and the fallback decision below
        // depends on the *placed* outcome rather than on any earlier step having gone well.
        let pixel_drawn = match (pixels.as_mut(), chart.graph_rect(draw_loc)) {
            (Some(pixels), Some(area)) => pixels.render_and_draw(f, area, &graph_data, self),
            _ => false,
        };

        // The image goes down *underneath* -- the chart draws its border, axis labels, and
        // legend as ordinary text on top of it in the normal course of rendering, which
        // reclaims those specific cells without any special-casing here. A successful pixel
        // draw also means the chart must not draw its own cell-marker line: it would be
        // redundant, and it would poke through the pixel image wherever a line cell falls.
        // That is what the legend-only datasets above already give us, so the chart only
        // needs rebuilding when the pixel path did *not* end up drawing.
        let chart = if pixel_drawn {
            chart
        } else {
            drop(chart);
            self.build_chart(graph_data.iter().map(create_dataset).collect())
        };

        // The image, if one was drawn, marked its cells `CellDiffOption::Skip` so ratatui's
        // diff engine leaves them alone next frame -- but the legend this chart is about to
        // draw over that same image is a plain `set_symbol` call that never clears the flag.
        // Get the rect before the chart is consumed by `render_widget` below.
        let legend_rect = pixel_drawn.then(|| chart.legend_rect(draw_loc)).flatten();

        f.render_widget(chart, draw_loc);

        // Now that the legend has legitimately overwritten those cells, let the diff engine
        // see the change: without this, the legend never reaches the terminal, because the
        // engine skips any cell still flagged from the image draw regardless of its content.
        if let Some(rect) = legend_rect {
            let buf = f.buffer_mut();
            for y in rect.top()..rect.bottom() {
                for x in rect.left()..rect.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_diff_option(ratatui::buffer::CellDiffOption::None);
                    }
                }
            }
        }
    }
}

/// Everything the pixel path needs to rasterise one graph, borrowed for the frame.
pub(crate) struct PixelDraw<'a> {
    /// The terminal graphics renderer. `None` disables the path.
    pub renderer: &'a PixelRenderer,
    /// Per-widget cache, so a still frame does not re-rasterise at redraw speed.
    pub cache: &'a mut ImageCache,
    /// The newest timestamp in the series, part of the cache key.
    pub last_time: Option<Instant>,
    /// Bumped when the theme or the set of series changes.
    pub style_epoch: u64,
}

impl PixelDraw<'_> {
    /// Rasterise into `area` and place the resulting image in the frame.
    ///
    /// `area` must be the chart's own `graph_area` -- see `TimeChart::graph_rect`. Nothing
    /// here re-derives it, because the one time it did, it landed a column left of the real
    /// plot area and the chart's y-axis line erased the image on every row.
    ///
    /// Returns whether the image is actually now sitting in the frame buffer. This has to
    /// be a real yes/no on the *placed* outcome, not on rasterisation alone: rasterising an
    /// RGBA buffer can succeed while the subsequent terminal-protocol encode still fails on
    /// a degenerate size, and the caller uses this return value to decide whether the chart
    /// needs to fall back to drawing its own cell-marker line.
    ///
    /// Draws the image *before* the caller draws the surrounding chart, so the chart's own
    /// border, axis labels, and legend -- ordinary text, drawn afterward in the normal
    /// course of rendering -- naturally overwrite their own cells and need no special
    /// handling here to avoid being erased by the image.
    fn render_and_draw<F: Copy + Default + Into<f64>>(
        &mut self, f: &mut Frame<'_>, area: Rect, graph_data: &[GraphData<'_, F>],
        graph: &TimeGraph<'_>,
    ) -> bool {
        if !self.renderer.is_active() {
            return false;
        }

        let Some(last_time) = self.last_time else {
            return false;
        };

        // Below this there is nothing an image buys over cell markers.
        if area.width < 4 || area.height < 3 {
            return false;
        }

        let (cell_w, cell_h) = self.renderer.font_size();
        let pixels = (
            u32::from(area.width) * u32::from(cell_w),
            u32::from(area.height) * u32::from(cell_h),
        );

        // Collect the series in data space. `x` is milliseconds relative to the right
        // edge, matching how the cell renderer plots time.
        let y_max = match graph.y_bounds {
            AxisBound::Max(max) => max,
            // The pixel path only handles the 0..max shape the graph widgets use.
            AxisBound::Zero | AxisBound::Min(_) => 1.0,
        };
        let series = collect_series(graph_data, last_time, graph.x_min, y_max);

        let background = colour_to_rgb(graph.general_widget_style.bg.unwrap_or(Color::Reset))
            .unwrap_or((0, 0, 0));

        let display_time = (-graph.x_min) as u64;

        let renderer = self.renderer;
        self.cache.get_or_update(
            last_time,
            display_time,
            area,
            self.style_epoch,
            pixels,
            |rgba, w, h| {
                rasterize_smooth(
                    rgba,
                    w,
                    h,
                    (graph.x_min, 0.0),
                    (0.0, y_max.max(f64::MIN_POSITIVE)),
                    background,
                    &series,
                );
            },
        );

        renderer.draw(f, area, self.cache)
    }
}

/// Flatten graph data into plain `(x, y)` series for the rasteriser.
fn collect_series<F: Copy + Default + Into<f64>>(
    graph_data: &[GraphData<'_, F>], last_time: Instant, x_min: f64, y_max: f64,
) -> Vec<Series> {
    graph_data
        .iter()
        .filter_map(|data| {
            let values = data.values?;
            let rgb = colour_to_rgb(data.style.fg.unwrap_or(Color::Reset))?;

            let points: Vec<(f64, f64)> = values
                .iter_along_base(data.time)
                .filter_map(|(time, value)| {
                    // Milliseconds before the right edge, so x runs negative to zero.
                    let offset = -(last_time.duration_since(*time).as_millis() as f64);
                    (offset >= x_min).then(|| ((offset), (*value).into().min(y_max)))
                })
                .collect();

            Some(Series { points, rgb })
        })
        .collect()
}

/// Creates a new [`Dataset`].
fn create_dataset<'a, F: Copy + Default + Into<f64>>(data: &'a GraphData<'a, F>) -> Dataset<'a, F> {
    let GraphData {
        time,
        values,
        style,
        name,
    } = data;

    let Some(values) = values else {
        return Dataset::default();
    };

    let dataset = Dataset::default()
        .style(*style)
        .data(time, values)
        .graph_type(GraphType::Line);

    if let Some(name) = name {
        dataset.name(name.as_ref())
    } else {
        dataset
    }
}

/// Build a dataset that carries a series' name and colour for the legend, but no points.
///
/// Used instead of [`create_dataset`] when the pixel path has actually drawn the line. The
/// underlying `Dataset` never gets a `.data()` call, so it draws nothing -- ratatui's chart
/// legend renders from `.name()`/`.style()` alone, regardless of point count. Without this,
/// the chart would draw its own cell-marker line directly on top of the pixel image (which
/// is drawn first -- see [`PixelDraw::render_and_draw`]), poking coarse marker glyphs
/// through the finer pixel line wherever the two paths cross.
fn create_legend_only_dataset<'a, F: Copy + Default + Into<f64>>(
    data: &'a GraphData<'a, F>,
) -> Dataset<'a, F> {
    let dataset = Dataset::default().style(data.style);

    if let Some(name) = data.name.as_ref() {
        dataset.name(name.as_ref())
    } else {
        dataset
    }
}

#[cfg(test)]
mod test {
    use std::borrow::Cow;

    use ratatui::{
        style::{Color, Style},
        symbols::Marker,
        text::Span,
        widgets::BorderType,
    };

    use super::{AxisBound, ChartScaling, TimeGraph};
    use crate::canvas::components::time_series::Axis;

    const Y_LABELS: [Cow<'static, str>; 3] = [
        Cow::Borrowed("0%"),
        Cow::Borrowed("50%"),
        Cow::Borrowed("100%"),
    ];

    fn create_time_series() -> TimeGraph<'static> {
        TimeGraph {
            title: " Network ".into(),
            x_min: -15000.0,
            hide_x_labels: false,
            y_bounds: AxisBound::Max(100.5),
            y_labels: &Y_LABELS,
            graph_style: Style::default().fg(Color::Red),
            general_widget_style: Style::default().bg(Color::Black),
            border_style: Style::default().fg(Color::Blue),
            border_type: BorderType::Plain,
            is_selected: false,
            is_expanded: false,
            title_style: Style::default().fg(Color::Cyan),
            legend_position: None,
            legend_constraints: None,
            marker: Marker::Braille,
            scaling: ChartScaling::Linear,
        }
    }

    #[test]
    fn time_series_gen_x_axis() {
        let tg = create_time_series();
        let style = Style::default().fg(Color::Red);
        let x_axis = tg.generate_x_axis();

        let actual = Axis::default()
            .bounds(AxisBound::Min(-15000.0))
            .labels(vec![Span::styled("15s", style), Span::styled("0s", style)])
            .style(style);
        assert_eq!(x_axis.bounds, actual.bounds);
        assert_eq!(x_axis.labels, actual.labels);
        assert_eq!(x_axis.style, actual.style);
    }

    #[test]
    fn time_series_gen_y_axis() {
        let tg = create_time_series();
        let style = Style::default().fg(Color::Red);
        let y_axis = tg.generate_y_axis();

        let actual = Axis::default()
            .bounds(AxisBound::Max(100.5))
            .labels(vec![
                Span::styled("0%", style),
                Span::styled("50%", style),
                Span::styled("100%", style),
            ])
            .style(style);

        assert_eq!(y_axis.bounds, actual.bounds);
        assert_eq!(y_axis.labels, actual.labels);
        assert_eq!(y_axis.style, actual.style);
    }
}
