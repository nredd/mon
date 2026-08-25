# Claude Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom.

Three widgets read [Claude Code](https://claude.com/claude-code) activity off the local
`~/.claude` tree:

- `claude` -- a table of live sessions
- `claude_graph` -- token throughput over time, by model family
- `claude_stats` -- token spend over the last hour, as stacked bands by model family

None of them are in the default layout. Add them to your
[layout](../../configuration/config-file/layout.md) to use them, or start from
[`sample_configs/claude_config.toml`](https://github.com/nredd/mon/blob/main/sample_configs/claude_config.toml),
which draws all three and nothing else:

```console
$ mon -C sample_configs/claude_config.toml --pixel_graphs kitty
```

That config also carries the family colour list and the two `use_log` toggles inline, so it
is a reasonable place to retune them.

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

## Token graph

Token throughput in tokens/second, one line per model family, differenced from the
cumulative totals.

The y-axis is **logarithmic by default**. Cache reads run into the millions of tokens per
second while fresh input tokens are single digits, so on a linear axis every series but the
largest sits flat on the floor. Set `use_log = false` to switch.

## Stats graph

The equivalent of Claude Code's own `/status` stats screen. Claude Code bars token spend by
day; this buckets by **minute over the last hour**, so the shape of a working session is
visible rather than collapsed into a single bar.

Families are drawn as stacked bands, so the top of the stack is the total spend in that
minute and each band is one family's share. The legend carries each family's total across
the whole window.

The y-axis is **linear by default**, unlike the token graph. A bucketed total spans a far
narrower range than an instantaneous rate -- a busy minute and a quiet one differ by a factor
of ten, not by five orders of magnitude -- and stacked bands only add up to the total on a
linear axis. Set `stats_use_log = true` if one family dwarfs the rest badly enough to need
it, accepting that the bands stop summing to the visible total.

The bands are filled only on the [pixel path](../../../#kitty-pixel-rendering). With cell
markers the graph degrades to the band boundaries as plain lines, which is still readable --
the same trade the pixel path makes everywhere else.

This is the only part of a Claude harvest that reads transcripts outside the live sessions,
so it is skipped entirely unless the widget is in your layout.

## Where the numbers come from

Tokens, agent counts, and turn durations are parsed out of the session transcripts under
`~/.claude/projects`.

`claude_stats` walks that tree itself rather than building on the live sessions, and
attributes each record to a bucket using the record's own timestamp. Two consequences worth
knowing: the window is complete the moment the widget first appears, instead of having to be
accumulated live over an hour before the graph says anything, and it keeps the tokens of
sessions that have since exited -- which the live-session view cannot, since it drops a
session's state as soon as it leaves the registry. Candidate files are filtered by
modification time before being opened, so a tree with a year of transcripts in it still only
opens the handful touched inside the window.

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

`claude_graph` takes the usual graph bindings.

| Binding   | Action                                  |
| --------- | --------------------------------------- |
| ++plus++  | Zoom in on chart (decrease time range)  |
| ++minus++ | Zoom out on chart (increase time range) |
| ++equal++ | Reset zoom                              |
