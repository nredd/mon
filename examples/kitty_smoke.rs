//! Minimal Kitty-protocol render smoke test, independent of any chart/legend code.
//!
//! Draws a plain red square via the exact same `Picker`/`Image` calls `PixelRenderer::draw`
//! uses, with nothing else on screen. If this doesn't show a red square, the bug is in
//! `ratatui-image`'s Kitty path or this terminal, not in `mon`'s chart-compositing code.
//!
//! ```console
//! $ cargo run --release --example kitty_smoke
//! ```

use std::io::stdout;
use std::time::Duration;

use image::{DynamicImage, RgbaImage};
use ratatui::{Terminal, backend::CrosstermBackend, layout::Rect};
use ratatui_image::{
    Image, Resize,
    picker::{Picker, ProtocolType},
};

fn main() -> std::io::Result<()> {
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| {
        #[allow(deprecated)]
        Picker::from_fontsize(ratatui_image::FontSize::new(11, 24))
    });
    picker.set_protocol_type(ProtocolType::Kitty);

    let (w, h) = (200u32, 100u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for chunk in rgba.as_chunks_mut::<4>().0 {
        chunk[0] = 255; // R
        chunk[1] = 0; // G
        chunk[2] = 0; // B
        chunk[3] = 255; // A
    }
    let buffer = RgbaImage::from_raw(w, h, rgba).expect("buffer sized correctly");

    crossterm::terminal::enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    let area = Rect::new(0, 0, 20, 10);
    let size = ratatui::layout::Size::new(area.width, area.height);
    let protocol = picker
        .new_protocol(DynamicImage::ImageRgba8(buffer), size, Resize::Fit(None))
        .expect("encode should succeed for a plain 200x100 RGBA square");

    for _ in 0..5 {
        terminal.draw(|f| {
            f.render_widget(Image::new(&protocol), area);
        })?;
        std::thread::sleep(Duration::from_secs(1));
    }

    crossterm::terminal::disable_raw_mode()?;
    println!("done -- did you see a red square in the top-left?");
    Ok(())
}
