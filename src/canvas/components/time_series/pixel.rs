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
//! 50fps. Re-rasterising and re-encoding a PNG at that rate would be absurd, so
//! [`ImageCache`] keys on everything that can change what the image looks like and re-runs
//! only when one of them moves.

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

/// A rasterised graph, retained between frames.
#[derive(Default)]
pub struct ImageCache {
    key: Option<CacheKey>,
    /// RGBA8, row-major, `width * height * 4` bytes.
    rgba: Vec<u8>,
    /// Pixel dimensions of `rgba`.
    pixels: (u32, u32),
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
        self.rgba.clear();
    }

    /// Whether anything is currently cached.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }
}

/// Repack an RGB buffer into RGBA, chroma-keying `background` to fully transparent.
///
/// `plotters` has no RGBA `PixelFormat` -- `BitMapBackend::with_buffer` writes 3 bytes per
/// pixel -- while `image`'s `RgbaImage` wants 4. That gap is the reason this exists at all.
///
/// The transparency is deliberate, not incidental. bottom never paints an actual background
/// color anywhere -- every widget just leaves cells at `Color::Reset` and lets the
/// terminal's own background show through, which is how the app stays theme-neutral. There
/// is no real RGB value to ask the terminal for, so the rasteriser has to guess one to fill
/// the bitmap with (see `colour_to_rgb`'s `Reset -> None -> black` fallback in the caller).
/// A guessed background painted opaque shows up as a wrong-colored rectangle sitting on top
/// of the terminal's actual background. Keying it transparent instead means a plain guess
/// is harmless: the guess never actually appears, and the real terminal background glows
/// through underneath the line exactly as it does everywhere else in the UI.
///
/// This is an exact match, not a tolerance/threshold match. `plotters` anti-aliases line
/// edges by blending toward the fill color it was given, so an edge pixel is a distinct RGB
/// value from a pure background pixel and correctly stays opaque -- keying only the exact
/// background leaves the anti-aliased halo intact, which is what makes the line's edges
/// look smooth instead of jagged-and-cut-out.
///
/// # Panics
///
/// Never. A short or misaligned source simply produces transparent black for the pixels it
/// cannot fill, which is invisible rather than a crash in a draw loop.
pub fn rgb_to_rgba(rgb: &[u8], rgba: &mut [u8], background: (u8, u8, u8)) {
    for (index, chunk) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let offset = index * 3;

        let pixel = if offset + 2 < rgb.len() {
            (rgb[offset], rgb[offset + 1], rgb[offset + 2])
        } else {
            (0, 0, 0)
        };

        chunk[0] = pixel.0;
        chunk[1] = pixel.1;
        chunk[2] = pixel.2;
        chunk[3] = if pixel == background { 0x00 } else { 0xFF };
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
}

/// Rasterise a set of series into an RGB buffer.
///
/// Writes `width * height * 3` bytes. The caller repacks to RGBA with [`rgb_to_rgba`],
/// since `plotters` has no RGBA `PixelFormat`.
///
/// Deliberately draws the data only -- no axes, labels, or legend. Those are still drawn by
/// the surrounding cell-based chart, which already gets them right; re-deriving axis
/// placement here would be duplicated work with a second chance to disagree.
pub fn rasterize(
    rgb: &mut [u8], width: u32, height: u32, x_range: (f64, f64), y_range: (f64, f64),
    background: (u8, u8, u8), series: &[Series],
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
            chart.draw_series(LineSeries::new(line.points.iter().copied(), colour))?;
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
    fn non_background_pixels_repack_opaque() {
        // plotters has no RGBA PixelFormat, so this bridge is unavoidable.
        let rgb = [1, 2, 3, 4, 5, 6];
        let mut rgba = [0u8; 8];

        rgb_to_rgba(&rgb, &mut rgba, (0, 0, 0));

        assert_eq!(rgba, [1, 2, 3, 0xFF, 4, 5, 6, 0xFF]);
    }

    #[test]
    fn background_pixels_key_out_to_fully_transparent() {
        // The whole point: bottom never paints a real background, so the rasteriser's
        // guessed fill must not show up as an opaque rectangle. It has to let the
        // terminal's actual background glow through instead.
        let rgb = [10, 20, 30, 10, 20, 30, 99, 20, 30];
        let mut rgba = [0u8; 12];

        rgb_to_rgba(&rgb, &mut rgba, (10, 20, 30));

        assert_eq!(
            rgba[3], 0x00,
            "an exact background pixel must be transparent"
        );
        assert_eq!(rgba[7], 0x00, "every background pixel, not just the first");
        assert_eq!(
            rgba[11], 0xFF,
            "a pixel that merely resembles the background must stay opaque -- this is what \
             keeps anti-aliased line edges smooth instead of jagged"
        );
    }

    #[test]
    fn a_short_source_yields_transparent_rather_than_panicking() {
        // A draw loop must not panic on an arithmetic surprise.
        let rgb = [9, 9, 9];
        let mut rgba = [0xAAu8; 8];

        rgb_to_rgba(&rgb, &mut rgba, (0, 0, 0));

        assert_eq!(&rgba[0..4], &[9, 9, 9, 0xFF]);
        assert_eq!(
            &rgba[4..8],
            &[0, 0, 0, 0x00],
            "the unfilled pixel defaults to black, which is also this test's background, \
             so it must key out transparent rather than show as a visible artifact"
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
            &[Series {
                points: vec![(-60.0, 50.0), (0.0, 50.0)],
                rgb: red,
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
            &[Series {
                points: vec![(-60.0, 10.0), (0.0, 90.0)],
                rgb: green,
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

        rasterize(&mut rgb, 40, 20, (5.0, 5.0), (7.0, 7.0), (0, 0, 0), &[]);
        rasterize(&mut rgb, 0, 20, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), &[]);
        rasterize(&mut rgb, 40, 0, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), &[]);

        // A buffer too small for the requested size must be refused, not overrun.
        let mut tiny = vec![0u8; 3];
        rasterize(&mut tiny, 40, 20, (-1.0, 0.0), (0.0, 1.0), (0, 0, 0), &[]);

        // An empty series is skipped rather than tripping plotters.
        rasterize(
            &mut rgb,
            40,
            20,
            (-1.0, 0.0),
            (0.0, 1.0),
            (0, 0, 0),
            &[Series {
                points: vec![],
                rgb: (1, 2, 3),
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

    /// Draw an RGBA buffer into `area`.
    ///
    /// Returns false if the image could not be built, so the caller can fall back to the
    /// cell-drawn graph instead of leaving the widget blank.
    ///
    /// Must be called *before* the surrounding chart draws its border, axis labels, and
    /// legend. Those are plain text, drawn afterward in the normal course of rendering, and
    /// a later `Widget::render` call unconditionally overwrites whatever a cell held before
    /// -- so drawing this first is what lets the chart's own decorations naturally reclaim
    /// their cells from the image, with no special-casing needed here. Doing it the other
    /// way round doesn't have an equivalent: a Kitty image's placeholder run replaces a
    /// cell's symbol outright, and there is no "draw under the existing text" operation at
    /// the ratatui `Buffer` level to undo that after the fact.
    pub fn draw(
        &self, f: &mut ratatui::Frame<'_>, area: Rect, rgba: &[u8], pixels: (u32, u32),
    ) -> bool {
        use image::{DynamicImage, RgbaImage};
        use ratatui_image::{Image, Resize};

        let Some(picker) = self.picker.as_ref() else {
            return false;
        };

        if area.width == 0 || area.height == 0 {
            return false;
        }

        let Some(buffer) = RgbaImage::from_raw(pixels.0, pixels.1, rgba.to_vec()) else {
            return false;
        };

        let size = ratatui::layout::Size::new(area.width, area.height);

        match picker.new_protocol(DynamicImage::ImageRgba8(buffer), size, Resize::Fit(None)) {
            Ok(protocol) => {
                f.render_widget(Image::new(&protocol), area);
                true
            }
            // Encoding can fail on a degenerate size. Falling back to the cell rendering
            // is strictly better than panicking inside a draw loop.
            Err(_) => false,
        }
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
