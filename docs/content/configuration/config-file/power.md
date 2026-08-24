# Power Widget

!!! note "Fork addition"

    This widget is specific to [mon](https://github.com/nredd/mon) and is not part of
    upstream bottom. It is macOS + Apple Silicon only.

## Config options

| Config option      | Type                                                   | Default | Behaviour                                                    |
| ------------------ | ------------------------------------------------------ | ------- | ------------------------------------------------------------ |
| `show_system`      | Boolean                                                | `true`  | Show whole-system power draw.                                |
| `show_cpu`         | Boolean                                                | `true`  | Show CPU package power.                                      |
| `show_gpu`         | Boolean                                                | `true`  | Show GPU power.                                              |
| `show_ane`         | Boolean                                                | `true`  | Show Apple Neural Engine power.                              |
| `show_ram`         | Boolean                                                | `false` | Show DRAM power. Off by default -- rarely reported.          |
| `hide_unreported`  | Boolean                                                | `true`  | Drop channels this machine has never reported a value for.   |
| `legend_position`  | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | `top-right` | Where to place the legend. |

`hide_unreported` is the one worth knowing about. Not every Apple Silicon chip wires up
every power rail -- on an M4, `cpu`, `ane`, and `ram` sit at a constant `0.0`. Leaving this
on keeps the chart to the channels that mean something rather than drawing flat lines that
look like an idle rail.

## Styling

See [the styling page](styling.md#power) for `[styles.power]`.

## Example

```toml
[power]
show_system = true
show_gpu = true
show_cpu = false
hide_unreported = true
legend_position = "top-right"

[styles.power]
# Read in channel order: system, CPU, GPU, ANE, RAM.
colours = ["#66c2a5", "#fc8d62", "#8da0cb", "#e78ac3", "#a6d854"]
```
