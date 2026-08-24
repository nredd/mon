# Flags

!!! Warning

    This section is in progress, and is just copied from the old documentation.

You can configure flags by putting them in `[flags]` table. Example:

```toml
[flags]
hide_avg_cpu = true
```

Most of the [command line flags](../command-line-options.md) have config file equivalents to avoid having to type them out
each time:

| Field                        | Type                                                                                                               | Functionality                                                                                                                                                                    |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `hide_avg_cpu`               | Boolean                                                                                                            | Deprecated - use `cpu.hide_avg_cpu`. Hides the average CPU usage.                                                                                                                |
| `dot_marker`                 | Boolean                                                                                                            | Deprecated, use `marker` instead. Equivalent to `marker = "dot"`.                                                                                                                 |
| `marker`                     | String (one of ["braille", "octant", "sextant", "quadrant", "half_block", "dot", "block", "bar"])                  | Which glyph family to plot graphs with. Defaults to `braille`.                                                                                                                    |
| `pixel_graphs`               | String (one of ["off", "auto", "kitty"])                                                                           | Draw graphs as real pixels rather than cell markers. Defaults to `off`.                                                                                                           |
| `cpu_left_legend`            | Boolean                                                                                                            | Deprecated - use `cpu.left_legend`. Puts the CPU chart legend to the left side.                                                                                                  |
| `current_usage`              | Boolean                                                                                                            | Deprecated - use `processes.current_usage`. Sets process CPU% to be based on current CPU%.                                                                                       |
| `group_processes`            | Boolean                                                                                                            | Deprecated - use `processes.default_grouped`. Groups processes with the same name by default.                                                                                    |
| `case_sensitive`             | Boolean                                                                                                            | Deprecated - use `processes.case_sensitive`. Enables case sensitivity by default.                                                                                                |
| `whole_word`                 | Boolean                                                                                                            | Deprecated - use `processes.whole_word`. Enables whole-word matching by default.                                                                                                 |
| `regex`                      | Boolean                                                                                                            | Deprecated - use `processes.regex`. Enables regex by default.                                                                                                                    |
| `basic`                      | Boolean                                                                                                            | Hides graphs and uses a more basic look.                                                                                                                                         |
| `use_old_network_legend`     | Boolean                                                                                                            | DEPRECATED - uses the older network legend.                                                                                                                                      |
| `battery`                    | Boolean                                                                                                            | Shows the battery widget.                                                                                                                                                        |
| `rate`                       | Unsigned Int (represents milliseconds) or String (represents human time)                                           | Sets a refresh rate in ms.                                                                                                                                                       |
| `default_time_value`         | Unsigned Int (represents milliseconds) or String (represents human time)                                           | Default time value for graphs in ms.                                                                                                                                             |
| `time_delta`                 | Unsigned Int (represents milliseconds) or String (represents human time)                                           | The amount in ms changed upon zooming.                                                                                                                                           |
| `hide_time`                  | Boolean                                                                                                            | Hides the time scale.                                                                                                                                                            |
| `temperature_type`           | String (one of ["k", "f", "c", "kelvin", "fahrenheit", "celsius"])                                                 | Sets the temperature unit type.                                                                                                                                                  |
| `default_widget_type`        | String (one of ["cpu", "proc", "net", "temp", "mem", "disk"], same as layout options)                              | Sets the default widget type, use --help for more info.                                                                                                                          |
| `default_widget_count`       | Unsigned Int (represents which `default_widget_type`)                                                              | Sets the n'th selected widget type as the default.                                                                                                                               |
| `disable_click`              | Boolean                                                                                                            | Disables mouse clicks.                                                                                                                                                           |
| `enable_cache_memory`        | Boolean                                                                                                            | Deprecated - use `memory.cache_memory`. Enable cache and buffer memory stats (not available on Windows).                                                                         |
| `process_memory_as_value`    | Boolean                                                                                                            | Deprecated - use `processes.default_memory_value`. Defaults to showing process memory usage by value.                                                                            |
| `tree`                       | Boolean                                                                                                            | Deprecated - use `processes.default_tree`. Defaults to showing the process widget in tree mode.                                                                                  |
| `show_table_scroll_position` | Boolean                                                                                                            | Shows the scroll position tracker in table widgets.                                                                                                                              |
| `show_table_scroll_bar`      | Boolean                                                                                                            | Shows a scroll bar on the right edge of table widgets.                                                                                                                           |
| `process_command`            | Boolean                                                                                                            | Deprecated - use `processes.process_command`. Show processes as their commands by default.                                                                                       |
| `disable_advanced_kill`      | Boolean                                                                                                            | Deprecated - use `processes.disable_advanced_kill`. Disable being able to send signals to processes on supported Unix-like systems. Only available on Linux, macOS, and FreeBSD. |
| `read_only`                  | Boolean                                                                                                            | Prevents performing any actions that affect the system (e.g. stopping processes).                                                                                                |
| `network_use_binary_prefix`  | Boolean                                                                                                            | Deprecated - use `network_graph.use_binary_prefix`. Displays the network widget with binary prefixes.                                                                            |
| `network_use_bytes`          | Boolean                                                                                                            | Deprecated - use `network_graph.use_bytes`. Displays the network widget using bytes.                                                                                             |
| `network_use_log`            | Boolean                                                                                                            | Deprecated - use `network_graph.use_log`. Displays the network widget with a log scale.                                                                                          |
| `disable_gpu`                | Boolean                                                                                                            | Disable NVIDIA and AMD GPU data collection.                                                                                                                                      |
| `retention`                  | String (human readable time, such as "10m", "1h", etc.)                                                            | How much data is stored at once in terms of time.                                                                                                                                |
| `unnormalized_cpu`           | Boolean                                                                                                            | Deprecated - use `processes.unnormalized_cpu`. Show process CPU% without normalizing over the number of cores.                                                                   |
| `expanded`                   | Boolean                                                                                                            | Expand the default widget upon starting the app.                                                                                                                                 |
| `memory_legend`              | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | Deprecated - use `memory_graph.legend_position` or `memory.legend_position`; where to place the legend for the memory widget.                                                    |
| `network_legend`             | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | Deprecated - use `network_graph.legend_position` or `network.legend_position`; where to place the legend for the network widget.                                                 |
| `average_cpu_row`            | Boolean                                                                                                            | Deprecated - use `cpu.basic_average_cpu_row`. Moves the average CPU usage entry to its own row when using basic mode.                                                            |
| `tree_collapse`              | Boolean                                                                                                            | Deprecated - use `processes.tree_collapse`. Collapse process tree by default.                                                                                                    |
| `autohide_time`              | Boolean                                                                                                            | Temporarily shows the time scale in graphs.                                                                                                                                      |
| `table_gap`                  | String (one of ["none", "space", "line"])                                                                          | Controls the gap between table headers and data rows. Defaults to "space".                                                                                                       |
| `disable_keys`               | Boolean                                                                                                            | Disables keyboard shortcuts, including the ones that stop bottom.                                                                                                                |
| `no_write`                   | Boolean                                                                                                            | Disables writing to the config file.                                                                                                                                             |
| `hide_k_threads`             | Boolean                                                                                                            | Deprecated - use `processes.hide_k_threads`. Hide kernel threads from being shown.                                                                                               |
| `free_arc`                   | Boolean                                                                                                            | Deprecated - use `memory.free_arc`. Subtract ARC memory that can be freed from memory usage.                                                                                     |

## Graph markers

`marker` picks which glyph family graphs are plotted with, from finest to coarsest:

| Marker       | Resolution per cell | Glyphs                        |
| ------------ | ------------------- | ----------------------------- |
| `braille`    | 2x4                 | Braille patterns. The default |
| `octant`     | 2x4                 | Legacy Computing octants      |
| `sextant`    | 2x3                 | Legacy Computing sextants     |
| `quadrant`   | 2x2                 | Quadrant blocks               |
| `half_block` | 1x2                 | `▀` `▄` `█`                   |
| `dot`        | 1x1                 | `•`                           |
| `block`      | 1x1                 | `█`, coloured on the background |
| `bar`        | 1x1                 | `▄`                           |

Braille gives the most detail but needs a font with good braille coverage. The Legacy
Computing blocks that `octant` and `sextant` use are newer and less widely covered still; if
they render as tofu, drop to `quadrant` or `half_block`.

Both spellings of the two-word names work, so `--marker half-block` and
`marker = "half_block"` are the same thing.

## Pixel graphs

Cell markers cap out at 2x4 subpixels per cell. A terminal that speaks the Kitty graphics
protocol can draw real pixels instead, which is worth roughly an order of magnitude more
vertical resolution on a short graph.

| Mode    | Behaviour                                                     |
| ------- | ------------------------------------------------------------- |
| `off`   | Cell markers. The default                                     |
| `auto`  | Query the terminal, and use pixels only if it answers          |
| `kitty` | Force the Kitty graphics protocol, skipping the query          |

!!! warning "`auto` cannot detect Kitty from inside tmux"

    Measured on Ghostty 1.3.1 + tmux 3.7c with `allow-passthrough on`: the Kitty capability
    query, wrapped in tmux's DCS passthrough, gets **no reply at all**. Other queries answer
    fine over the same path -- primary DA returns `ESC [?1;2;4c`, cell size returns
    `ESC [6;24;11t` -- so passthrough itself is working.

    The image transport is unaffected. Unicode placeholder cells reach tmux's own text
    buffer with correct row/column diacritics, which is what gives correct clipping,
    scrolling, and pane switching.

    So under tmux only detection is broken, and `kitty` is the setting to use.

The image covers the data region only. The border, y-axis labels, and x-axis labels are
still drawn as text by the normal chart, so the pixel path reuses the same axis geometry
rather than re-deriving it.

If the image cannot be encoded for any reason, the cell-drawn graph underneath stays
visible, so the failure mode is "looks like `off`" rather than a blank widget.
