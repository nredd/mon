# Claude Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom. See [the usage page](../../usage/widgets/claude.md), which also covers
    the statusline tee that cost and context data depend on.

## Config options

Set under `[claude]`. These cover both the `claude` table and the `claude_graph` graph.

| Config option     | Type                                                   | Default | Behaviour                                        |
| ----------------- | ------------------------------------------------------ | ------- | ------------------------------------------------ |
| `use_log`         | Boolean                                                | `true`  | Logarithmic y-axis on the token graph.           |
| `legend_position` | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | `top-right` | Where to place the graph legend. |

`use_log` defaults on because the series genuinely span orders of magnitude -- cache reads
run into the millions of tokens per second while fresh input tokens are single digits. On a
linear axis everything but the largest series sits flat on the floor.

## Styling

See [the styling page](styling.md#claude) for `[styles.claude]`.

## Example

```toml
[claude]
use_log = true
legend_position = "top-right"

[styles.claude]
# Read in model-family order: Opus, Sonnet, Haiku, Fable, Other.
colours = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181"]
```
