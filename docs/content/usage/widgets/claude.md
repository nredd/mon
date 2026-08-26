# Claude Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom.

Three widgets read [Claude Code](https://claude.com/claude-code) activity off the local
`~/.claude` tree:

- `claude` -- a table of live sessions
- `claude_graph` -- token throughput right now, by model family
- `claude_stats` -- token spend over a selectable range, by model family

None of them are in the default layout. Add them to your
[layout](../../configuration/config-file/layout.md) to use them, or start from
[`sample_configs/claude_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/claude_config.toml),
which draws all three and nothing else:

```console
$ mon -C sample_configs/claude_config.toml --pixel_graphs kitty
```

That config also carries the family colour list, the two `use_log` toggles, and the starting
range inline, so it is a reasonable place to retune them.

## Sessions table

One row per running session, discovered from `~/.claude/sessions/<PID>.json` and pruned with
`kill(pid, 0)` so a session that died without cleaning up does not linger.

| Column | Source |
| ------ | ------ |
| `Session` | The session's name |
| `Dir` | Last component of its working directory |
| `Model` | Model family, folded from the raw model id |
| `State` | `busy` / `idle` |
| `Tokens` | Every token spent, cache included |
| `Cost` | USD so far |
| `Ctx` | Context-window occupancy |
| `Agents` | Subagent messages produced |

Sort by clicking a header or pressing the letter in its label. Defaults to `Tokens`,
descending.

## Stats graph

The equivalent of Claude Code's own `/status` -> Stats -> Models screen. Claude Code bars
token spend by day; this covers a **selectable range** from half an hour to thirty days, so
the shape of a single working session is visible as well as the shape of a month.

Each model family is drawn as its own **unfilled staircase from the baseline**, all overlaid
-- so a band's height is that family's own spend, not its share of a stack. Under the plot
sit two rows: an inline legend (`● Opus 1.2M · ● Sonnet 840.0k`) carrying each family's
total across the window, and the range selector with the active range picked out.

| Range | Bucket | Buckets |
| ----- | ------ | ------- |
| `30m` | 30s    | 60      |
| `2h`  | 2m     | 60      |
| `8h`  | 5m     | 96      |
| `24h` | 15m    | 96      |
| `7d`  | 1h     | 168     |
| `30d` | 6h     | 120     |

The pairing is not free choice. A terminal graph has a couple of hundred usable columns, so
every range yields between sixty and a hundred and eighty buckets whatever its span -- a
fixed one-minute bucket would give the thirty-minute view thirty points and the thirty-day
view forty-three thousand.

Set the starting range with `stats_range`; `T` cycles it at runtime.

The x-axis carries **absolute local times** -- clock times up to `24h`, dates above it --
and the y-axis fills with ticked labels rather than showing only its endpoints, so a band's
height can be read off the axis instead of merely compared with its neighbours.

The y-axis is **linear by default**. Bands are compared by height, and on a log axis a band
twice as tall is not twice the tokens. Set `stats_use_log = true`, or press `l`, if one
family dwarfs the rest badly enough to need it.

Each band is drawn as a **rounded staircase**, holding a bucket's value flat across the span
it covers. That is not only cosmetic: a bucket is a total over its width, not a reading at
an instant, so sloping between bucket centres would spread one busy minute over three and
understate its peak. The corner rounding is what makes the chart read the way `/status` does
rather than like a bar code.

The stepping is pixel-path-only. With cell markers the graph degrades to straight-joined
lines, which is still readable -- the same trade the pixel path makes everywhere else.

This is the only part of a Claude harvest that reads transcripts outside the live sessions,
so it is skipped entirely unless the widget is in your layout -- not even the thread that
does it gets started.

## Token graph

The same graph over a fixed ten-minute window at a ten-second bucket, divided by the bucket
width: what is being spent **right now**, in tokens per second, by family. Same outlines,
same inline legend, no selector row -- its window is fixed, and the stats graph beside it is
the one that answers longer spans.

The y-axis is **linear by default**; `l` toggles it, or set `use_log = true`.

!!! warning "This graph used to be wrong. If you are upgrading, that is why it changed."

    It differenced `totals_by_model()` -- cumulative tokens across the sessions currently in
    the registry -- against the previous tick, keyed by model family. Two things made that
    wrong rather than merely coarse:

    - The "first sighting, do not compute a rate" guard keyed on the *family*, which is
      already present from every other session. So a new or resumed session -- whose
      accumulator is rebuilt from zero with its tailer at offset zero, re-reading the whole
      transcript -- put its entire lifetime token count into one tick's delta. Every family
      in that transcript spiked together, and since the totals are cache-inclusive that was
      routinely millions of tokens per second.
    - The session registry is written by a running process and can be read mid-write. A
      single torn read dropped a session for one tick and brought it back backfilled on the
      next, which is the same spike on a loop.

    It now reads the same bucketed history the stats graph does, which attributes every
    record by the record's own timestamp, so a backfill lands in the past where it belongs.

## Where the numbers come from

Tokens, agent counts, and turn durations are parsed out of the session transcripts under
`~/.claude/projects`.

Two levels of that tree matter:

- `projects/<project>/<session>.jsonl` -- the main session transcript.
- `projects/<project>/<session>/subagents/<agent>.jsonl` -- one per subagent.

The subagent files are not incidental detail. On a real tree they outnumber the session
transcripts four to one and hold more bytes than all of them together, and their messages
are almost entirely *disjoint* from the parent's: a subagent's turns are billed to the
account but written only there.

`claude_stats` walks that tree itself rather than building on the live sessions, and
attributes each record to a bucket using the record's own timestamp. Two consequences worth
knowing: the window is complete the moment the widget first appears, instead of having to be
accumulated live before the graph says anything, and it keeps the tokens of sessions that
have since exited -- which the live-session view cannot, since it drops a session's state as
soon as it leaves the registry.

### The scan, and why it is not on the collection thread

Thirty days is not a subset of the tree. Long-lived sessions keep old transcripts freshly
modified, so the modification-time filter stops filtering well before a month and the window
is effectively everything -- around 1600 files and 600MB on the machine this was measured
on. So the scan runs on a thread of its own and publishes snapshots; a collection tick reads
the latest one and never waits.

It also checkpoints itself to `<cache dir>/mon/claude-history.json` -- roughly 3MB -- so
only the first run pays for a cold read. Measured on that tree: **600ms** cold across 18
slices, then **11ms** to restore and caught up on the first refresh. Losing or deleting the
checkpoint costs one cold read and nothing else, which is what a cache directory is for.

While a cold read is in progress the legend row reads `scanning transcripts... 62%`, because
a graph showing a tenth of the data looks exactly like one showing all of it.

### These totals are about half what `/status` shows, and this side is correct

Claude Code writes **one transcript record per content block** -- thinking, text, tool use
-- and each of them repeats the same cumulative `usage` object for the message. Its own
rollup at `~/.claude/stats-cache.json` sums every record with no dedup, so it double-counts
any message with more than one block.

That is not inference. `dailyModelTokens` in that file matches a naive sum over both
transcript levels **byte for byte**, on every day and every model checked:

| Day (UTC) | Model | `/status` | `mon` | Ratio |
| --- | --- | --- | --- | --- |
| 2026-08-20 | Sonnet | 1,674,475,203 | 827,311,318 | 2.02x |
| 2026-08-20 | Fable | 494,410,640 | 248,313,944 | 1.99x |
| 2026-08-20 | Opus | 182,645,658 | 97,066,122 | 1.88x |
| 2026-08-23 | Sonnet | 1,019,556,621 | 556,491,496 | 1.83x |
| 2026-08-23 | Opus | 106,826,017 | 57,625,136 | 1.85x |

`claude-metrics` counts a message's request-level fields (`input_tokens`,
`cache_read_input_tokens`, `cache_creation_input_tokens`) exactly once, keyed on
`requestId` + `message.id`, and tracks `output_tokens` as a high-water mark because it grows
with each block. So do not try to reconcile the two screens -- they answer the same question
and only one of them is deduping.

`cargo run -p claude-metrics --example history_scan --release` prints the per-day per-model
totals if you want to check them against your own count.

That file is also UTC-keyed, daily-only, and only recomputed when you open the Stats screen,
which is a second reason it is not used as a source here.

**Cost, context-window occupancy, and rate limits need the statusline tee.** Claude Code
hands those to the statusline command on stdin and writes them nowhere else on disk, so
`mon` reads them from a cache that your statusline has to populate. Add this near the top of
`~/.claude/statusline.sh`, right after it reads stdin:

```bash
_mon_cache_payload() {
  local dir="${HOME}/.claude/statusline-cache"
  mkdir -p "$dir" 2>/dev/null || return 0

  local key
  key=$(printf '%s' "$input" | jq -r '.session_id // .sessionId // empty' 2>/dev/null)
  if [ -z "$key" ]; then
    key=$(printf '%s' "$input" | jq -r '.workspace.current_dir // .cwd // "unknown"' 2>/dev/null | tr '/._' '---')
  fi
  [ -n "$key" ] || key="unknown"

  local tmp="${dir}/.${key}.$$"
  printf '%s' "$input" >"$tmp" 2>/dev/null || { rm -f "$tmp" 2>/dev/null; return 0; }
  mv -f "$tmp" "${dir}/${key}.json" 2>/dev/null || rm -f "$tmp" 2>/dev/null

  find "$dir" -maxdepth 1 -name '*.json' -mtime +1 -delete 2>/dev/null
  return 0
} 2>/dev/null
_mon_cache_payload || true
```

Without it the `Cost` and `Ctx` columns read `N/A` and everything else still works.

!!! warning "The numbers are a close estimate, not billing truth"

    Two gaps, both properties of the data source:

    - Background Haiku calls (session titles and similar) are billed but never written to a
      transcript, so they are invisible here.
    - A few percent of a long session's tokens are simply not in the tree. Calibrated
      against `~/.claude.json`'s `lastModelUsage`, a short session matched exactly on all
      four token fields; a 1199-line one matched input, cache-read, and cache-write exactly
      with output at 97.7%.

## Key bindings

| Binding   | Widget         | Action                                       |
| --------- | -------------- | -------------------------------------------- |
| ++t++     | `claude_stats` | Cycle the range, wrapping                    |
| ++plus++  | `claude_stats` | Shorten the range, stopping at `30m`         |
| ++minus++ | `claude_stats` | Lengthen the range, stopping at `30d`        |
| ++equal++ | `claude_stats` | Back to the configured `stats_range`         |
| ++l++     | both graphs    | Toggle the logarithmic y-axis                |

++t++ is shift-`t`. On `claude_stats` the zoom keys move between *ranges* rather than
rescaling the axis -- its axis span has to match the span of the buckets the collector
rolled up, so it is not a free choice.
