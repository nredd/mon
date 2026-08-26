//! Code related to drawing.
//!
//! Note that eventually this should not contain any widget-specific draw code,
//! but rather just generic code or components.

pub mod components;
pub mod dialogs;
mod drawing_utils;
mod widgets;

use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Constraint, Direction, Flex, Layout, Position, Rect},
    style::Style,
    symbols::Marker,
    text::Span,
    widgets::Paragraph,
};

use crate::{
    app::{
        App,
        layout_manager::{BottomColRow, BottomLayout, BottomWidgetType},
    },
    constants::*,
    options::config::style::Styles,
};
use crate::{
    canvas::components::time_series::pixel::PixelRenderer, options::config::flags::GraphMarker,
};

/// Handles the canvas' state.
pub struct Painter {
    pub styles: Styles,

    /// Used to know whether to invalidate things.
    previous_height: u16,

    /// Used to know whether to invalidate things.
    previous_width: u16,

    /// The layout.
    layout: BottomLayout,

    /// Terminal graphics renderer for the pixel path.
    pixel: PixelRenderer,

    /// Bumped whenever something happens that makes every cached image stale -- a resize,
    /// or a reattach to a different terminal process, which invalidates transmitted image
    /// ids and would otherwise leave blank cells.
    style_epoch: u64,
}

impl Painter {
    pub fn init(
        layout: BottomLayout, styling: Styles, pixel: PixelRenderer,
    ) -> anyhow::Result<Self> {
        let painter = Painter {
            styles: styling,
            previous_height: 0,
            previous_width: 0,
            layout,
            pixel,
            style_epoch: 0,
        };

        Ok(painter)
    }

    /// The graphics renderer, if the pixel path is usable.
    pub(crate) fn pixel_renderer(&self) -> Option<&PixelRenderer> {
        self.pixel.is_active().then_some(&self.pixel)
    }

    /// The current image-cache epoch.
    pub(crate) fn style_epoch(&self) -> u64 {
        self.style_epoch
    }

    /// Invalidate every cached image.
    pub(crate) fn invalidate_images(&mut self) {
        self.style_epoch = self.style_epoch.wrapping_add(1);
    }

    /// Ask the terminal to drop every image it is holding.
    ///
    /// Smuggled through a cell symbol rather than written to stdout directly, which is the
    /// same mechanism `ratatui-image` uses to emit graphics escapes: ratatui writes cell
    /// symbols to the terminal verbatim, and writing to stdout mid-frame would interleave
    /// with ratatui's own output.
    fn delete_images(&self, f: &mut Frame<'_>) {
        if !self.pixel.is_active() {
            return;
        }

        // `a=d,d=A` is "delete all placements and free the image data".
        const DELETE_ALL: &str = "\x1b_Ga=d,d=A\x1b\\";

        let area = f.area();
        if area.width == 0 || area.height == 0 {
            return;
        }

        if let Some(cell) = f.buffer_mut().cell_mut((area.x, area.y)) {
            cell.set_symbol(DELETE_ALL);
        }
    }

    /// Determines the border style.
    pub fn get_border_style(&self, widget_id: u64, selected_widget_id: u64) -> Style {
        let is_on_widget = widget_id == selected_widget_id;
        if is_on_widget {
            self.styles.highlighted_border_style
        } else {
            self.styles.border_style
        }
    }

    /// Map the configured marker onto ratatui's.
    ///
    /// Every arm here was already implemented by the vendored chart renderer; only
    /// braille and dot were previously reachable.
    pub(crate) fn get_marker(&self, marker: GraphMarker) -> Marker {
        match marker {
            GraphMarker::Braille => Marker::Braille,
            GraphMarker::Dot => Marker::Dot,
            GraphMarker::Block => Marker::Block,
            GraphMarker::Bar => Marker::Bar,
            GraphMarker::HalfBlock => Marker::HalfBlock,
            GraphMarker::Quadrant => Marker::Quadrant,
            GraphMarker::Sextant => Marker::Sextant,
            GraphMarker::Octant => Marker::Octant,
        }
    }

    fn draw_frozen_indicator(&self, f: &mut Frame<'_>, draw_loc: Rect) {
        f.render_widget(
            Paragraph::new(Span::styled(
                "Frozen, press 'f' to unfreeze",
                self.styles.selected_text_style,
            )),
            Layout::default()
                .horizontal_margin(1)
                .constraints([Constraint::Length(1)])
                .split(draw_loc)[0],
        )
    }

    pub fn draw_data<B: Backend>(
        &mut self, terminal: &mut Terminal<B>, app_state: &mut App,
    ) -> Result<(), B::Error> {
        use BottomWidgetType::*;

        terminal.draw(|f| {
            let (terminal_size, frozen_draw_loc) = if app_state.data_store.is_frozen() {
                // TODO: Remove built-in cache?
                let split_loc = Layout::default()
                    .constraints([Constraint::Min(0), Constraint::Length(1)])
                    .split(f.area());
                (split_loc[0], Some(split_loc[1]))
            } else {
                (f.area(), None)
            };
            let terminal_height = terminal_size.height;
            let terminal_width = terminal_size.width;

            if (self.previous_height == 0 && self.previous_width == 0)
                || (self.previous_height != terminal_height
                    || self.previous_width != terminal_width)
            {
                app_state.is_force_redraw = true;
                self.previous_height = terminal_height;
                self.previous_width = terminal_width;

                // A resize changes every image's cell footprint, and a reattach lands here
                // too -- the new terminal process knows nothing about image ids the old one
                // was sent, and reusing them leaves blank cells.
                self.invalidate_images();
            }

            // TODO: We should probably remove this or make it done elsewhere, not the
            // responsibility of the app.
            if app_state.should_get_widget_bounds() {
                // If we're force drawing, reset ALL mouse boundaries.
                for widget in app_state.widget_map.values_mut() {
                    widget.top_left_corner = None;
                    widget.bottom_right_corner = None;
                }

                // Reset process kill dialog button locations...
                app_state.process_kill_dialog.handle_redraw();

                // Reset battery dialog button locations...
                for battery_widget in app_state.states.battery_state.widget_states.values_mut() {
                    battery_widget.tab_click_locs = None;
                }
            }

            // A dialog covers the graphs, but `set_style` below only repaints *cells*. A
            // Kitty image lives in the terminal's graphics layer and would show straight
            // through it, so it has to be deleted explicitly.
            if app_state.help_dialog_state.is_showing_help
                || app_state.process_kill_dialog.is_open()
            {
                self.delete_images(f);
            }

            // TODO: Make drawing dialog generic.
            if app_state.help_dialog_state.is_showing_help {
                let area = f.area();
                f.buffer_mut()
                    .set_style(area, self.styles.general_widget_style);

                let gen_help_len = GENERAL_HELP_TEXT.len() as u16 + 3;
                let border_len = terminal_height.saturating_sub(gen_help_len) / 2;
                let [_, vertical_dialog_chunk, _] = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(border_len),
                        Constraint::Length(gen_help_len),
                        Constraint::Length(border_len),
                    ])
                    .areas(terminal_size);

                // An approximate proxy for the max line length to use.
                const MAX_TEXT_LENGTH: u16 = const {
                    let mut max = 0;

                    let mut i = 0;
                    while i < HELP_TEXT.len() {
                        let section = HELP_TEXT[i];
                        let mut j = 0;
                        while j < section.len() {
                            let line = section[j];
                            if line.len() > max {
                                max = line.len();
                            }

                            j += 1;
                        }

                        i += 1;
                    }

                    max as u16
                };

                let dialog_width = vertical_dialog_chunk.width;
                let [middle_dialog_chunk] = if dialog_width < MAX_TEXT_LENGTH {
                    Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(100)])
                        .areas(vertical_dialog_chunk)
                } else {
                    // We calculate this so that the margins never have to split an odd number.
                    let len = if (dialog_width.saturating_sub(MAX_TEXT_LENGTH)) % 2 == 0 {
                        MAX_TEXT_LENGTH
                    } else {
                        // It can only be 1 if the difference is greater than 1, so this is fine.
                        MAX_TEXT_LENGTH + 1
                    };

                    Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Length(len)])
                        .flex(Flex::SpaceAround)
                        .areas(vertical_dialog_chunk)
                };

                self.draw_help_dialog(f, app_state, middle_dialog_chunk);
            } else if app_state.process_kill_dialog.is_open() {
                let area = f.area();
                f.buffer_mut()
                    .set_style(area, self.styles.general_widget_style);

                // FIXME: For width, just limit to a max size or full width. For height, not
                // sure. Maybe pass max and let child handle?
                let horizontal_padding = if terminal_width < 100 { 0 } else { 5 };
                let vertical_padding = if terminal_height < 100 { 0 } else { 5 };

                let vertical_dialog_chunk = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(vertical_padding),
                        Constraint::Fill(1),
                        Constraint::Length(vertical_padding),
                    ])
                    .areas::<3>(terminal_size)[1];

                let dialog_draw_area = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([
                        Constraint::Length(horizontal_padding),
                        Constraint::Fill(1),
                        Constraint::Length(horizontal_padding),
                    ])
                    .areas::<3>(vertical_dialog_chunk)[1];

                app_state
                    .process_kill_dialog
                    .draw(f, dialog_draw_area, &self.styles);
            } else if app_state.is_expanded {
                if let Some(frozen_draw_loc) = frozen_draw_loc {
                    self.draw_frozen_indicator(f, frozen_draw_loc);
                }

                let rect = Layout::default()
                    .margin(0)
                    .constraints([Constraint::Percentage(100)])
                    .split(terminal_size);
                match &app_state.current_widget.widget_type {
                    Cpu => self.draw_cpu(f, app_state, rect[0], app_state.current_widget.widget_id),
                    CpuLegend => self.draw_cpu(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id - 1,
                    ),
                    Mem | BasicMem => self.draw_memory_graph(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    Disk => self.draw_disk_table(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    Temp => self.draw_temp_table(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    Net => {
                        self.draw_network(f, app_state, rect[0], app_state.current_widget.widget_id)
                    }
                    Proc | ProcSearch | ProcSort => {
                        let widget_id = app_state.current_widget.widget_id
                            - match &app_state.current_widget.widget_type {
                                ProcSearch => 1,
                                ProcSort => 2,
                                _ => 0,
                            };

                        self.draw_process(f, app_state, rect[0], widget_id);
                    }
                    Battery =>
                    {
                        #[cfg(feature = "battery")]
                        self.draw_battery(f, app_state, rect[0], app_state.current_widget.widget_id)
                    }
                    TempGraph => self.draw_temperature_graph(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    DiskIoGraph => self.draw_disk_io_graph(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    Power => self.draw_power_graph(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    Claude => self.draw_claude_table(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    ClaudeStats => self.draw_claude_stats(
                        f,
                        app_state,
                        rect[0],
                        app_state.current_widget.widget_id,
                    ),
                    _ => {}
                }
            } else if app_state.app_config_fields.use_basic_mode {
                // Basic mode. This basically removes all graphs but otherwise
                // the same info.
                if let Some(frozen_draw_loc) = frozen_draw_loc {
                    self.draw_frozen_indicator(f, frozen_draw_loc);
                }

                let data = app_state.data_store.get_data();
                let actual_cpu_data_len = data.cpu_harvest.len();

                // This fixes #397, apparently if the height is 1, it can't render the CPU
                // bars...
                let cpu_height = {
                    let c = (actual_cpu_data_len / 4) as u16
                        + u16::from(!actual_cpu_data_len.is_multiple_of(4))
                        + u16::from(
                            app_state.app_config_fields.show_average_cpu
                                && app_state.app_config_fields.dedicated_average_row
                                && !actual_cpu_data_len.saturating_sub(1).is_multiple_of(4),
                        );

                    if c <= 1 { 1 } else { c }
                };

                let mut mem_rows = 1;

                if data.swap_harvest.is_some() {
                    mem_rows += 1; // add row for swap
                }

                #[cfg(feature = "zfs")]
                {
                    if data.arc_harvest.is_some() {
                        mem_rows += 1; // add row for arc
                    }
                }

                #[cfg(not(target_os = "windows"))]
                {
                    if data.cache_harvest.is_some() {
                        mem_rows += 1;
                    }
                }

                #[cfg(feature = "gpu")]
                {
                    mem_rows += data.gpu_harvest.len() as u16; // add row(s) for gpu
                }

                let network_rows = if app_state.app_config_fields.network_show_packets {
                    4 // 4 rows for RX/TX and Packet Rates (Avg sizes moved to right side)
                } else {
                    2 // 2 rows for RX and TX
                };

                if mem_rows < network_rows {
                    mem_rows += network_rows - mem_rows; // min rows
                }

                let vertical_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .margin(0)
                    .constraints([
                        Constraint::Length(cpu_height),
                        Constraint::Length(mem_rows),
                        Constraint::Length(2),
                        Constraint::Min(5),
                    ])
                    .split(terminal_size);

                let middle_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(vertical_chunks[1]);

                if vertical_chunks[0].width >= 2 {
                    self.draw_basic_cpu(f, app_state, vertical_chunks[0], 1);
                }
                if middle_chunks[0].width >= 2 {
                    self.draw_basic_memory(f, app_state, middle_chunks[0], 2);
                }
                if middle_chunks[1].width >= 2 {
                    self.draw_basic_network(f, app_state, middle_chunks[1], 3);
                }

                let mut later_widget_id: Option<u64> = None;
                if let Some(basic_table_widget_state) = &app_state.states.basic_table_widget_state {
                    let widget_id = basic_table_widget_state.currently_displayed_widget_id;
                    later_widget_id = Some(widget_id);
                    if vertical_chunks[3].width >= 2 {
                        match basic_table_widget_state.currently_displayed_widget_type {
                            Disk => {
                                self.draw_disk_table(f, app_state, vertical_chunks[3], widget_id)
                            }
                            Proc | ProcSort => {
                                let wid = widget_id
                                    - match basic_table_widget_state.currently_displayed_widget_type
                                    {
                                        ProcSearch => 1,
                                        ProcSort => 2,
                                        _ => 0,
                                    };
                                self.draw_process(f, app_state, vertical_chunks[3], wid);
                            }
                            Temp => {
                                self.draw_temp_table(f, app_state, vertical_chunks[3], widget_id)
                            }
                            Battery =>
                            {
                                #[cfg(feature = "battery")]
                                self.draw_battery(f, app_state, vertical_chunks[3], widget_id)
                            }
                            TempGraph => self.draw_temperature_graph(
                                f,
                                app_state,
                                vertical_chunks[3],
                                widget_id,
                            ),
                            DiskIoGraph => {
                                self.draw_disk_io_graph(f, app_state, vertical_chunks[3], widget_id)
                            }
                            Power => {
                                self.draw_power_graph(f, app_state, vertical_chunks[3], widget_id)
                            }
                            Claude => {
                                self.draw_claude_table(f, app_state, vertical_chunks[3], widget_id)
                            }
                            ClaudeStats => {
                                self.draw_claude_stats(f, app_state, vertical_chunks[3], widget_id)
                            }
                            _ => {}
                        }
                    }
                }

                if let Some(widget_id) = later_widget_id {
                    self.draw_basic_table_arrows(f, app_state, vertical_chunks[2], widget_id);
                }
            } else {
                // Draws using the passed in (or default) layout.
                if let Some(frozen_draw_loc) = frozen_draw_loc {
                    self.draw_frozen_indicator(f, frozen_draw_loc);
                }

                // A two-pass algorithm - get layouts using constraints (first pass),
                // then pass each layout to the corresponding widget (second pass).
                // Note that layouts are already cached in ratatui, so we don't need
                // to do it manually!
                let base = Layout::vertical(self.layout.rows.iter().map(|r| r.constraint))
                    .split(terminal_size);

                for (br, base) in self.layout.rows.iter().zip(base.iter()) {
                    let base =
                        Layout::horizontal(br.children.iter().map(|bc| bc.constraint)).split(*base);

                    for (bc, base) in br.children.iter().zip(base.iter()) {
                        let base = Layout::vertical(bc.children.iter().map(|bcr| bcr.constraint))
                            .split(*base);

                        for (widgets, base) in bc.children.iter().zip(base.iter()) {
                            let widget_draw_locs =
                                Layout::horizontal(widgets.children.iter().map(|bw| bw.constraint))
                                    .split(*base);

                            self.draw_widgets_with_constraints(
                                f,
                                app_state,
                                widgets,
                                &widget_draw_locs,
                            );
                        }
                    }
                }
            }
        })?;

        // We also move it back to the origin to try rand avoid wasting CPU cycles for things like kitty's
        // `cursor_trail` calculations.
        let backend = terminal.backend_mut();
        backend.set_cursor_position(Position::ORIGIN)?;
        backend.flush()?;

        if let Some(updated_current_widget) = app_state
            .widget_map
            .get(&app_state.current_widget.widget_id)
        {
            app_state.current_widget = updated_current_widget.clone();
        }

        app_state.is_force_redraw = false;
        app_state.is_determining_widget_boundary = false;

        Ok(())
    }

    fn draw_widgets_with_constraints(
        &self, f: &mut Frame<'_>, app_state: &mut App, widgets: &BottomColRow,
        widget_draw_locs: &[Rect],
    ) {
        use BottomWidgetType::*;
        for (widget, draw_loc) in widgets.children.iter().zip(widget_draw_locs) {
            if draw_loc.width >= 2 && draw_loc.height >= 2 {
                match &widget.widget_type {
                    Cpu => self.draw_cpu(f, app_state, *draw_loc, widget.widget_id),
                    Mem => self.draw_memory_graph(f, app_state, *draw_loc, widget.widget_id),
                    Net => self.draw_network(f, app_state, *draw_loc, widget.widget_id),
                    Temp => self.draw_temp_table(f, app_state, *draw_loc, widget.widget_id),
                    Disk => self.draw_disk_table(f, app_state, *draw_loc, widget.widget_id),
                    Proc => self.draw_process(f, app_state, *draw_loc, widget.widget_id),
                    Battery =>
                    {
                        #[cfg(feature = "battery")]
                        self.draw_battery(f, app_state, *draw_loc, widget.widget_id)
                    }
                    TempGraph => {
                        self.draw_temperature_graph(f, app_state, *draw_loc, widget.widget_id)
                    }
                    DiskIoGraph => {
                        self.draw_disk_io_graph(f, app_state, *draw_loc, widget.widget_id)
                    }
                    Power => self.draw_power_graph(f, app_state, *draw_loc, widget.widget_id),
                    Claude => self.draw_claude_table(f, app_state, *draw_loc, widget.widget_id),
                    ClaudeStats => {
                        self.draw_claude_stats(f, app_state, *draw_loc, widget.widget_id)
                    }
                    _ => {}
                }
            }
        }
    }
}
