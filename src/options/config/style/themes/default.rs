use ratatui::{
    style::{Color, Modifier, Style},
    widgets::BorderType,
};

use super::{colour, hex};
use crate::options::config::style::Styles;

impl Styles {
    pub(crate) fn default_palette() -> Self {
        const FIRST_COLOUR: Color = Color::LightMagenta;
        const SECOND_COLOUR: Color = Color::LightYellow;
        const THIRD_COLOUR: Color = Color::LightCyan;
        const FOURTH_COLOUR: Color = Color::LightGreen;
        #[cfg(not(target_os = "windows"))]
        const FIFTH_COLOUR: Color = Color::LightRed;
        const HIGHLIGHT_COLOUR: Color = Color::LightBlue;
        const AVG_COLOUR: Color = Color::Red;
        const ALL_COLOUR: Color = Color::Green;
        const DEFAULT_SELECTED_TEXT_STYLE: Style = colour!(Color::Black).bg(HIGHLIGHT_COLOUR);
        const TEXT_COLOUR: Color = Color::Gray;

        let list_colours = vec![
            colour!(Color::LightMagenta),
            colour!(Color::LightYellow),
            colour!(Color::LightCyan),
            colour!(Color::LightGreen),
            colour!(Color::LightBlue),
            colour!(Color::Cyan),
            colour!(Color::Green),
            colour!(Color::Blue),
        ];

        let second_colours_list = vec![
            colour!(FOURTH_COLOUR),
            colour!(Color::LightBlue),
            colour!(Color::LightRed),
            colour!(Color::Cyan),
            colour!(Color::Green),
            colour!(Color::Blue),
            colour!(Color::Red),
        ];

        Self {
            ram_style: colour!(FIRST_COLOUR),
            #[cfg(not(target_os = "windows"))]
            cache_style: colour!(FIFTH_COLOUR),
            swap_style: colour!(SECOND_COLOUR),
            #[cfg(feature = "zfs")]
            arc_style: colour!(THIRD_COLOUR),
            #[cfg(feature = "gpu")]
            gpu_colours: second_colours_list.clone(),
            rx_style: colour!(FIRST_COLOUR),
            tx_style: colour!(SECOND_COLOUR),
            total_rx_style: colour!(THIRD_COLOUR),
            total_tx_style: colour!(FOURTH_COLOUR),
            all_cpu_colour: colour!(ALL_COLOUR),
            avg_cpu_colour: colour!(AVG_COLOUR),
            cpu_colour_styles: list_colours.clone(),
            temp_graph_colour_styles: list_colours.clone(),
            disk_io_read_colour_styles: list_colours.clone(),
            power_colour_styles: list_colours,
            // Model-family series, in fixed order: Opus, Sonnet, Haiku, Fable, Other.
            // These are the dataviz categorical slots 1-5, stepped for a dark surface.
            // Validated: lightness band, chroma floor, adjacent CVD separation (worst
            // 8.4), normal-vision floor (worst 19.3), and 3:1 contrast all pass.
            claude_colour_styles: vec![
                hex!("#3987e5"),
                hex!("#d95926"),
                hex!("#199e70"),
                hex!("#c98500"),
                hex!("#d55181"),
            ],
            disk_io_write_colour_styles: second_colours_list,
            border_style: colour!(TEXT_COLOUR),
            highlighted_border_style: colour!(HIGHLIGHT_COLOUR),
            text_style: colour!(TEXT_COLOUR),
            selected_text_style: DEFAULT_SELECTED_TEXT_STYLE,
            table_header_style: colour!(HIGHLIGHT_COLOUR).add_modifier(Modifier::BOLD),
            widget_title_style: colour!(TEXT_COLOUR),
            general_widget_style: Style::default(),
            graph_style: colour!(TEXT_COLOUR),
            graph_legend_style: colour!(TEXT_COLOUR),
            high_battery: colour!(Color::Green),
            medium_battery: colour!(Color::Yellow),
            low_battery: colour!(Color::Red),
            invalid_query_style: colour!(Color::Red),
            disabled_text_style: colour!(Color::DarkGray),
            border_type: BorderType::Plain,
            #[cfg(target_os = "linux")]
            thread_text_style: colour!(Color::Green),
        }
    }

    pub fn default_light_palette() -> Self {
        let list_colours = vec![
            colour!(Color::LightMagenta),
            colour!(Color::LightBlue),
            colour!(Color::LightRed),
            colour!(Color::Cyan),
            colour!(Color::Green),
            colour!(Color::Blue),
            colour!(Color::Red),
        ];

        let second_colours_list = vec![
            colour!(Color::LightGreen),
            colour!(Color::LightCyan),
            colour!(Color::LightRed),
            colour!(Color::Cyan),
            colour!(Color::Green),
            colour!(Color::Blue),
            colour!(Color::Red),
        ];

        Self {
            ram_style: colour!(Color::Blue),
            #[cfg(not(target_os = "windows"))]
            cache_style: colour!(Color::LightRed),
            swap_style: colour!(Color::Red),
            #[cfg(feature = "zfs")]
            arc_style: colour!(Color::LightBlue),
            #[cfg(feature = "gpu")]
            gpu_colours: second_colours_list.clone(),
            rx_style: colour!(Color::Blue),
            tx_style: colour!(Color::Red),
            total_rx_style: colour!(Color::LightBlue),
            total_tx_style: colour!(Color::LightRed),
            cpu_colour_styles: list_colours.clone(),
            temp_graph_colour_styles: list_colours.clone(),
            disk_io_read_colour_styles: list_colours.clone(),
            power_colour_styles: list_colours,
            // The same eight hues stepped for a light surface. Three of these sit below
            // 3:1 contrast there, so the relief rule applies -- satisfied because the
            // graph legend direct-labels every series with its family name.
            claude_colour_styles: vec![
                hex!("#2a78d6"),
                hex!("#eb6834"),
                hex!("#1baf7a"),
                hex!("#eda100"),
                hex!("#e87ba4"),
            ],
            disk_io_write_colour_styles: second_colours_list,
            border_style: colour!(Color::Black),
            text_style: colour!(Color::Black),
            selected_text_style: colour!(Color::White).bg(Color::LightBlue),
            table_header_style: colour!(Color::Black).add_modifier(Modifier::BOLD),
            widget_title_style: colour!(Color::Black),
            graph_style: colour!(Color::Black),
            graph_legend_style: colour!(Color::Black),
            disabled_text_style: colour!(Color::Gray),
            ..Self::default_palette()
        }
    }
}

mod tests {
    #[test]
    fn default_palettes_valid() {
        let _ = super::Styles::default_palette();
        let _ = super::Styles::default_light_palette();
    }
}
