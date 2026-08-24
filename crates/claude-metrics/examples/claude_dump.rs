//! Dumps what `claude-metrics` reads out of the real `~/.claude` tree.
//!
//! Used to cross-check totals against `~/.claude.json`'s `projects[cwd].lastModelUsage`,
//! which records real per-model counts and cost -- but only for the *last* session in each
//! project, written at shutdown. It is a calibration reference, not a live source.
//!
//! ```console
//! $ cargo run -p claude-metrics --example claude_dump
//! ```

fn main() {
    // Calibration mode: run the accumulator over one transcript so its totals can be
    // compared against `~/.claude.json`'s `lastModelUsage` for that session.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--transcript") {
        // Every remaining argument is folded into one accumulator. A session's totals live
        // across its main transcript *and* its `subagents/agent-*.jsonl` files, so pass all
        // of them to compare against `lastModelUsage`, which aggregates the lot.
        let paths: Vec<String> = args.collect();
        if paths.is_empty() {
            eprintln!("--transcript wants one or more paths");
            std::process::exit(2);
        }
        dump_transcript(&paths);
        return;
    }

    let Some(mut metrics) = claude_metrics::ClaudeMetrics::with_default_root() else {
        eprintln!("No $HOME, so no ~/.claude to read.");
        std::process::exit(1);
    };

    println!("root: {}", metrics.root().display());
    metrics.refresh();

    let sessions = metrics.sessions();
    println!("\n{} live session(s):", sessions.len());

    for session in sessions {
        let id = session.session_id.as_deref().unwrap_or("?");
        println!(
            "\n  pid {pid:<8} {name:<12} {status:<6} {cwd}\n    id    {id}\n    tmux  {tmux}  pane {pane}",
            pid = session.pid,
            name = session.name.as_deref().unwrap_or("-"),
            status = session.status.as_deref().unwrap_or("-"),
            cwd = session.cwd.as_deref().unwrap_or("-"),
            tmux = session.tmux.as_deref().unwrap_or("-"),
            pane = session.tmux_pane().unwrap_or("-"),
        );

        let totals = metrics.totals_for(id);
        if totals.is_empty() {
            println!("    (no usage read yet)");
            continue;
        }

        println!(
            "    {:<8} {:>12} {:>12} {:>14} {:>14} {:>14}",
            "model", "input", "output", "cache read", "cache write", "total"
        );
        for (family, t) in totals {
            println!(
                "    {:<8} {:>12} {:>12} {:>14} {:>14} {:>14}",
                family.label(),
                t.input,
                t.output,
                t.cache_read,
                t.cache_creation,
                t.total()
            );
        }

        println!("    subagent messages: {}", metrics.subagent_messages(id));
        if let Some(ms) = metrics.last_turn_duration_ms(id) {
            // Integer division: a turn duration in whole seconds is plenty, and it keeps
            // the lint about u64 -> f64 precision honest rather than silenced.
            println!("    last turn: {}.{}s", ms / 1000, (ms % 1000) / 100);
        }
    }

    let all = metrics.totals_by_model();
    if !all.is_empty() {
        println!("\nacross all live sessions:");
        for (family, t) in all {
            println!(
                "  {:<8} in {:>10}  out {:>10}  cache_r {:>12}  cache_w {:>12}",
                family.label(),
                t.input,
                t.output,
                t.cache_read,
                t.cache_creation
            );
        }
    }
}

/// Accumulate one or more transcript files into a single set of totals.
fn dump_transcript(paths: &[String]) {
    use claude_metrics::{Record, UsageAccumulator};

    let mut acc = UsageAccumulator::default();
    let mut lines = 0u64;

    for path in paths {
        let Ok(contents) = std::fs::read_to_string(path) else {
            eprintln!("Could not read: '{path}'");
            std::process::exit(1);
        };

        for line in contents.lines() {
            lines += 1;
            if let Some(record) = Record::parse(line) {
                acc.ingest(&record);
            }
        }
    }

    println!("{} file(s), {lines} lines\n", paths.len());
    println!(
        "{:<8} {:>10} {:>10} {:>12} {:>12}",
        "model", "input", "output", "cache read", "cache write"
    );
    for (family, t) in acc.totals() {
        println!(
            "{:<8} {:>10} {:>10} {:>12} {:>12}",
            family.label(),
            t.input,
            t.output,
            t.cache_read,
            t.cache_creation
        );
    }
    println!(
        "\nmain messages: {}  subagent messages: {}",
        acc.main_messages, acc.sidechain_messages
    );
}
