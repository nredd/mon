# Power Widget

!!! note "Fork addition"

    This widget is specific to [mon](https://github.com/nredd/mon) and is not part of
    upstream bottom. It is macOS + Apple Silicon only.

The power widget graphs power draw in Watts over time, sampled from Apple Silicon's own
counters via [macmon](https://github.com/vladkens/macmon). No `sudo` is required.

It is not in the default layout -- add a `power` widget to your
[layout](../../configuration/config-file/layout.md) to use it.

## Features

Up to five channels are drawn: whole-system, CPU package, GPU, Apple Neural Engine, and
DRAM. The legend shows each channel's most recent reading, and the y-axis scales
automatically to the visible peak with 25% headroom.

!!! warning "Not every chip reports every channel"

    Which rails are actually wired up varies by chip. On an M4, `CPU`, `ANE`, and `RAM`
    report a constant `0.0` while only `System` and `GPU` carry real figures -- the same
    zeros the `macmon` binary reports, so this is the hardware rather than a bug.

    Because of that, `hide_unreported` defaults to `true` and drops any channel that has
    never reported a nonzero value. Otherwise those rails draw as flat lines pinned to the
    bottom of the chart, which reads as "idle" rather than "not measured". Set it to
    `false` to see every channel regardless.

The displayed time range can be adjusted through either the keyboard or mouse.

## Key bindings

Note that key bindings are generally case-sensitive.

| Binding   | Action                                  |
| --------- | --------------------------------------- |
| ++plus++  | Zoom in on chart (decrease time range)  |
| ++minus++ | Zoom out on chart (increase time range) |
| ++equal++ | Reset zoom                              |

## Mouse bindings

| Binding      | Action                                                         |
| ------------ | -------------------------------------------------------------- |
| ++"Scroll"++ | Scrolling up or down zooms in or out of the graph respectively |
