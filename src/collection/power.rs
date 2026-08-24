//! Apple Silicon power metrics, sampled via the [macmon] library.
//!
//! [`macmon::Sampler::get_metrics`] *blocks* the calling thread for the whole sampling
//! interval, so it cannot run on bottom's collection thread -- a 1000ms interval would
//! stall every other collector for a full second. The sampler therefore owns a dedicated
//! thread and publishes over a channel; [`PowerSampler::latest`] drains it without
//! blocking.
//!
//! macmon's error type is `Box<dyn Error>`, which is **not** `Send`, so it is stringified
//! inside the worker before it crosses the channel.
//!
//! Cluster labels come from [`macmon::SocInfo`] rather than being hardcoded: they are `E`
//! and `P` on M1-M4, but `P` and `S` on M5+.
//!
//! NOTE(redd): not every power channel reports on every chip. On this M4, `cpu_power`,
//! `ane_power`, and `ram_power` read a constant `0.0` while `gpu_power` and `sys_power`
//! carry real figures -- verified against the upstream `macmon` binary, which reports the
//! same zeros. Anything drawing these must lead with `sys_power`/`gpu_power` and treat a
//! flat zero as "this chip does not report it" rather than "idle".
//!
//! [macmon]: https://github.com/vladkens/macmon

use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use macmon::{Metrics, Sampler};

/// Per-cluster CPU figures for one sampling interval.
#[derive(Clone, Debug, Default)]
pub struct ClusterData {
    /// UI label for this cluster, read from `SocInfo` (`E`/`P` on M1-M4, `P`/`S` on M5+).
    pub label: String,
    /// Cluster frequency in MHz.
    pub freq_mhz: u32,
    /// Fraction of the interval spent active, in `0.0..=1.0`.
    pub active_ratio: f32,
    /// Frequency-weighted active residency, in `0.0..=1.0`.
    pub scaled_ratio: f32,
}

/// One sample of Apple Silicon power and clock data.
///
/// All `*_power_w` fields are Watts; all `*_ratio` fields are `0.0..=1.0`, *not* percent.
#[derive(Clone, Debug, Default)]
pub struct PowerData {
    /// CPU package power, in Watts.
    pub cpu_power_w: f32,
    /// GPU power, in Watts.
    pub gpu_power_w: f32,
    /// Apple Neural Engine power, in Watts.
    pub ane_power_w: f32,
    /// DRAM power, in Watts.
    pub ram_power_w: f32,
    /// Sum of CPU, GPU, and ANE power, in Watts.
    pub all_power_w: f32,
    /// Whole-system power estimate in Watts. `None` on machines that do not report it.
    pub sys_power_w: Option<f32>,
    /// Efficiency-tier cluster (the lower tier, whatever it is called on this chip).
    pub ecpu: ClusterData,
    /// Performance-tier cluster (the higher tier).
    pub pcpu: ClusterData,
    /// GPU frequency in MHz.
    pub gpu_freq_mhz: u32,
    /// Fraction of the interval the GPU spent active, in `0.0..=1.0`.
    pub gpu_active_ratio: f32,
}

impl PowerData {
    /// Build from a raw macmon sample plus the static SoC labels.
    fn from_metrics(metrics: &Metrics, ecpu_label: &str, pcpu_label: &str) -> Self {
        Self {
            cpu_power_w: metrics.cpu_power,
            gpu_power_w: metrics.gpu_power,
            ane_power_w: metrics.ane_power,
            ram_power_w: metrics.ram_power,
            all_power_w: metrics.all_power,
            // macmon reports 0.0 rather than an absent value on machines with no system
            // power sensor. Treat that as "not available" so the UI can say so instead of
            // drawing a flat zero line.
            sys_power_w: (metrics.sys_power > 0.0).then_some(metrics.sys_power),
            ecpu: ClusterData {
                label: ecpu_label.to_owned(),
                freq_mhz: metrics.ecpu_freq_mhz,
                active_ratio: metrics.ecpu_active_ratio,
                scaled_ratio: metrics.ecpu_scaled_ratio,
            },
            pcpu: ClusterData {
                label: pcpu_label.to_owned(),
                freq_mhz: metrics.pcpu_freq_mhz,
                active_ratio: metrics.pcpu_active_ratio,
                scaled_ratio: metrics.pcpu_scaled_ratio,
            },
            gpu_freq_mhz: metrics.gpu_freq_mhz,
            gpu_active_ratio: metrics.gpu_active_ratio,
        }
    }
}

/// A message from the sampling worker.
type Sample = Result<PowerData, String>;

/// Owns the macmon sampling thread and hands back the most recent sample.
///
/// Dropping this closes the channel, which makes the worker exit after its current sample
/// completes -- there is no way to interrupt a `get_metrics` call already in flight, so
/// shutdown lags by up to one interval.
#[derive(Debug)]
pub struct PowerSampler {
    rx: Receiver<Sample>,
    /// Most recent successful sample, retained between collection ticks. bottom collects
    /// faster than a useful power interval, so most ticks find an empty channel.
    latest: Option<PowerData>,
    /// Set once the worker reports a failure. Sampling is not retried after this.
    error: Option<String>,
    /// Count of samples received. Lets a caller tell a fresh reading from the retained
    /// one `latest` hands back between intervals.
    received: u64,
}

impl PowerSampler {
    /// Spawn the sampling worker.
    ///
    /// `interval_ms` is how long each `get_metrics` call blocks for. macmon derives its
    /// figures from counter deltas over that window, so a very short interval is noisy.
    pub fn spawn(interval_ms: u32) -> Self {
        let (tx, rx) = mpsc::channel();

        thread::Builder::new()
            .name("mon-power".to_owned())
            .spawn(move || {
                // `Box<dyn Error>` is not `Send`, so stringify before every send.
                let mut sampler = match Sampler::new() {
                    Ok(sampler) => sampler,
                    Err(err) => {
                        let _ = tx.send(Err(format!("Could not start the power sampler: {err}")));
                        return;
                    }
                };

                let (ecpu_label, pcpu_label) = {
                    let soc = sampler.get_soc_info();
                    (soc.ecpu_label.clone(), soc.pcpu_label.clone())
                };

                loop {
                    let sample = match sampler.get_metrics(interval_ms) {
                        Ok(metrics) => {
                            Ok(PowerData::from_metrics(&metrics, &ecpu_label, &pcpu_label))
                        }
                        Err(err) => Err(format!("Could not sample power: {err}")),
                    };

                    let failed = sample.is_err();

                    // A send error means the receiver went away, so there is nobody left
                    // to sample for.
                    if tx.send(sample).is_err() || failed {
                        break;
                    }
                }
            })
            // A failure here means the OS refused a thread, which is not something this
            // widget can recover from or usefully retry.
            .map_or_else(
                |err| PowerSampler {
                    rx: mpsc::channel().1,
                    latest: None,
                    error: Some(format!("Could not spawn the power sampler thread: {err}")),
                    received: 0,
                },
                |_handle| PowerSampler {
                    rx,
                    latest: None,
                    error: None,
                    received: 0,
                },
            )
    }

    /// Drain whatever the worker has produced since the last call.
    ///
    /// Never blocks. Call this before reading [`Self::latest`], [`Self::received`], or
    /// [`Self::error`] -- they are plain getters and do not touch the channel themselves.
    pub fn poll(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Ok(data)) => {
                    self.latest = Some(data);
                    self.received += 1;
                }
                Ok(Err(err)) => {
                    self.error = Some(err);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.error.is_none() {
                        self.error = Some("The power sampler stopped unexpectedly.".to_owned());
                    }
                    break;
                }
            }
        }
    }

    /// The most recent sample, or `None` until the first interval closes.
    ///
    /// Deliberately retains the previous reading between intervals -- bottom collects far
    /// faster than a useful power interval, so most ticks have nothing new. Compare
    /// [`Self::received`] against a stored value to tell fresh from retained.
    pub fn latest(&self) -> Option<&PowerData> {
        self.latest.as_ref()
    }

    /// The reason sampling is not working, if it is not working.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// How many samples have arrived so far.
    pub fn received(&self) -> u64 {
        self.received
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sys_power_of_zero_reads_as_unavailable() {
        let absent = Metrics::default();
        let data = PowerData::from_metrics(&absent, "E", "P");
        assert_eq!(data.sys_power_w, None);

        let present = Metrics {
            sys_power: 12.5,
            ..Default::default()
        };
        let data = PowerData::from_metrics(&present, "E", "P");
        assert_eq!(data.sys_power_w, Some(12.5));
    }

    #[test]
    fn cluster_labels_come_from_soc_info_not_hardcoded() {
        // M5+ relabels the tiers to P/S. Nothing in here may assume E/P.
        let data = PowerData::from_metrics(&Metrics::default(), "P", "S");
        assert_eq!(data.ecpu.label, "P");
        assert_eq!(data.pcpu.label, "S");
    }
}
