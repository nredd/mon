//! Scan a real `~/.claude` tree and report what the history widget would draw.
//!
//! Exists to check the scan against ground truth on a machine with real transcripts, which
//! a unit test cannot do -- the fixtures it builds are the shapes we already thought of.
//! Prints per-day per-family totals so they can be diffed against an independent count, and
//! times the cold read, the checkpoint write, and the warm restore.
//!
//! ```console
//! $ cargo run -p claude-metrics --example history_scan --release
//! ```

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use claude_metrics::{Bucket, ModelFamily, REFRESH_CHUNK_BYTES, TokenHistory};

const WINDOW: Duration = Duration::from_hours(30 * 24);
const BUCKET: Duration = Duration::from_secs(10);
const DAY: Duration = Duration::from_hours(24);

fn main() {
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("no HOME; nothing to scan");
        return;
    };

    let root = std::path::PathBuf::from(home).join(".claude");
    let checkpoint = std::env::temp_dir().join("claude-metrics-history-scan.json");
    let _ = std::fs::remove_file(&checkpoint);

    println!("root: {}", root.display());

    let mut history = TokenHistory::new(&root, WINDOW, BUCKET);

    let started = Instant::now();
    let mut passes = 0u32;

    while !history.refresh_bounded(REFRESH_CHUNK_BYTES) {
        passes += 1;

        if let Some(progress) = history.warmup_progress() {
            println!("  pass {passes}: {:.0}%", progress * 100.0);
        }
    }

    let cold = started.elapsed();
    println!("cold read: {cold:.2?} over {} passes\n", passes + 1);

    report(&history);

    let started = Instant::now();
    match history.save(&checkpoint) {
        Ok(()) => {
            let size = std::fs::metadata(&checkpoint).map_or(0, |meta| meta.len());
            println!(
                "\ncheckpoint: {:.1} MB in {:.2?}",
                f64::from(u32::try_from(size / 1024).unwrap_or(u32::MAX)) / 1024.0,
                started.elapsed()
            );
        }
        Err(err) => println!("\ncheckpoint failed: {err}"),
    }

    let started = Instant::now();
    let mut restored = TokenHistory::new(&root, WINDOW, BUCKET);
    restored.load(&checkpoint);
    let load = started.elapsed();

    let started = Instant::now();
    let first = restored.refresh_bounded(REFRESH_CHUNK_BYTES);
    let warm = started.elapsed();

    println!("warm start: load {load:.2?}, first refresh {warm:.2?}, caught up: {first}");

    // Both sides have to be caught up before they can be compared. Live sessions append
    // while this runs, so the original needs another pass too -- otherwise the restored one
    // legitimately holds more and the diff says "mismatch" about working code.
    while !restored.refresh_bounded(REFRESH_CHUNK_BYTES) {}
    while !history.refresh_bounded(REFRESH_CHUNK_BYTES) {}

    let before = totals_by_day(&history);
    let after = totals_by_day(&restored);

    if before == after {
        println!("restored history matches the one it was written from");
    } else {
        println!("MISMATCH after restore");

        for (key, value) in &before {
            let other = after.get(key).copied().unwrap_or(0);
            if *value != other {
                println!("  {} {}: {value} -> {other}", key.0, key.1);
            }
        }
    }

    let _ = std::fs::remove_file(&checkpoint);
}

fn report(history: &TokenHistory) {
    let families = history.families();
    println!("families in window: {families:?}\n");

    println!("{:<12}{:<10}{:>18}", "day (UTC)", "family", "tokens");

    for ((day, family), total) in totals_by_day(history) {
        if total < 1_000_000 {
            continue;
        }

        println!("{day:<12}{family:<10}{total:>18}");
    }
}

/// Per-day per-family totals, keyed so they can be compared across two histories.
fn totals_by_day(history: &TokenHistory) -> BTreeMap<(String, String), u64> {
    let mut out = BTreeMap::new();

    for bucket in history.aggregate(WINDOW, DAY) {
        for family in ModelFamily::ALL {
            let total = bucket.total_for(family);

            if total > 0 {
                out.insert((utc_day(&bucket), family.label().to_owned()), total);
            }
        }
    }

    out
}

/// `YYYY-MM-DD` for a bucket that starts on a UTC day boundary.
fn utc_day(bucket: &Bucket) -> String {
    let days = bucket.start_ms / 86_400_000;
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Howard Hinnant's `civil_from_days`, which is the standard way to do this without a date
/// library. The crate deliberately has none.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };

    (
        if m <= 2 { y + 1 } else { y },
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}
