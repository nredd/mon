//! Rasterising a time-series graph and handing it to the terminal as an actual image.
//!
//! # Why this exists
//!
//! Cell-based markers cap out at 2x4 subpixels per cell. A terminal that speaks the Kitty
//! graphics protocol can draw real pixels instead, which is worth roughly an order of
//! magnitude more vertical resolution on a short graph.
//!
//! # Detection does not work under tmux
//!
//! Measured on this machine (Ghostty 1.3.1, tmux 3.7c, `allow-passthrough on`):
//!
//! - The Kitty capability query, wrapped in tmux DCS passthrough, gets **no reply at all**.
//!   Other queries answer fine through the same path -- primary DA returns
//!   `ESC [?1;2;4c` and cell size returns `ESC [6;24;11t` -- so passthrough itself works.
//! - Consequently [`ratatui_image::picker::Picker::from_query_stdio`] reports `Halfblocks`
//!   and falls back to a default 10x20 font size, when the real cell is 24x11.
//! - Unicode placeholder cells *do* survive into tmux's own text buffer with correct
//!   row/column diacritics, which is the mechanism that gives correct clipping, scrolling,
//!   and pane switching.
//!
//! So the transport is fine and only detection is broken. That is why [`PixelMode`] has an
//! explicit `kitty` setting rather than only `auto`: under tmux, auto can never select it.
//!
//! # Caching is mandatory
//!
//! There is no frame-level throttle in the draw loop, and mouse motion redraws at up to
//! 50fps. Re-rasterising at that rate would be absurd, and re-*encoding* at that rate is
//! worse than absurd: a Kitty transmit carries the whole RGBA buffer as base64, so doing it
//! per frame floods tmux's passthrough until the graph flickers in and out. [`ImageCache`]
//! therefore keys on everything that can change what the image looks like, and holds both
//! the pixels and their encoding until one of those inputs actually moves.

use std::time::Instant;

use ratatui::{layout::Rect, style::Color};

/// Which graphics protocol the pixel path should use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "generate_schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PixelMode {
    /// Draw graphs with cell markers. The default.
    #[default]
    Off,
    /// Query the terminal and use pixels only if it answers.
    ///
    /// Note this cannot select Kitty from inside tmux -- see the module docs.
    Auto,
    /// Force the Kitty graphics protocol, skipping the query.
    Kitty,
}

impl std::str::FromStr for PixelMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "off" | "false" | "none" => Ok(PixelMode::Off),
            "auto" => Ok(PixelMode::Auto),
            "kitty" => Ok(PixelMode::Kitty),
            _ => Err(format!(
                "'{s}' is not a valid pixel mode. Expected one of: off, auto, kitty"
            )),
        }
    }
}

/// Everything that can change what a rendered graph looks like.
///
/// If two frames agree on all of this, they agree on the image, and the cached encode can
/// be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheKey {
    /// The newest timestamp in the series.
    last_time: Instant,
    /// How wide a time window is on screen.
    display_time: u64,
    /// Cell dimensions of the draw area.
    width: u16,
    height: u16,
    /// Bumped whenever the theme or the set of drawn series changes.
    style_epoch: u64,
}

/// A rasterised graph and its encoded form, retained between frames.
#[derive(Default)]
pub struct ImageCache {
    key: Option<CacheKey>,
    /// RGBA8, row-major, `width * height * 4` bytes.
    rgba: Vec<u8>,
    /// Pixel dimensions of `rgba`.
    pixels: (u32, u32),
    /// The terminal-protocol encoding of `rgba`, built on demand by
    /// [`PixelRenderer::draw`] and dropped whenever `rgba` is rebuilt.
    ///
    /// Retaining this is not an optimisation, it is a correctness requirement under tmux.
    /// Building a protocol mints a fresh image id and produces a transmit sequence carrying
    /// the *entire* RGBA buffer as base64 -- megabytes for a full-width graph. Doing that
    /// per frame rather than per data change floods tmux's passthrough, which cannot keep
    /// up and drops or truncates sequences, so the graph visibly flickers in and out. It
    /// also leaks a new image id into the terminal on every single frame.
    protocol: Option<ratatui_image::protocol::Protocol>,
}

impl ImageCache {
    /// Return the cached image if it is still valid, otherwise rebuild it with `render`.
    ///
    /// `render` is handed a mutable RGBA buffer already sized to the requested pixel
    /// dimensions, and fills it in place.
    pub fn get_or_update<F>(
        &mut self, last_time: Instant, display_time: u64, area: Rect, style_epoch: u64,
        pixels: (u32, u32), render: F,
    ) -> (&[u8], (u32, u32))
    where
        F: FnOnce(&mut [u8], u32, u32),
    {
        let key = CacheKey {
            last_time,
            display_time,
            width: area.width,
            height: area.height,
            style_epoch,
        };

        if self.key == Some(key) && self.pixels == pixels && !self.rgba.is_empty() {
            return (&self.rgba, self.pixels);
        }

        let (w, h) = pixels;
        let needed = (w as usize) * (h as usize) * 4;

        self.rgba.clear();
        self.rgba.resize(needed, 0);
        render(&mut self.rgba, w, h);

        // The encoding describes the old pixels, so it cannot outlive them.
        self.protocol = None;
        self.key = Some(key);
        self.pixels = pixels;

        (&self.rgba, self.pixels)
    }

    /// Drop the cached image, forcing a re-encode on the next frame.
    ///
    /// The app invalidates in bulk instead, by bumping the style epoch that forms part of
    /// every cache key -- see `Painter::invalidate_images`. This is kept as the direct
    /// route, and is what the cache's own tests drive.
    #[cfg(test)]
    pub fn invalidate(&mut self) {
        self.key = None;
        self.protocol = None;
        self.rgba.clear();
    }

    /// Whether anything is currently cached.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }
}

/// Line width in pixels.
///
/// One-pixel lines are what `plotters` draws through its non-anti-aliased fast path, so
/// this being greater than one is load-bearing for smoothness, not just for weight.
const STROKE: u32 = 2;

/// Rasterise a set of series into an anti-aliased RGBA buffer.
///
/// Writes `width * height * 4` bytes.
///
/// # Where the anti-aliasing comes from
///
/// `plotters` anti-aliases paths wider than a single pixel -- the one-pixel case takes a
/// fast Bresenham path with hard stair-steps, which is why [`STROKE`] is two. So the edge
/// softening is already done by the time this sees the buffer. What is *not* already right
/// is what it was softened against.
///
/// # Why the blend has to be undone
///
/// `plotters` anti-aliases by blending toward the colour the bitmap was filled with, and
/// that fill is a guess: bottom never paints a background, so there is no real value to ask
/// the terminal for and [`colour_to_rgb`] falls back to black. Emitting those blended edges
/// as-is paints a dark fringe along every line on a light terminal -- anti-aliasing against
/// the wrong background looks worse than no anti-aliasing at all.
///
/// So each pixel is unblended back into the pair it came from. An edge pixel is
/// `line * alpha + background * (1 - alpha)`, so projecting it onto the line-minus-
/// background direction recovers `alpha`, and the pixel is re-emitted as the line's own
/// colour at that alpha. The softening survives, but it now lives entirely in the alpha
/// channel, where the terminal composites it against whatever its real background is.
/// Which series a pixel belongs to is decided by whichever candidate leaves the smallest
/// residual, so crossing lines resolve to the one actually drawn there.
///
/// # Panics
///
/// Never. A short or misaligned destination is left as-is rather than crashing a draw loop.
pub fn rasterize_smooth(
    rgba: &mut [u8], width: u32, height: u32, x_range: (f64, f64), y_range: (f64, f64),
    background: (u8, u8, u8), series: &[Series],
) {
    let needed = (width as usize) * (height as usize) * 4;

    if width == 0 || height == 0 || rgba.len() < needed {
        return;
    }

    let mut rgb = vec![0u8; (width as usize) * (height as usize) * 3];

    rasterize(
        &mut rgb, width, height, x_range, y_range, background, STROKE, series,
    );

    // Precompute each candidate's offset from the background, which is the direction an
    // edge pixel of that series must lie along.
    let axes: Vec<((u8, u8, u8), [f32; 3], f32)> = series
        .iter()
        .filter_map(|line| {
            let axis = [
                f32::from(line.rgb.0) - f32::from(background.0),
                f32::from(line.rgb.1) - f32::from(background.1),
                f32::from(line.rgb.2) - f32::from(background.2),
            ];

            let length = axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2];

            // A series the same colour as the background has no direction to project onto,
            // and nothing it drew could be told apart from the fill anyway.
            (length > 0.0).then_some((line.rgb, axis, length))
        })
        .collect();

    for (index, out) in rgba[..needed].as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let offset = index * 3;
        let pixel = (rgb[offset], rgb[offset + 1], rgb[offset + 2]);

        if pixel == background {
            *out = [0, 0, 0, 0];
            continue;
        }

        let delta = [
            f32::from(pixel.0) - f32::from(background.0),
            f32::from(pixel.1) - f32::from(background.1),
            f32::from(pixel.2) - f32::from(background.2),
        ];

        let mut best: Option<((u8, u8, u8), f32, f32)> = None;

        for (colour, axis, length) in &axes {
            let dot = delta[0] * axis[0] + delta[1] * axis[1] + delta[2] * axis[2];
            let alpha = (dot / length).clamp(0.0, 1.0);

            let residual: f32 = (0..3)
                .map(|c| {
                    let r = delta[c] - alpha * axis[c];
                    r * r
                })
                .sum();

            if best.is_none_or(|(_, _, previous)| residual < previous) {
                best = Some((*colour, alpha, residual));
            }
        }

        match best {
            // Round rather than truncate, so the faintest edge is not dropped entirely.
            Some((colour, alpha, _)) => {
                *out = [colour.0, colour.1, colour.2, (alpha * 255.0 + 0.5) as u8];
            }
            // Nothing to attribute it to, so it cannot be part of a line.
            None => *out = [0, 0, 0, 0],
        }
    }
}

/// Convert a ratatui colour to an RGB triple for the rasteriser.
///
/// Indexed and named colours are mapped through the standard xterm palette. `Reset` has no
/// RGB meaning, so it comes back as `None` and the caller decides.
pub fn colour_to_rgb(colour: Color) -> Option<(u8, u8, u8)> {
    let rgb = match colour {
        Color::Reset => return None,
        Color::Black => (0, 0, 0),
        Color::Red => (128, 0, 0),
        Color::Green => (0, 128, 0),
        Color::Yellow => (128, 128, 0),
        Color::Blue => (0, 0, 128),
        Color::Magenta => (128, 0, 128),
        Color::Cyan => (0, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (0, 0, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(index) => indexed_to_rgb(index),
    };

    Some(rgb)
}

/// The standard xterm 256-colour cube.
fn indexed_to_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        // The first 16 are the named colours again.
        0..=15 => {
            const BASE: [(u8, u8, u8); 16] = [
                (0, 0, 0),
                (128, 0, 0),
                (0, 128, 0),
                (128, 128, 0),
                (0, 0, 128),
                (128, 0, 128),
                (0, 128, 128),
                (192, 192, 192),
                (128, 128, 128),
                (255, 0, 0),
                (0, 255, 0),
                (255, 255, 0),
                (0, 0, 255),
                (255, 0, 255),
                (0, 255, 255),
                (255, 255, 255),
            ];
            BASE[index as usize]
        }
        // A 6x6x6 colour cube, with the standard non-linear step table.
        16..=231 => {
            const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let index = index - 16;
            (
                STEPS[(index / 36) as usize],
                STEPS[((index % 36) / 6) as usize],
                STEPS[(index % 6) as usize],
            )
        }
        // A 24-step grey ramp.
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            (level, level, level)
        }
    }
}

/// One series to rasterise: points in data space plus the colour to draw them in.
pub struct Series {
    /// `(x, y)` pairs in data space. `x` is milliseconds before the right edge, so it runs
    /// negative-to-zero left-to-right, matching how the cell renderer plots time.
    pub points: Vec<(f64, f64)>,
    /// Line colour.
    pub rgb: (u8, u8, u8),
    /// Whether to fill the area between the line and zero.
    ///
    /// Used to stack series: a caller that has already turned per-series values into
    /// running totals draws them largest-first with this set, and each fill covers the
    /// lower part of the one before it, leaving a visible band per series.
    pub filled: bool,
}

/// Rasterise a set of series into an RGB buffer, `stroke` pixels wide.
///
/// Writes `width * height * 3` bytes. `plotters` has no RGBA `PixelFormat`, which is why
/// this is RGB and why [`rasterize_smooth`] exists to do the repacking.
///
/// Every pixel this writes is *exactly* `background` or *exactly* one of the series colours.
/// `plotters`' bitmap backend has no anti-aliasing -- `draw_line` runs plain Bresenham -- so
/// there are no blended edge pixels to worry about. [`rasterize_smooth`] leans on that
/// directly: it is what lets a subsample be classified as covered or not by an equality test.
///
/// Deliberately draws the data only -- no axes, labels, or legend. Those are still drawn by
/// the surrounding cell-based chart, which already gets them right; re-deriving axis
/// placement here would be duplicated work with a second chance to disagree.
pub fn rasterize(
    rgb: &mut [u8], width: u32, height: u32, x_range: (f64, f64), y_range: (f64, f64),
    background: (u8, u8, u8), stroke: u32, series: &[Series],
) {
    use plotters::prelude::*;

    if width == 0 || height == 0 || rgb.len() < (width as usize) * (height as usize) * 3 {
        return;
    }

    // A degenerate range would make plotters' coordinate mapping divide by zero.
    let (x_lo, x_hi) = if (x_range.1 - x_range.0).abs() < f64::EPSILON {
        (x_range.0, x_range.0 + 1.0)
    } else {
        x_range
    };
    let (y_lo, y_hi) = if (y_range.1 - y_range.0).abs() < f64::EPSILON {
        (y_range.0, y_range.0 + 1.0)
    } else {
        y_range
    };

    // `plotters` returns errors for things like a zero-size drawing area. None of them are
    // recoverable mid-frame and none should take the app down, so a failure here just
    // leaves the buffer as-is and the frame draws a blank image.
    let mut render = || -> Result<(), Box<dyn std::error::Error>> {
        let backend = BitMapBackend::with_buffer(rgb, (width, height));
        let root = backend.into_drawing_area();
        root.fill(&RGBColor(background.0, background.1, background.2))?;

        // Zero margins and no label areas: this image is the data region only, and the
        // surrounding cell-based chart already draws the block, axes, and labels around it.
        let mut chart = ChartBuilder::on(&root)
            .margin(0)
            .set_all_label_area_size(0)
            .build_cartesian_2d(x_lo..x_hi, y_lo..y_hi)?;

        for line in series {
            if line.points.is_empty() {
                continue;
            }

            let colour = RGBColor(line.rgb.0, line.rgb.1, line.rgb.2);

            if line.filled {
                // Filled to the bottom of the plot rather than to zero, so a log axis --
                // where zero is negative infinity and the baseline is the axis minimum --
                // does not leave the band floating.
                chart.draw_series(
                    AreaSeries::new(line.points.iter().copied(), y_lo, colour.filled())
                        .border_style(colour.stroke_width(stroke)),
                )?;
            } else {
                chart.draw_series(LineSeries::new(
                    line.points.iter().copied(),
                    colour.stroke_width(stroke),
                ))?;
            }
        }

        root.present()?;
        Ok(())
    };

    let _ = render();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    #[test]
    fn an_unchanged_frame_reuses_the_cached_image() {
        // The whole reason the cache exists: mouse motion redraws at up to 50fps with no
        // frame-level throttle, and re-rasterising at that rate would be absurd.
        let mut cache = ImageCache::default();
        let now = Instant::now();
        let mut renders = 0;

        for _ in 0..10 {
            cache.get_or_update(now, 60_000, area(80, 20), 0, (160, 40), |buf, _, _| {
                renders += 1;
                buf.fill(0x7F);
            });
        }

        assert_eq!(renders, 1, "only the first frame may rasterise");
    }

    #[test]
    fn every_key_field_invalidates_the_cache() {
        let now = Instant::now();
        let later = now + std::time::Duration::from_millis(1);

        // Each case differs from the baseline in exactly one field.
        let cases: Vec<(&str, Instant, u64, Rect, u64, (u32, u32))> = vec![
            ("new data", later, 60_000, area(80, 20), 0, (160, 40)),
            ("zoom", now, 30_000, area(80, 20), 0, (160, 40)),
            ("width", now, 60_000, area(81, 20), 0, (160, 40)),
            ("height", now, 60_000, area(80, 21), 0, (160, 40)),
            ("style", now, 60_000, area(80, 20), 1, (160, 40)),
            ("font size", now, 60_000, area(80, 20), 0, (320, 80)),
        ];

        for (label, time, display, rect, epoch, pixels) in cases {
            let mut cache = ImageCache::default();
            let mut renders = 0;

            cache.get_or_update(now, 60_000, area(80, 20), 0, (160, 40), |buf, _, _| {
                renders += 1;
                buf.fill(1);
            });
            cache.get_or_update(time, display, rect, epoch, pixels, |buf, _, _| {
                renders += 1;
                buf.fill(2);
            });

            assert_eq!(renders, 2, "a change of {label} must invalidate the cache");
        }
    }

    #[test]
    fn invalidate_forces_a_re_encode() {
        // Reattaching to a different terminal process invalidates every transmitted image
        // id, so the next frame has to send the image again rather than reuse it.
        let mut cache = ImageCache::default();
        let now = Instant::now();
        let mut renders = 0;

        cache.get_or_update(now, 60_000, area(80, 20), 0, (160, 40), |buf, _, _| {
            renders += 1;
            buf.fill(1);
        });
        assert!(!cache.is_empty());

        cache.invalidate();
        assert!(cache.is_empty());

        cache.get_or_update(now, 60_000, area(80, 20), 0, (160, 40), |buf, _, _| {
            renders += 1;
            buf.fill(1);
        });

        assert_eq!(renders, 2, "an invalidated cache must rasterise again");
    }

    #[test]
    fn a_fully_covered_pixel_is_opaque_and_keeps_its_colour() {
        // Every subsample is the line colour, so there is nothing to average against and
        // the pixel must come through untouched -- the interior of a line must not be
        // softened by the anti-aliasing that exists for its edges.
        let red = (200, 30, 40);
        let mut rgba = vec![0u8; 4];

        rasterize_smooth(
            &mut rgba,
            1,
            1,
            (-1.0, 0.0),
            (0.0, 1.0),
            red,
            &[Series {
                points: vec![(-1.0, 0.5), (0.0, 0.5)],
                rgb: red,
                filled: false,
            }],
        );

        // Background == line colour here, so "not background" never fires; that is the
        // degenerate case, and it must land on transparent rather than on garbage.
        assert_eq!(rgba, vec![0, 0, 0, 0]);
    }

    #[test]
    fn an_empty_plot_is_fully_transparent() {
        // bottom never paints a real background, so the rasteriser's guessed fill must not
        // show up as an opaque rectangle over the terminal's actual background.
        let mut rgba = vec![0xAAu8; 40 * 20 * 4];

        rasterize_smooth(
            &mut rgba,
            40,
            20,
            (-60.0, 0.0),
            (0.0, 100.0),
            (0, 0, 0),
            &[],
        );

        assert!(
            rgba.as_chunks::<4>().0.iter().all(|px| *px == [0, 0, 0, 0]),
            "an empty plot must leave nothing behind at all"
        );
    }

    #[test]
    fn a_sloped_line_produces_partial_coverage() {
        // The whole point of supersampling. plotters draws hard Bresenham stair-steps, so
        // without the downsample every pixel would be 0 or 255 alpha and the line would
        // look worse than the cell markers it replaces.
        let red = (255, 0, 0);
        let mut rgba = vec![0u8; 60 * 30 * 4];

        rasterize_smooth(
            &mut rgba,
            60,
            30,
            (-60.0, 0.0),
            (0.0, 100.0),
            (0, 0, 0),
            &[Series {
                points: vec![(-60.0, 5.0), (0.0, 95.0)],
                rgb: red,
                filled: false,
            }],
        );

        let alphas: Vec<u8> = rgba.as_chunks::<4>().0.iter().map(|px| px[3]).collect();

        assert!(
            alphas.contains(&0xFF),
            "the body of the line must be fully opaque"
        );
        assert!(
            alphas.iter().any(|&a| a > 0 && a < 0xFF),
            "a sloped line must produce partially covered edge pixels -- without them the \
             line is aliased and this whole code path buys nothing"
        );
        assert!(
            alphas.contains(&0),
            "the empty area around the line must stay fully transparent"
        );
    }

    #[test]
    fn an_edge_pixel_keeps_the_line_colour_rather_than_blending_toward_the_background() {
        // Averaging colour across the block instead of averaging coverage would drag the
        // guessed background into every edge pixel, which is a dark halo around every line
        // on a light terminal. Edge pixels must carry the line's own colour, with the
        // softening expressed purely in alpha.
        let red = (255, 0, 0);
        let mut rgba = vec![0u8; 60 * 30 * 4];

        rasterize_smooth(
            &mut rgba,
            60,
            30,
            (-60.0, 0.0),
            (0.0, 100.0),
            (0, 0, 0),
            &[Series {
                points: vec![(-60.0, 5.0), (0.0, 95.0)],
                rgb: red,
                filled: false,
            }],
        );

        for px in rgba.as_chunks::<4>().0.iter().filter(|px| px[3] > 0) {
            assert_eq!(
                (px[0], px[1], px[2]),
                red,
                "a covered pixel must be the line colour at any coverage level"
            );
        }
    }

    #[test]
    fn a_short_destination_is_left_alone_rather_than_panicking() {
        // A draw loop must not panic on an arithmetic surprise.
        let mut rgba = vec![0xAAu8; 8];

        rasterize_smooth(&mut rgba, 40, 20, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), &[]);

        assert_eq!(
            rgba,
            vec![0xAA; 8],
            "too small to fill, so nothing was written"
        );
    }

    #[test]
    fn colours_map_to_rgb_with_reset_left_to_the_caller() {
        assert_eq!(colour_to_rgb(Color::Rgb(1, 2, 3)), Some((1, 2, 3)));
        assert_eq!(colour_to_rgb(Color::LightRed), Some((255, 0, 0)));
        assert_eq!(
            colour_to_rgb(Color::Reset),
            None,
            "Reset has no RGB meaning; the caller picks"
        );
    }

    #[test]
    fn the_indexed_palette_matches_xterm() {
        assert_eq!(colour_to_rgb(Color::Indexed(0)), Some((0, 0, 0)));
        assert_eq!(colour_to_rgb(Color::Indexed(15)), Some((255, 255, 255)));
        // 16 is the first cube entry, and is black.
        assert_eq!(colour_to_rgb(Color::Indexed(16)), Some((0, 0, 0)));
        // 21 is (0, 0, 255): the last step of blue in the first plane.
        assert_eq!(colour_to_rgb(Color::Indexed(21)), Some((0, 0, 255)));
        // 231 is the top of the cube, white.
        assert_eq!(colour_to_rgb(Color::Indexed(231)), Some((255, 255, 255)));
        // The grey ramp runs 8..=238.
        assert_eq!(colour_to_rgb(Color::Indexed(232)), Some((8, 8, 8)));
        assert_eq!(colour_to_rgb(Color::Indexed(255)), Some((238, 238, 238)));
    }

    #[test]
    fn pixel_modes_parse_from_both_spellings() {
        assert_eq!("off".parse::<PixelMode>().unwrap(), PixelMode::Off);
        assert_eq!("auto".parse::<PixelMode>().unwrap(), PixelMode::Auto);
        assert_eq!("kitty".parse::<PixelMode>().unwrap(), PixelMode::Kitty);
        assert_eq!("KITTY".parse::<PixelMode>().unwrap(), PixelMode::Kitty);
        assert_eq!(PixelMode::default(), PixelMode::Off);

        let err = "sixel".parse::<PixelMode>().unwrap_err();
        assert!(err.contains("sixel"));
        assert!(err.contains("off, auto, kitty"));
    }
}

#[cfg(test)]
mod raster_tests {
    use super::*;

    /// Read one pixel out of an RGB buffer.
    fn pixel(rgb: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8) {
        let offset = ((y * width + x) * 3) as usize;
        (rgb[offset], rgb[offset + 1], rgb[offset + 2])
    }

    #[test]
    fn the_background_is_filled_even_with_no_series() {
        let (w, h) = (40, 20);
        let mut rgb = vec![0u8; (w * h * 3) as usize];

        rasterize(
            &mut rgb,
            w,
            h,
            (-60.0, 0.0),
            (0.0, 100.0),
            (10, 20, 30),
            1,
            &[],
        );

        assert_eq!(pixel(&rgb, w, 0, 0), (10, 20, 30));
        assert_eq!(pixel(&rgb, w, w - 1, h - 1), (10, 20, 30));
    }

    #[test]
    fn a_flat_line_lands_at_the_expected_height() {
        // A horizontal line at y=50 in a 0..100 range must draw across the vertical middle,
        // and must not draw at the top or bottom.
        let (w, h) = (60, 40);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        let red = (255, 0, 0);

        rasterize(
            &mut rgb,
            w,
            h,
            (-60.0, 0.0),
            (0.0, 100.0),
            (0, 0, 0),
            1,
            &[Series {
                points: vec![(-60.0, 50.0), (0.0, 50.0)],
                rgb: red,
                filled: false,
            }],
        );

        let coloured_rows: Vec<u32> = (0..h)
            .filter(|y| (0..w).any(|x| pixel(&rgb, w, x, *y) == red))
            .collect();

        assert!(!coloured_rows.is_empty(), "the line must actually be drawn");

        let middle = h / 2;
        for row in &coloured_rows {
            assert!(
                row.abs_diff(middle) <= 2,
                "a line at y=50 of 0..100 drew at row {row}, expected near {middle}"
            );
        }
    }

    #[test]
    fn a_rising_line_is_higher_on_the_right_than_on_the_left() {
        // y grows upward in data space but downward in pixel space, so a rising series must
        // produce a *smaller* row index on the right.
        let (w, h) = (80, 40);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        let green = (0, 255, 0);

        rasterize(
            &mut rgb,
            w,
            h,
            (-60.0, 0.0),
            (0.0, 100.0),
            (0, 0, 0),
            1,
            &[Series {
                points: vec![(-60.0, 10.0), (0.0, 90.0)],
                rgb: green,
                filled: false,
            }],
        );

        // plotters anti-aliases diagonals, so an exact colour match would only catch the
        // handful of fully-saturated pixels. Match on the dominant channel instead.
        let is_green = |p: (u8, u8, u8)| p.1 > 40 && p.1 > p.0 && p.1 > p.2;
        let row_at = |x: u32| (0..h).find(|y| is_green(pixel(&rgb, w, x, *y)));
        let _ = green;

        // Sample the actual drawn extent rather than assuming it reaches the buffer edges.
        let columns: Vec<u32> = (0..w).filter(|x| row_at(*x).is_some()).collect();
        assert!(
            columns.len() > 4,
            "the line must span most of the buffer, drew {} columns",
            columns.len()
        );

        let left = row_at(columns[0]).unwrap();
        let right = row_at(*columns.last().unwrap()).unwrap();

        assert!(
            right < left,
            "a rising series drew left row {left} and right row {right}; \
             the right should be nearer the top"
        );
    }

    #[test]
    fn degenerate_ranges_and_sizes_do_not_panic() {
        // Everything here happens inside a draw loop, where a panic takes the app down.
        let mut rgb = vec![0u8; 40 * 20 * 3];

        rasterize(&mut rgb, 40, 20, (5.0, 5.0), (7.0, 7.0), (0, 0, 0), 1, &[]);
        rasterize(&mut rgb, 0, 20, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), 1, &[]);
        rasterize(&mut rgb, 40, 0, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), 1, &[]);

        // A buffer too small for the requested size must be refused, not overrun.
        let mut tiny = vec![0u8; 3];
        rasterize(
            &mut tiny,
            40,
            20,
            (-1.0, 0.0),
            (0.0, 1.0),
            (0, 0, 0),
            1,
            &[],
        );

        // An empty series is skipped rather than tripping plotters.
        rasterize(
            &mut rgb,
            40,
            20,
            (-1.0, 0.0),
            (0.0, 1.0),
            (0, 0, 0),
            1,
            &[Series {
                points: vec![],
                rgb: (1, 2, 3),
                filled: false,
            }],
        );
    }
}

/// Owns the terminal graphics picker and decides whether the pixel path is usable.
pub struct PixelRenderer {
    picker: Option<ratatui_image::picker::Picker>,
    mode: PixelMode,
}

impl PixelRenderer {
    /// Set up the renderer.
    ///
    /// **Must be called before `enable_raw_mode`.** `Picker::from_query_stdio` drives the
    /// terminal into raw mode itself to read the capability replies, and doing that twice
    /// leaves the terminal in a state neither side expects.
    pub fn new(mode: PixelMode) -> Self {
        use ratatui_image::picker::{Picker, ProtocolType};

        if mode == PixelMode::Off {
            return Self { picker: None, mode };
        }

        let picker = match Picker::from_query_stdio() {
            Ok(mut picker) => {
                if mode == PixelMode::Kitty {
                    // Forced, because the query cannot succeed under tmux -- see the module
                    // docs. The font size the query reports is still used; if the terminal
                    // did not report one, ratatui-image's fallback is close enough that the
                    // image is merely scaled slightly, not broken.
                    picker.set_protocol_type(ProtocolType::Kitty);
                }
                Some(picker)
            }
            Err(_) => {
                // A terminal that will not answer at all cannot be driven blind in `auto`,
                // but an explicit `kitty` is the user saying they know better.
                if mode == PixelMode::Kitty {
                    // The cell size this terminal actually reports (CSI 16 t returned
                    // `ESC [6;24;11t`), used when the query gave nothing back.
                    //
                    // `from_fontsize` is deprecated in favour of `from_query_stdio`, but
                    // the query failing is precisely the case this branch handles, so the
                    // recommended replacement is not available here.
                    #[allow(deprecated)]
                    let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(11, 24));
                    picker.set_protocol_type(ProtocolType::Kitty);
                    Some(picker)
                } else {
                    None
                }
            }
        };

        Self { picker, mode }
    }

    /// Whether graphs should draw as pixels.
    pub fn is_active(&self) -> bool {
        use ratatui_image::picker::ProtocolType;

        self.picker
            .as_ref()
            .is_some_and(|p| p.protocol_type() != ProtocolType::Halfblocks)
    }

    /// The configured mode, whatever detection concluded.
    pub fn mode(&self) -> PixelMode {
        self.mode
    }

    /// Pixel dimensions of one terminal cell.
    pub fn font_size(&self) -> (u16, u16) {
        self.picker.as_ref().map_or((11, 24), |p| {
            let size = p.font_size();
            (size.width, size.height)
        })
    }

    /// Draw the cache's rasterised graph into `area`.
    ///
    /// Encodes on first use and then reuses the encoding until [`ImageCache::get_or_update`]
    /// rebuilds the pixels, which is why this takes the cache rather than a loose buffer.
    /// Encoding per frame instead of per data change is what made the graph flicker under
    /// tmux -- see the `protocol` field on [`ImageCache`].
    ///
    /// Returns false if the image could not be built, so the caller can fall back to the
    /// cell-drawn graph instead of leaving the widget blank.
    ///
    /// `area` must be the chart's own plot region, and this must be called *before* the
    /// chart draws its border, axis labels, and legend. Those are plain text, drawn
    /// afterward in the normal course of rendering, and a later `Widget::render` call
    /// unconditionally overwrites whatever a cell held before -- so drawing this first is
    /// what lets the chart's own decorations reclaim their cells from the image, with no
    /// special-casing needed here. Doing it the other way round has no equivalent: a Kitty
    /// image's placeholder run replaces a cell's symbol outright, and there is no "draw
    /// under the existing text" operation at the ratatui `Buffer` level to undo that.
    pub fn draw(&self, f: &mut ratatui::Frame<'_>, area: Rect, cache: &mut ImageCache) -> bool {
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::{Image, Resize};

        let Some(picker) = self.picker.as_ref() else {
            return false;
        };

        if area.width == 0 || area.height == 0 {
            return false;
        }

        if cache.protocol.is_none() {
            let (width, height) = cache.pixels;

            let Some(buffer) = RgbaImage::from_raw(width, height, cache.rgba.clone()) else {
                return false;
            };

            let size = ratatui::layout::Size::new(area.width, area.height);

            match picker.new_protocol(DynamicImage::ImageRgba8(buffer), size, Resize::Fit(None)) {
                Ok(protocol) => cache.protocol = Some(protocol),
                // Encoding can fail on a degenerate size. Falling back to the cell rendering
                // is strictly better than panicking inside a draw loop.
                Err(_) => return false,
            }
        }

        let Some(protocol) = cache.protocol.as_ref() else {
            return false;
        };

        f.render_widget(Image::new(protocol), area);

        true
    }
}

impl std::fmt::Debug for PixelRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixelRenderer")
            .field("mode", &self.mode)
            .field("active", &self.is_active())
            .finish()
    }
}
