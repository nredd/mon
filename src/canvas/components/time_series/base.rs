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
        pixel::{ImageCache, PixelRenderer, Series, colour_to_rgb, rasterize, rgb_to_rgba},
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

        let x_axis = self.generate_x_axis();
        let y_axis = self.generate_y_axis();

        // Rasterise *before* consuming `graph_data` into datasets. The cell-drawn chart is
        // still rendered underneath, so a pixel path that cannot encode degrades to the
        // normal graph rather than to nothing.
        let pixel_area = pixels
            .as_mut()
            .and_then(|p| p.render(f, draw_loc, &graph_data, self));

        let data = graph_data.into_iter().map(create_dataset).collect();

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

        f.render_widget(
            TimeChart::new(data)
                .block(block)
                .x_axis(x_axis)
                .y_axis(y_axis)
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
                .scaling(self.scaling),
            draw_loc,
        );

        // Painted last so it sits over the cell-drawn lines rather than under them.
        if let Some((area, renderer, rgba, size)) = pixel_area {
            renderer.draw(f, area, rgba, size);
        }
    }
}

/// Creates a new [`Dataset`].
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
    /// Work out the plot area, rasterise into it, and hand back what to draw where.
    ///
    /// Returns `None` when the pixel path is off, unusable, or the area is too small to be
    /// worth it -- in every case the caller just leaves the cell-drawn graph alone.
    fn render<'c, F: Copy + Default + Into<f64>>(
        &'c mut self, _f: &mut Frame<'_>, draw_loc: Rect, graph_data: &[GraphData<'_, F>],
        graph: &TimeGraph<'_>,
    ) -> Option<(Rect, &'c PixelRenderer, &'c [u8], (u32, u32))> {
        if !self.renderer.is_active() {
            return None;
        }

        let last_time = self.last_time?;
        let area = plot_area(draw_loc, graph)?;

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

        let (rgba, size) = self.cache.get_or_update(
            last_time,
            display_time,
            area,
            self.style_epoch,
            pixels,
            |rgba, w, h| {
                let mut rgb = vec![0u8; (w as usize) * (h as usize) * 3];
                rasterize(
                    &mut rgb,
                    w,
                    h,
                    (graph.x_min, 0.0),
                    (0.0, y_max.max(f64::MIN_POSITIVE)),
                    background,
                    &series,
                );
                rgb_to_rgba(&rgb, rgba);
            },
        );

        Some((area, self.renderer, rgba, size))
    }
}

/// The region inside the border, past the y-labels and above the x-labels.
///
/// Reusing the surrounding chart's own axis geometry rather than re-deriving it means the
/// image lands exactly on the data region, and there is only one place that decides where
/// the axes go.
fn plot_area(draw_loc: Rect, graph: &TimeGraph<'_>) -> Option<Rect> {
    // Inside the border.
    let inner = draw_loc.inner(ratatui::layout::Margin::new(1, 1));

    let label_width: u16 = graph
        .y_labels
        .iter()
        .map(|l| l.chars().count() as u16)
        .max()
        .unwrap_or(0);

    let x_label_rows = u16::from(!graph.hide_x_labels);

    let width = inner.width.checked_sub(label_width)?;
    let height = inner.height.checked_sub(x_label_rows)?;

    // Below this there is nothing an image buys over cell markers.
    if width < 4 || height < 3 {
        return None;
    }

    Some(Rect::new(inner.x + label_width, inner.y, width, height))
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

fn create_dataset<F: Copy + Default + Into<f64>>(data: GraphData<'_, F>) -> Dataset<'_, F> {
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
        .style(style)
        .data(time, values)
        .graph_type(GraphType::Line);

    if let Some(name) = name {
        dataset.name(name)
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
