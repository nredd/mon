# Agent Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom.

Two widgets track token usage for a coding-agent harness running locally, read off that
harness's own transcript files on disk:

- `agent_graph` -- token throughput over time, by series
- `agent_stats` -- token spend over the last hour, as stacked bands by series

Both are harness-agnostic. Which harness a given instance reads is set per-instance via
`source = "claude" | "codex" | "pi" | "all"` on that widget's layout entry -- see
[the config-file page](../../configuration/config-file/agent.md). `source = "all"` merges
every harness present on the machine into one series per harness instead of one series per
model family within a single harness.

Neither widget is in the default layout. Add them to your
[layout](../../configuration/config-file/layout.md), or start from one of the ready-made
sample configs, each of which draws a stats graph and a rate graph and nothing else:

- [`sample_configs/claude_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/claude_config.toml) -- `source = "claude"`
- [`sample_configs/codex_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/codex_config.toml) -- `source = "codex"`
- [`sample_configs/pi_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/pi_config.toml) -- `source = "pi"`
- [`sample_configs/all_harnesses_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/all_harnesses_config.toml) -- `source = "all"`, every harness combined

```console
$ mon -C sample_configs/all_harnesses_config.toml --pixel_graphs kitty
```

## No live-session table

Earlier versions of this fork had a third widget, a live sessions table built from Claude
Code's own PID registry (`~/.claude/sessions/<PID>.json`). It is gone, on purpose: Codex and
pi write no equivalent live registry, and there is no honest way to build a "sessions
running right now" table for them. Rather than give Claude a table the other two harnesses
can't have, every harness -- Claude included -- is read the same way: by tailing whatever
transcripts each harness has written to disk, a refresh tick behind the actual model call.
That means every number here is a close, slightly lagging estimate, never a live,
synchronous one, for any harness.

## Token graph

Token throughput in tokens/second, one line per series, differenced from the cumulative
totals each harness's transcripts add up to.

The y-axis is **logarithmic by default**. Cache reads run into the millions of tokens per
second while fresh input tokens are single digits, so on a linear axis every series but the
largest sits flat on the floor. Set `use_log = false` to switch.

## Stats graph

Token spend bucketed by **minute over the last hour**, so the shape of a working session is
visible rather than collapsed into a single number. This is what Claude Code's own `/status`
stats screen does, generalized to every harness and to `source = "all"`.

Series are drawn as stacked bands, so the top of the stack is the total spend in that minute
and each band is one series' share. The legend carries each series' total across the whole
window.

The y-axis is **linear by default**, unlike the token graph. A bucketed total spans a far
narrower range than an instantaneous rate -- a busy minute and a quiet one differ by a factor
of ten, not by five orders of magnitude -- and stacked bands only add up to the total on a
linear axis. Set `stats_use_log = true` if one series dwarfs the rest badly enough to need
it, accepting that the bands stop summing to the visible total.

Each band is drawn as a **rounded staircase**, holding a bucket's value flat across the
minute it covers. That is not only cosmetic: a bucket is a total over a minute, not a
reading at an instant, so sloping between bucket centres would spread one busy minute over
three and understate its peak. The corner rounding is cosmetic, and is what makes the chart
read the way `/status` does rather than like a bar code.

Both the fill and the stepping are pixel-path-only. With cell markers the graph degrades to
straight-joined band boundaries, which is still readable -- the same trade the pixel path
makes everywhere else.

## What each `source` reads and how it counts

Every harness parses defensively: its transcript schema is undocumented or drifts between
releases, so a schema surprise must never take a widget down. Every field is optional,
unknown fields are ignored, and an unreadable file is treated as absent rather than as an
error.

### `source = "claude"`

Reads `~/.claude/projects/<cwd-slug>/<sessionId>.jsonl` and its
`<sessionId>/subagents/agent-*.jsonl` files. Series are model families: Opus, Sonnet, Haiku,
Fable, Other, folded from the raw model id by substring match.

Counting rules: dedupe on `requestId` + `message.id` (retries and resumed sessions replay
identical messages); a message with several content blocks carries one `usage` object and
counts once; take `cache_creation_input_tokens` alone, never plus its `ephemeral_*`
sub-buckets; `output_tokens` is a running total across a message's content-block records and
is tracked as a high-water mark, not summed; `<synthetic>` models and `isApiErrorMessage`
records are skipped.

!!! warning "Background Haiku calls are invisible here"

    Session titles and similar internal calls are billed but never written to a transcript,
    so Claude's Haiku totals can read lower than the real spend. A few percent of a long
    session's tokens can also be missing outright -- calibrated against
    `~/.claude.json`'s `lastModelUsage`, a short session matched exactly on all four token
    fields, a 1199-line one matched input/cache-read/cache-write exactly with output at
    97.7%.

### `source = "codex"`

Reads `$CODEX_HOME/sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl` (or `~/.codex/sessions` when
`CODEX_HOME` is unset) plus `archived_sessions/rollout-*.jsonl`. Series are model families:
GPT-5, GPT-4, the o-series (o1/o3/o4), Codex-branded minis, Other, folded from the raw model
id by substring match.

Counting rules: only `event_msg/token_count` events with a non-null `info` count (a null
`info` is a session-start ping or an aborted turn); the cumulative `info.total_token_usage`
is preferred over `info.last_token_usage`, and this crate tracks the previous cumulative
snapshot per rollout file to take the delta, so a repeated identical snapshot is not
double-counted; reasoning tokens (`reasoning_output_tokens`) are folded into output, priced
the same as output; Codex exposes no cache-write count, so cache-creation is always zero for
this harness.

### `source = "pi"`

Reads `~/.pi/agent/sessions/<slug>/<timestamp>_<uuid>.jsonl`. Series are providers:
Anthropic, OpenAI, Google, Other, taken directly from each assistant message's own
`provider` field rather than folded from its model id -- pi is multi-provider by design, and
a per-model series would balloon as models rotate.

Counting rules: every assistant message carries one clean per-turn `usage` object, taken
directly with no running-total or high-water-mark trick needed (unlike Claude); `usage` on
`compaction` and `branch_summary` entries is also counted, since both represent a real
summarization LLM call.

### `source = "all"`

Merges the three ledgers above into one, grouped by **harness label** (`Claude`/`Codex`/
`Pi`) instead of by model family or provider -- every family or provider within one harness
is summed into a single number for that harness. A harness with nothing on disk (never run
on this machine) simply contributes no series, nothing to configure.

## Key bindings

`agent_graph` and `agent_stats` take the usual graph bindings.

| Binding   | Action                                  |
| --------- | ---------------------------------------- |
| ++plus++  | Zoom in on chart (decrease time range)  |
| ++minus++ | Zoom out on chart (increase time range) |
| ++equal++ | Reset zoom                              |
