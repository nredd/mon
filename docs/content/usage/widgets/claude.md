# Claude Widgets

!!! note "Fork addition"

    These widgets are specific to [mon](https://github.com/nredd/mon) and are not part of
    upstream bottom.

Two widgets read live [Claude Code](https://claude.com/claude-code) activity off the local
`~/.claude` tree:

- `claude` -- a table of live sessions
- `claude_graph` -- token throughput over time, by model family

Neither is in the default layout. Add them to your
[layout](../../configuration/config-file/layout.md) to use them.

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

## Where the numbers come from

Tokens, agent counts, and turn durations are parsed out of the session transcripts under
`~/.claude/projects`.

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
