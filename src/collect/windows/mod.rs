//! Windows collector. Uses only user-mode, documented APIs and needs no
//! administrator rights or kernel driver.

mod disk;
mod gpu;
mod info;
mod net;
mod pdh;
mod process;

use std::time::Instant;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};

use super::{Collector, nvml::Nvml};
use crate::model::*;

pub struct WindowsCollector {
    info: StaticInfo,
    query: Option<pdh::Query>,
    cpu_util: Option<pdh::Counter>,
    cpu_perf: Option<pdh::Counter>,
    cpu_base: Option<pdh::Counter>,
    ctx_switches: Option<pdh::Counter>,
    thermal: Option<pdh::Counter>,
    energy: Option<pdh::Counter>,
    paging: Option<pdh::Counter>,
    gpus: Option<gpu::Gpus>,
    disks: Option<disk::Disks>,
    net: net::Net,
    procs: process::Processes,
    nvml: Option<Nvml>,
    start: Instant,
}

// SAFETY: every handle inside is only touched by the sampler thread that owns the collector.
unsafe impl Send for WindowsCollector {}

impl WindowsCollector {
    pub fn new() -> Self {
        let mut info = info::static_info();
        info.memory_total = memory_status().map(|m| m.ullTotalPhys).unwrap_or(0);
        let query = pdh::Query::new();
        let add = |p: &str| query.as_ref().and_then(|q| q.add(p));
        let cpu_util = add(r"\Processor Information(*)\% Processor Utility")
            .or_else(|| add(r"\Processor Information(*)\% Processor Time"));
        let cpu_perf = add(r"\Processor Information(*)\% Processor Performance");
        let cpu_base = add(r"\Processor Information(_Total)\Processor Frequency");
        let ctx_switches = add(r"\System\Context Switches/sec");
        let thermal = add(r"\Thermal Zone Information(*)\High Precision Temperature");
        let energy = add(r"\Energy Meter(*)\Power");
        let paging = add(r"\Paging File(_Total)\% Usage");
        let gpus = query.as_ref().map(gpu::Gpus::new);
        let disks = query.as_ref().map(disk::Disks::new);
        if let Some(q) = &query {
            // Prime rate counters so the first real sample has values.
            q.collect();
        }
        Self {
            info,
            query,
            cpu_util,
            cpu_perf,
            cpu_base,
            ctx_switches,
            thermal,
            energy,
            paging,
            gpus,
            disks,
            net: net::Net::default(),
            procs: process::Processes::default(),
            nvml: Nvml::load(),
            start: Instant::now(),
        }
    }
}

fn memory_status() -> Option<MEMORYSTATUSEX> {
    let mut m = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: dwLength is set as required.
    unsafe { GlobalMemoryStatusEx(&mut m) }.ok().map(|_| m)
}

/// Sort PDH "group,index" processor instances numerically, dropping totals.
fn per_cpu(values: Vec<(String, f64)>) -> Vec<f64> {
    let mut v: Vec<((u32, u32), f64)> = values
        .into_iter()
        .filter_map(|(name, val)| {
            let (g, i) = name.split_once(',')?;
            Some(((g.parse().ok()?, i.parse().ok()?), val))
        })
        .collect();
    v.sort_by_key(|e| e.0);
    v.into_iter().map(|e| e.1).collect()
}

fn total_of(values: &[(String, f64)]) -> Option<f64> {
    values.iter().find(|(n, _)| n == "_Total").map(|(_, v)| *v)
}

impl Collector for WindowsCollector {
    fn static_info(&self) -> StaticInfo {
        self.info.clone()
    }

    fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let mut s = Snapshot { t: now.duration_since(self.start).as_secs_f64(), ..Default::default() };
        if let Some(q) = &self.query {
            q.collect();
        } else {
            s.notes.push(Note::new("system", "Performance counters are unavailable; most readings will be missing."));
        }

        // ---- CPU
        let util = self.cpu_util.map(|c| c.values()).unwrap_or_default();
        let perf = self.cpu_perf.map(|c| c.values()).unwrap_or_default();
        let base = self.cpu_base.and_then(|c| c.value()).filter(|v| *v > 0.0).or(self.info.cpu.base_mhz);
        let core_util = per_cpu(util.clone());
        let core_perf = per_cpu(perf.clone());
        s.cpu.usage = total_of(&util).map(|v| v.clamp(0.0, 100.0) as f32);
        s.cpu.per_core = core_util
            .iter()
            .enumerate()
            .map(|(i, u)| CoreSample {
                usage: u.clamp(0.0, 100.0) as f32,
                mhz: base.and_then(|b| core_perf.get(i).map(|p| b * p / 100.0)),
            })
            .collect();
        if let (Some(b), Some(p)) = (base, total_of(&perf)) {
            s.cpu.avg_mhz = Some(b * p / 100.0);
        }
        s.cpu.max_core_mhz = s.cpu.per_core.iter().filter_map(|c| c.mhz).fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))));

        // ---- Sensors: ACPI thermal zones
        let mut acpi_plausible = Vec::new();
        if let Some(c) = &self.thermal {
            for (inst, tenth_kelvin) in c.values() {
                let celsius = tenth_kelvin / 10.0 - 273.15;
                if !(-40.0..=150.0).contains(&celsius) {
                    continue;
                }
                let label = inst.rsplit('.').next().unwrap_or(&inst).to_string();
                let sensor = Sensor {
                    id: format!("acpi.{inst}"),
                    group: "ACPI".into(),
                    label: format!("Thermal zone {label}"),
                    kind: SensorKind::Temperature,
                    value: celsius,
                    high: None,
                    critical: None,
                };
                // Firmware often parks unused zones at exactly 20 °C (293.2 K).
                if celsius > 21.0 {
                    acpi_plausible.push(sensor.clone());
                }
                s.sensors.push(sensor);
            }
        }

        // ---- Sensors: RAPL energy meters (CPU package / core power, no driver needed)
        if let Some(c) = &self.energy {
            for (inst, mw) in c.values() {
                let lower = inst.to_ascii_lowercase();
                if lower == "_total" {
                    continue;
                }
                let watts = mw / 1000.0;
                let label = if lower.ends_with("_pkg") {
                    s.cpu.package_power_w = Some(watts as f32);
                    "CPU Package".to_string()
                } else if let Some(core) = lower.split('_').find_map(|p| p.strip_prefix("core")).filter(|n| !n.is_empty()) {
                    format!("Core {core}")
                } else if lower.ends_with("_pp1") || lower.contains("gpu") {
                    "Graphics".to_string()
                } else if lower.contains("dram") {
                    "DRAM".to_string()
                } else {
                    inst.clone()
                };
                s.sensors.push(Sensor {
                    id: format!("rapl.{lower}"),
                    group: "RAPL energy meter".into(),
                    label,
                    kind: SensorKind::Power,
                    value: watts,
                    high: None,
                    critical: None,
                });
            }
        }

        // ---- Memory
        if let Some(m) = memory_status() {
            s.memory.total = m.ullTotalPhys;
            s.memory.available = m.ullAvailPhys;
            s.memory.used = m.ullTotalPhys.saturating_sub(m.ullAvailPhys);
        }
        let mut pi = PERFORMANCE_INFORMATION { cb: size_of::<PERFORMANCE_INFORMATION>() as u32, ..Default::default() };
        // SAFETY: cb is set as required.
        if unsafe { GetPerformanceInfo(&mut pi, pi.cb) }.is_ok() {
            let page = pi.PageSize as u64;
            s.memory.cached = Some(pi.SystemCache as u64 * page);
            s.memory.committed = Some(pi.CommitTotal as u64 * page);
            s.memory.commit_limit = Some(pi.CommitLimit as u64 * page);
            let pagefile = (pi.CommitLimit as u64).saturating_sub(pi.PhysicalTotal as u64) * page;
            s.memory.swap_total = Some(pagefile);
            if let Some(pct) = self.paging.and_then(|c| c.value()) {
                s.memory.swap_used = Some((pagefile as f64 * pct / 100.0) as u64);
            }
        }

        // ---- GPUs
        if let Some(g) = &mut self.gpus {
            s.gpus = g.sample();
            if !g.has_counters() {
                s.notes.push(Note::new("gpu", "GPU performance counters are unavailable (WDDM 2.x driver required)."));
            }
        }
        if let Some(nv) = &mut self.nvml {
            for n in nv.sample() {
                // Prefer Windows' utilisation numbers; take what only NVML knows.
                match s.gpus.iter_mut().find(|g| g.vendor == GpuVendor::Nvidia && names_match(&g.name, &n.name)) {
                    Some(g) => g.fill_from(&n),
                    None => s.gpus.push(n),
                }
            }
        }

        // ---- Storage
        if let Some(d) = &self.disks {
            s.disks = d.sample();
            let (sensors, drives) = d.health();
            s.sensors.extend(sensors);
            s.drives = drives;
        }

        // ---- Network
        s.networks = self.net.sample(now);

        // ---- Processes & system totals
        let (procs, totals) = self.procs.sample(self.info.cpu.logical_cores, now);
        s.processes = procs;
        if let Some(t) = totals {
            s.system.processes = Some(t.processes);
            s.system.threads = Some(t.threads);
            s.system.handles = Some(t.handles);
        }
        s.system.context_switches_per_sec = self.ctx_switches.and_then(|c| c.value());
        // SAFETY: trivial call.
        s.system.uptime_secs = Some(unsafe { GetTickCount64() } / 1000);

        // ---- Battery
        let mut ps = SYSTEM_POWER_STATUS::default();
        // SAFETY: valid out-pointer.
        if unsafe { GetSystemPowerStatus(&mut ps) }.is_ok() && ps.BatteryFlag != 128 && ps.BatteryFlag != 255 {
            s.battery = Some(Battery {
                percent: (ps.BatteryLifePercent <= 100).then_some(ps.BatteryLifePercent as f32),
                charging: ps.BatteryFlag & 8 != 0,
                on_ac: ps.ACLineStatus == 1,
                seconds_left: (ps.BatteryLifeTime != u32::MAX).then_some(ps.BatteryLifeTime as u64),
                power_w: None,
                health_pct: None,
            });
        }

        // ---- CPU temperature: the best we can do without a kernel driver.
        if let Some((t, src)) = super::pick_cpu_temperature(&acpi_plausible) {
            s.cpu.temperature = Some(t);
            s.cpu.temperature_source = Some(format!("{src} (firmware-reported)"));
        } else {
            s.notes.push(Note::new(
                "cpu",
                "CPU die temperature needs a kernel driver on Windows, which OpenTemp does not install. \
                 Package power, clocks, drive and GPU temperatures are shown where available.",
            ));
        }
        s
    }
}

fn names_match(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase().replace("nvidia", "").split_whitespace().collect::<Vec<_>>().join(" ");
    let (a, b) = (norm(a), norm(b));
    a == b || a.contains(&b) || b.contains(&a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_cpu_sorts_numerically_and_skips_totals() {
        let v = vec![
            ("0,10".to_string(), 10.0),
            ("0,2".to_string(), 2.0),
            ("_Total".to_string(), 50.0),
            ("0,_Total".to_string(), 50.0),
            ("1,0".to_string(), 100.0),
        ];
        assert_eq!(per_cpu(v), vec![2.0, 10.0, 100.0]);
    }

    #[test]
    fn nvidia_names_match_loosely() {
        assert!(names_match("NVIDIA GeForce RTX 4070", "GeForce RTX 4070"));
        assert!(!names_match("NVIDIA GeForce RTX 4070", "NVIDIA GeForce RTX 3060"));
    }
}
