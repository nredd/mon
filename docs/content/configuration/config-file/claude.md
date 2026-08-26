# Claude Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom. See [the usage page](../../usage/widgets/claude.md), which also covers
    the statusline tee that cost and context data depend on.

## Config options

Set under `[claude]`. These cover the `claude` table, the `claude_graph` rate graph, and the
`claude_stats` history graph.

| Config option     | Type                                                   | Default | Behaviour                                        |
| ----------------- | ------------------------------------------------------ | ------- | ------------------------------------------------ |
| `use_log`         | Boolean                                                | `false` | Logarithmic y-axis on the token-rate graph.      |
| `stats_use_log`   | Boolean                                                | `false` | Logarithmic y-axis on the stats graph.           |
| `stats_range`     | String (one of ["30m", "2h", "8h", "24h", "7d", "30d"]) | `2h`   | How far back the stats graph starts out reaching. |
| `legend_position` | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | `top-right` | Where to place the graph legend. |

Both `use_log` flags default off. The graphs exist to compare model families against each
other over time, and on a log axis a band twice as tall is not twice the tokens. Turn one on
for a window where a single family genuinely dwarfs the rest badly enough that the others
sit flat on the floor -- or press `l`, which toggles the focused graph at runtime.

`stats_range` only sets where the graph *starts*. `T` cycles it, `+`/`-` step it, and `=`
comes back here. An unrecognised value is a config error rather than a silent fallback.

`legend_position` now only affects the sessions table; both graphs draw an inline legend in
a reserved row under the plot instead of a box floating inside it.

## Styling

See [the styling page](styling.md#claude) for `[styles.claude]`.

## Example

```toml
[claude]
use_log = false
stats_use_log = false
stats_range = "2h"

[styles.claude]
# Read in model-family order: Opus, Sonnet, Haiku, Fable, Other.
colours = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181"]
```
