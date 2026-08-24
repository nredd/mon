//! Dumps Apple Silicon power samples so `src/collection/power.rs` can be cross-checked
//! against the upstream `macmon` binary.
//!
//! Run both side by side under the same load and compare the watt columns:
//!
//! ```console
//! $ cargo run --example power_dump -- --interval 1000 --samples 10
//! $ macmon pipe -s 10 -i 1000 | jq '{cpu: .cpu_power, gpu: .gpu_power, ane: .ane_power}'
//! ```
//!
//! This is the only thing that exercises the collector until the power widget lands, since
//! `UsedWidgets::use_power` is still hardcoded false.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("power_dump only runs on macOS -- macmon reads Apple Silicon counters.");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    use std::{thread, time::Duration};

    use bottom::power::PowerSampler;

    let mut interval_ms: u32 = 1000;
    let mut samples: usize = 5;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--interval" => {
                interval_ms = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--interval wants a number of milliseconds"));
            }
            "--samples" => {
                samples = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| fail("--samples wants a count"));
            }
            "-h" | "--help" => {
                println!("power_dump [--interval <ms>] [--samples <n>]");
                return;
            }
            other => fail(&format!("Unrecognized argument: '{other}'")),
        }
    }

    println!("Sampling every {interval_ms}ms, {samples} samples.");

    let mut sampler = PowerSampler::spawn(interval_ms);
    let mut seen = 0usize;
    let mut last_received = 0u64;

    // The first sample cannot land before one full interval has elapsed, and the sampler
    // is deliberately non-blocking, so poll rather than assuming a reading is ready.
    let poll = Duration::from_millis(u64::from(interval_ms).max(1) / 4 + 1);

    while seen < samples {
        thread::sleep(poll);
        sampler.poll();

        if let Some(err) = sampler.error() {
            eprintln!("error: {err}");
            std::process::exit(1);
        }

        // `latest` retains the previous reading between intervals, which is what the widget
        // wants but would print duplicates here. Only advance on a genuinely new sample.
        let received = sampler.received();
        if received == last_received {
            continue;
        }
        last_received = received;

        let Some(data) = sampler.latest() else {
            continue;
        };

        seen += 1;
        println!(
            "\n#{seen}\n  \
             cpu  {cpu:>7.3} W    gpu  {gpu:>7.3} W    ane {ane:>7.3} W\n  \
             ram  {ram:>7.3} W    all  {all:>7.3} W    sys {sys}\n  \
             {ecpu_label:<3} {ecpu_mhz:>5} MHz  active {ecpu_act:>5.1}%  scaled {ecpu_scl:>5.1}%\n  \
             {pcpu_label:<3} {pcpu_mhz:>5} MHz  active {pcpu_act:>5.1}%  scaled {pcpu_scl:>5.1}%\n  \
             GPU {gpu_mhz:>5} MHz  active {gpu_act:>5.1}%",
            cpu = data.cpu_power_w,
            gpu = data.gpu_power_w,
            ane = data.ane_power_w,
            ram = data.ram_power_w,
            all = data.all_power_w,
            sys = data
                .sys_power_w
                .map_or_else(|| "  n/a".to_owned(), |w| format!("{w:>7.3} W")),
            ecpu_label = data.ecpu.label,
            ecpu_mhz = data.ecpu.freq_mhz,
            ecpu_act = data.ecpu.active_ratio * 100.0,
            ecpu_scl = data.ecpu.scaled_ratio * 100.0,
            pcpu_label = data.pcpu.label,
            pcpu_mhz = data.pcpu.freq_mhz,
            pcpu_act = data.pcpu.active_ratio * 100.0,
            pcpu_scl = data.pcpu.scaled_ratio * 100.0,
            gpu_mhz = data.gpu_freq_mhz,
            gpu_act = data.gpu_active_ratio * 100.0,
        );
    }
}

#[cfg(target_os = "macos")]
fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
