# Agent Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom. See [the usage page](../../usage/widgets/agent.md).

## Config options

Set under `[agent]`. These cover both `agent_graph` and `agent_stats`, and apply to every
instance of either widget regardless of its `source`.

| Config option     | Type                                                   | Default | Behaviour                                        |
| ----------------- | ------------------------------------------------------ | ------- | ------------------------------------------------ |
| `use_log`         | Boolean                                                 | `true`  | Logarithmic y-axis on the token-rate graph.      |
| `stats_use_log`   | Boolean                                                 | `false` | Logarithmic y-axis on the token-history graph.   |
| `legend_position` | String (one of ["none", "top-left", "top", "top-right", "left", "right", "bottom-left", "bottom", "bottom-right"]) | `top-right` | Where to place the graph legend. |

`use_log` defaults on because the series genuinely span orders of magnitude -- cache reads
run into the millions of tokens per second while fresh input tokens are single digits. On a
linear axis everything but the largest series sits flat on the floor. `stats_use_log`
defaults off: a bucketed total spans a far narrower range than an instantaneous rate, and
stacked bands only sum to the visible total on a linear axis.

## `source`, per widget instance

Which harness (or harnesses) a particular `agent_graph`/`agent_stats` instance reads is not
set in `[agent]` -- it is a field on that widget's own layout entry, so two instances can
read different sources side by side:

```toml
[[row]]
[[row.child]]
type = "agent_graph"
source = "claude"   # "claude" | "codex" | "pi" | "all", defaults to "claude"
```

`source = "all"` merges every harness present on the machine into one series set, one line
or band per harness (`Claude`/`Codex`/`Pi`) rather than per model family within one harness.
A harness with nothing on disk simply contributes no series.

## Styling

See [the styling page](styling.md#agent) for `[styles.agent]`.

## Example

```toml
[agent]
use_log = true
stats_use_log = false
legend_position = "top-right"

[styles.agent]
# Read in model-family order: Opus, Sonnet, Haiku, Fable, Other -- this is Claude's order,
# since the widget below reads source = "claude". A source = "all" widget would read this
# in harness order (Claude, Codex, Pi) instead.
colours = ["#3987e5", "#d95926", "#199e70", "#c98500", "#d55181"]

[[row]]
[[row.child]]
type = "agent_stats"
source = "claude"
```
