//! Dump the bucketed token history read from `~/.claude`, for eyeballing the data layer.
//!
//! ```console
//! $ cargo run --release --example history_probe
//! ```

use std::time::Duration;

use claude_metrics::TokenHistory;

fn main() {
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("no HOME");
        return;
    };

    let root = std::path::Path::new(&home).join(".claude");
    let mut history = TokenHistory::new(&root, Duration::from_secs(3600), Duration::from_secs(60));

    let started = std::time::Instant::now();
    history.refresh();
    let elapsed = started.elapsed();

    let families = history.families();
    let buckets = history.buckets();

    println!("refresh took {elapsed:?}");
    println!("families: {families:?}");
    println!("buckets: {}", buckets.len());

    for bucket in buckets.iter().filter(|b| b.total() > 0) {
        let per: Vec<String> = families
            .iter()
            .map(|f| format!("{}={}", f.label(), bucket.total_for(*f)))
            .collect();

        println!(
            "  {} total={:>10}  {}",
            bucket.start_ms,
            bucket.total(),
            per.join(" ")
        );
    }
}
