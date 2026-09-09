use super::ProbeResult;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    process::Stdio,
};

#[derive(Clone, Default, Serialize)]
pub struct Counters {
    pub bytes: u64,
    pub video_frames: u64,
    pub audio_frames: u64,
    pub last_video_dts_ms: u32,
}

#[derive(Clone, Default, Serialize)]
pub struct SessionSnapshot {
    pub input: Counters,
    pub output: Counters,
    pub input_queue_events: usize,
    pub input_queue_capacity: usize,
    pub timestamp_lag_ms: u32,
    pub output_track_dts_ms: BTreeMap<u8, u32>,
    pub missing_video_tracks: usize,
    pub encode_groups: usize,
    pub video_decoders: usize,
    pub publishing_outputs: usize,
    pub failed_outputs: usize,
    pub started: bool,
    pub completed: bool,
}

#[derive(Serialize)]
pub struct Sample {
    pub elapsed_ms: u64,
    pub measurement_phase: bool,
    pub own_pids: Vec<u32>,
    pub own_cpu_percent: f64,
    pub own_rss_kib: u64,
    pub sessions: Vec<SessionSnapshot>,
    pub input_bytes_per_second: f64,
    pub output_bytes_per_second: f64,
    pub output_video_frames_per_second: f64,
    pub host: HostSample,
}

#[derive(Clone, Serialize)]
pub struct HostSample {
    pub busy_percent: f64,
    pub iowait_percent: f64,
    pub steal_percent: f64,
}

#[derive(Default)]
pub struct Host {
    previous: Option<[u64; 8]>,
    over_limit: u8,
}

impl Host {
    pub fn sample(&mut self) -> ProbeResult<Option<HostSample>> {
        let data =
            fs::read_to_string("/proc/stat").map_err(|_| "Host-CPU-Metrik ist nicht lesbar")?;
        let mut words = data
            .lines()
            .next()
            .ok_or("Host-CPU-Metrik fehlt")?
            .split_whitespace();
        if words.next() != Some("cpu") {
            return Err("Host-CPU-Metrik ist ungültig");
        }
        let mut current = [0; 8];
        for value in &mut current {
            *value = words
                .next()
                .and_then(|v| v.parse().ok())
                .ok_or("Host-CPU-Metrik ist ungültig")?;
        }
        let Some(previous) = self.previous.replace(current) else {
            return Ok(None);
        };
        let delta: Vec<_> = current
            .iter()
            .zip(previous)
            .map(|(now, before)| now.saturating_sub(before))
            .collect();
        let total = delta.iter().sum::<u64>() as f64;
        if total == 0.0 {
            return Err("Host-CPU-Metrik hat keine neue Zeitbasis");
        }
        Ok(Some(HostSample {
            busy_percent: (total - delta[3] as f64) / total * 100.0,
            iowait_percent: delta[4] as f64 / total * 100.0,
            steal_percent: delta[7] as f64 / total * 100.0,
        }))
    }

    pub fn must_stop(&mut self, busy_percent: f64) -> bool {
        self.over_limit = if busy_percent > 85.0 {
            self.over_limit.saturating_add(1)
        } else {
            0
        };
        self.over_limit >= 3
    }
}

#[derive(Serialize)]
pub struct Distribution {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub peak: f64,
}

pub fn distribution(values: impl Iterator<Item = f64>) -> Option<Distribution> {
    let mut values: Vec<_> = values.filter(|v| v.is_finite()).collect();
    values.sort_by(f64::total_cmp);
    let percentile = |p: usize| values[((values.len() * p).div_ceil(100)).saturating_sub(1)];
    values.last().copied().map(|peak| Distribution {
        p50: percentile(50),
        p95: percentile(95),
        p99: percentile(99),
        peak,
    })
}

pub fn descendants() -> ProbeResult<BTreeSet<u32>> {
    let mut found = BTreeSet::from([std::process::id()]);
    let mut pending = vec![std::process::id()];
    while let Some(pid) = pending.pop() {
        let threads = match fs::read_dir(format!("/proc/{pid}/task")) {
            Ok(threads) => threads,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("Eigene Prozessnachkommen sind nicht lesbar"),
        };
        for thread in threads {
            let thread = thread.map_err(|_| "Eigene Threadliste ist nicht lesbar")?;
            let content = match fs::read_to_string(thread.path().join("children")) {
                Ok(content) => content,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("Eigene Kindprozessliste ist nicht lesbar"),
            };
            for child in content.split_whitespace() {
                let child = child
                    .parse()
                    .map_err(|_| "Eigene Prozess-ID ist ungültig")?;
                if found.insert(child) {
                    pending.push(child);
                }
                if found.len() > 64 {
                    return Err("Lasttreiber hat mehr als 64 eigene Prozesse");
                }
            }
        }
    }
    Ok(found)
}

pub struct Processes {
    ticks_per_second: f64,
    previous: BTreeMap<(u32, u64), u64>,
}

impl Processes {
    pub async fn new() -> ProbeResult<Self> {
        let output = tokio::process::Command::new("/usr/bin/getconf")
            .arg("CLK_TCK")
            .env_clear()
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|_| "CPU-Zeitbasis konnte nicht gelesen werden")?;
        let ticks_per_second: f64 = std::str::from_utf8(&output.stdout)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .filter(|v: &f64| v.is_finite() && *v > 0.0)
            .ok_or("CPU-Zeitbasis ist ungültig")?;
        if !output.status.success() {
            return Err("CPU-Zeitbasis konnte nicht gelesen werden");
        }
        Ok(Self {
            ticks_per_second,
            previous: BTreeMap::new(),
        })
    }

    pub fn sample(&mut self, seconds: f64) -> ProbeResult<(Vec<u32>, f64, u64)> {
        let pids = descendants()?;
        let mut rss_kib = 0;
        let mut delta_ticks = 0;
        let mut current = BTreeMap::new();
        for pid in &pids {
            let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
                Ok(stat) => stat,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("Eigene CPU-Metrik ist nicht lesbar"),
            };
            let fields: Vec<_> = stat
                .rsplit_once(") ")
                .ok_or("Eigene CPU-Metrik ist ungültig")?
                .1
                .split_whitespace()
                .collect();
            let number = |index: usize| {
                fields
                    .get(index)
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("Eigene CPU-Metrik ist ungültig")
            };
            let ticks = number(11)? + number(12)?;
            let key = (*pid, number(19)?);
            delta_ticks += ticks.saturating_sub(self.previous.get(&key).copied().unwrap_or(0));
            current.insert(key, ticks);
            let status = match fs::read_to_string(format!("/proc/{pid}/status")) {
                Ok(status) => status,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("Eigene RSS-Metrik ist nicht lesbar"),
            };
            let rss = status.lines().find_map(|line| {
                line.strip_prefix("VmRSS:")
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<u64>().ok())
            });
            match rss {
                Some(rss) => rss_kib += rss,
                None if fields.first() == Some(&"Z") => {}
                None => return Err("Eigene RSS-Metrik fehlt oder ist ungültig"),
            }
        }
        self.previous = current;
        Ok((
            pids.into_iter().collect(),
            delta_ticks as f64 / self.ticks_per_second / seconds * 100.0,
            rss_kib,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_threshold_needs_three_consecutive_samples_and_resets() {
        let mut host = Host::default();
        assert!(!host.must_stop(86.0));
        assert!(!host.must_stop(99.0));
        assert!(!host.must_stop(85.0));
        assert!(!host.must_stop(90.0));
        assert!(!host.must_stop(90.0));
        assert!(host.must_stop(90.0));
    }

    #[test]
    fn percentiles_use_bounded_observed_values() {
        assert!(distribution(std::iter::empty()).is_none());
        let values = distribution((1..=100).map(f64::from)).unwrap();
        assert_eq!(
            (values.p50, values.p95, values.p99, values.peak),
            (50.0, 95.0, 99.0, 100.0)
        );
    }
}
