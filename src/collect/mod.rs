//! Hardware collectors. Each platform module implements [`Collector`]; the
//! sampler thread owns one collector and calls [`Collector::sample`] on a timer.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

use crate::model::{Snapshot, StaticInfo};

pub mod linux_parse;
pub mod nvme;
pub mod nvml;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

pub trait Collector: Send {
    fn static_info(&self) -> StaticInfo;
    /// Gather one snapshot. Rates (bytes/s, CPU %) are computed against the
    /// previous call, so the first call may return partial data.
    fn sample(&mut self) -> Snapshot;
}

/// Create the collector for the current platform.
pub fn platform() -> Box<dyn Collector> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsCollector::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxCollector::new())
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacCollector::new())
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Box::new(Unsupported)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
struct Unsupported;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
impl Collector for Unsupported {
    fn static_info(&self) -> StaticInfo {
        StaticInfo { os_name: std::env::consts::OS.into(), ..Default::default() }
    }
    fn sample(&mut self) -> Snapshot {
        let mut s = Snapshot::default();
        s.notes.push(crate::model::Note::new("system", "This operating system is not supported yet."));
        s
    }
}

/// Turns monotonically increasing counters into per-second rates.
///
/// Counter resets and 32-bit wraps are handled: a smaller value than last time
/// yields no rate for that interval instead of a huge spike.
pub struct RateTracker<K> {
    last: HashMap<K, (Instant, u64)>,
}

impl<K: Eq + Hash + Clone> Default for RateTracker<K> {
    fn default() -> Self {
        Self { last: HashMap::new() }
    }
}

impl<K: Eq + Hash + Clone> RateTracker<K> {
    pub fn rate(&mut self, key: &K, value: u64, now: Instant) -> Option<f64> {
        let prev = self.last.insert(key.clone(), (now, value));
        let (then, old) = prev?;
        let dt = now.duration_since(then).as_secs_f64();
        if dt <= 0.0 || value < old {
            return None;
        }
        Some((value - old) as f64 / dt)
    }

    /// Forget keys that were not seen in the latest pass.
    pub fn retain(&mut self, keep: impl Fn(&K) -> bool) {
        self.last.retain(|k, _| keep(k));
    }
}

/// CPU time accounting for processes, shared by all platforms.
///
/// Feed it each process's cumulative CPU time (any unit, given as seconds) and
/// it returns the share of the whole machine used since the last pass.
#[derive(Default)]
pub struct ProcessCpuTracker {
    last: HashMap<(u32, u64), f64>,
    seen: HashMap<(u32, u64), f64>,
    last_wall: Option<Instant>,
    wall_dt: f64,
}

impl ProcessCpuTracker {
    /// Call once at the start of each pass.
    pub fn begin(&mut self, now: Instant) {
        self.wall_dt = self.last_wall.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.last_wall = Some(now);
        self.seen.clear();
    }

    /// `start` disambiguates reused PIDs (creation time, or 0 if unknown).
    pub fn percent(&mut self, pid: u32, start: u64, cpu_secs: f64, logical_cpus: usize) -> f32 {
        let key = (pid, start);
        self.seen.insert(key, cpu_secs);
        let Some(prev) = self.last.get(&key) else { return 0.0 };
        if self.wall_dt <= 0.0 || cpu_secs < *prev {
            return 0.0;
        }
        let pct = (cpu_secs - prev) / (self.wall_dt * logical_cpus.max(1) as f64) * 100.0;
        pct.clamp(0.0, 100.0) as f32
    }

    /// Call once at the end of each pass.
    pub fn end(&mut self) {
        std::mem::swap(&mut self.last, &mut self.seen);
    }
}

/// Keep only the `n` most interesting processes (by CPU, then memory) so the
/// UI never has to lay out thousands of rows per frame.
pub fn trim_processes(list: &mut Vec<crate::model::ProcessSample>, n: usize) {
    list.sort_by(|a, b| {
        b.cpu_pct
            .partial_cmp(&a.cpu_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.memory.cmp(&a.memory))
    });
    // Keep the top CPU users plus the top memory users, de-duplicated.
    if list.len() > n {
        let mut by_mem: Vec<usize> = (0..list.len()).collect();
        by_mem.sort_by(|&a, &b| list[b].memory.cmp(&list[a].memory));
        let mut keep = vec![false; list.len()];
        for i in 0..n / 2 {
            keep[i] = true;
        }
        for &i in by_mem.iter().take(n / 2) {
            keep[i] = true;
        }
        let mut i = 0;
        list.retain(|_| {
            let k = keep[i];
            i += 1;
            k
        });
    }
}

/// x86 CPU brand string and feature flags straight from CPUID.
#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub fn cpuid_info() -> (Option<String>, Option<String>, Vec<String>) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::{__cpuid, __cpuid_count};
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::{__cpuid, __cpuid_count};

    // SAFETY: CPUID is available on every x86_64 CPU and on all x86 CPUs Rust supports.
    unsafe {
        let leaf0 = __cpuid(0);
        let mut vendor = Vec::with_capacity(12);
        for r in [leaf0.ebx, leaf0.edx, leaf0.ecx] {
            vendor.extend_from_slice(&r.to_le_bytes());
        }
        let vendor = String::from_utf8_lossy(&vendor).trim().to_string();

        let ext_max = __cpuid(0x8000_0000).eax;
        let brand = if ext_max >= 0x8000_0004 {
            let mut bytes = Vec::with_capacity(48);
            for leaf in 0x8000_0002u32..=0x8000_0004 {
                let r = __cpuid(leaf);
                for v in [r.eax, r.ebx, r.ecx, r.edx] {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
            let s = String::from_utf8_lossy(&bytes).trim_matches(char::from(0)).trim().to_string();
            Some(s.split_whitespace().collect::<Vec<_>>().join(" "))
        } else {
            None
        };

        let mut features = Vec::new();
        if leaf0.eax >= 1 {
            let l1 = __cpuid(1);
            let checks = [
                (l1.edx, 26, "SSE2"),
                (l1.ecx, 0, "SSE3"),
                (l1.ecx, 19, "SSE4.1"),
                (l1.ecx, 20, "SSE4.2"),
                (l1.ecx, 25, "AES"),
                (l1.ecx, 28, "AVX"),
                (l1.ecx, 12, "FMA3"),
                (l1.edx, 28, "HTT"),
                (l1.ecx, 5, "VMX"),
            ];
            for (reg, bit, name) in checks {
                if reg & (1 << bit) != 0 {
                    features.push(name.to_string());
                }
            }
        }
        if leaf0.eax >= 7 {
            let l7 = __cpuid_count(7, 0);
            let checks = [
                (l7.ebx, 5, "AVX2"),
                (l7.ebx, 16, "AVX-512F"),
                (l7.ebx, 3, "BMI1"),
                (l7.ebx, 8, "BMI2"),
                (l7.ebx, 29, "SHA"),
            ];
            for (reg, bit, name) in checks {
                if reg & (1 << bit) != 0 {
                    features.push(name.to_string());
                }
            }
        }
        if ext_max >= 0x8000_0001 {
            let e1 = __cpuid(0x8000_0001);
            if e1.ecx & (1 << 2) != 0 {
                features.push("SVM".into());
            }
        }
        let vendor = match vendor.as_str() {
            "GenuineIntel" => "Intel".to_string(),
            "AuthenticAMD" => "AMD".to_string(),
            _ => vendor,
        };
        (brand, Some(vendor), features)
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
pub fn cpuid_info() -> (Option<String>, Option<String>, Vec<String>) {
    (None, None, Vec::new())
}

/// Pick the reading that best represents "the CPU temperature" from a list of
/// sensors, using well-known driver labels first.
pub fn pick_cpu_temperature(sensors: &[crate::model::Sensor]) -> Option<(f32, String)> {
    use crate::model::SensorKind;
    const PREFERRED: [(&str, &str); 8] = [
        ("k10temp", "Tctl"),
        ("k10temp", "Tdie"),
        ("zenpower", "Tdie"),
        ("coretemp", "Package id 0"),
        ("cpu_thermal", ""),
        ("SMC", "CPU"),
        ("acpitz", ""),
        ("ACPI", ""),
    ];
    let temps: Vec<_> = sensors.iter().filter(|s| s.kind == SensorKind::Temperature).collect();
    for (group, label) in PREFERRED {
        if let Some(s) = temps
            .iter()
            .find(|s| s.group.eq_ignore_ascii_case(group) && (label.is_empty() || s.label.starts_with(label)))
        {
            return Some((s.value as f32, format!("{} · {}", s.group, s.label)));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn rate_tracker_handles_first_sample_and_reset() {
        let mut r = RateTracker::<&str>::default();
        let t0 = Instant::now();
        assert_eq!(r.rate(&"a", 100, t0), None);
        let t1 = t0 + Duration::from_secs(2);
        assert_eq!(r.rate(&"a", 300, t1), Some(100.0));
        let t2 = t1 + Duration::from_secs(1);
        assert_eq!(r.rate(&"a", 10, t2), None, "a counter reset must not produce a spike");
        let t3 = t2 + Duration::from_secs(1);
        assert_eq!(r.rate(&"a", 20, t3), Some(10.0));
    }

    #[test]
    fn process_cpu_is_share_of_machine() {
        let mut p = ProcessCpuTracker::default();
        let t0 = Instant::now();
        p.begin(t0);
        assert_eq!(p.percent(1, 0, 10.0, 4), 0.0);
        p.end();
        p.begin(t0 + Duration::from_secs(1));
        // 2 CPU-seconds in 1 s on 4 CPUs = 50 %.
        assert!((p.percent(1, 0, 12.0, 4) - 50.0).abs() < 0.01);
        // A reused PID with a different start time starts from scratch.
        assert_eq!(p.percent(2, 7, 99.0, 4), 0.0);
        p.end();
    }

    #[test]
    fn trim_keeps_top_cpu_and_top_memory() {
        use crate::model::ProcessSample;
        let mut v: Vec<ProcessSample> = (0..10)
            .map(|i| ProcessSample { pid: i, cpu_pct: i as f32, memory: (10 - i) as u64, ..Default::default() })
            .collect();
        trim_processes(&mut v, 4);
        let pids: Vec<u32> = v.iter().map(|p| p.pid).collect();
        assert!(pids.contains(&9) && pids.contains(&8), "top CPU kept: {pids:?}");
        assert!(pids.contains(&0) && pids.contains(&1), "top memory kept: {pids:?}");
        assert_eq!(v.len(), 4);
    }
}
