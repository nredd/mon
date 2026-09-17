//! Dump the bucketed token history read from `~/.claude`, for eyeballing the data layer.
//!
//! ```console
//! $ cargo run --release --example history_probe
//! ```

use std::time::Duration;

use harness_metrics::{Harness, HarnessLedger};

fn main() {
    let Some(mut ledger) = HarnessLedger::new(
        Harness::Claude,
        Duration::from_secs(3600),
        Duration::from_secs(60),
    ) else {
        eprintln!("no HOME");
        return;
    };

    let started = std::time::Instant::now();
    ledger.refresh();
    let elapsed = started.elapsed();

    let families = ledger.families_present();
    let buckets = ledger.buckets();

    println!("refresh took {elapsed:?}");
    println!("families: {families:?}");
    println!("buckets: {}", buckets.len());

    for bucket in buckets.iter().filter(|b| b.total() > 0) {
        let per: Vec<String> = families
            .iter()
            .map(|f| format!("{}={}", f, bucket.total_for(f)))
            .collect();

        println!(
            "  {} total={:>10}  {}",
            bucket.start_ms,
            bucket.total(),
            per.join(" ")
        );
    }
}
