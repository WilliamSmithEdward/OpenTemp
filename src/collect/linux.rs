//! Linux collector: `/proc` and `/sys` (hwmon, cpufreq, drm, power_supply).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::linux_parse::*;
use super::{Collector, ProcessCpuTracker, RateTracker, nvml::Nvml};
use crate::model::*;

pub struct LinuxCollector {
    info: StaticInfo,
    prev_stat: Option<ProcStat>,
    prev_ctxt: Option<(Instant, u64)>,
    net: RateTracker<(String, bool)>,
    disk: RateTracker<(String, u8)>,
    procs: ProcessCpuTracker,
    proc_io: RateTracker<(u32, u64, bool)>,
    nvml: Option<Nvml>,
    clk_tck: f64,
    page_size: u64,
    start: Instant,
}

fn read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok()
}

fn read_num<T: std::str::FromStr>(p: impl AsRef<Path>) -> Option<T> {
    read(p)?.trim().parse().ok()
}

fn dir_entries(p: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(p).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

impl LinuxCollector {
    pub fn new() -> Self {
        let cpuinfo = parse_cpuinfo(&read("/proc/cpuinfo").unwrap_or_default());
        let (brand, vendor, features) = super::cpuid_info();
        let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let max_khz: Option<f64> = read_num("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq");
        let base_khz: Option<f64> = read_num("/sys/devices/system/cpu/cpu0/cpufreq/base_frequency");
        let cache = |idx: u32| -> Option<u64> {
            let s = read(format!("/sys/devices/system/cpu/cpu0/cache/index{idx}/size"))?;
            let s = s.trim();
            let n: u64 = s.trim_end_matches(['K', 'M']).parse().ok()?;
            Some(if s.ends_with('M') { n * 1024 * 1024 } else { n * 1024 })
        };
        let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: valid out-pointer.
        unsafe { libc::uname(&mut uts) };
        let field = |f: &[libc::c_char]| {
            let b: Vec<u8> = f.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            String::from_utf8_lossy(&b).into_owned()
        };
        let mem = parse_meminfo(&read("/proc/meminfo").unwrap_or_default());
        let info = StaticInfo {
            os_name: read("/etc/os-release").and_then(|t| parse_os_release(&t)).unwrap_or_else(|| "Linux".into()),
            os_version: field(&uts.release),
            kernel: format!("{} {}", field(&uts.sysname), field(&uts.release)),
            hostname: field(&uts.nodename),
            arch: std::env::consts::ARCH.into(),
            cpu: CpuInfo {
                model: brand.or(cpuinfo.model).unwrap_or_else(|| "Unknown processor".into()),
                vendor: vendor.or(cpuinfo.vendor).unwrap_or_default(),
                physical_cores: if cpuinfo.physical_cores > 0 { cpuinfo.physical_cores } else { logical },
                logical_cores: logical,
                sockets: cpuinfo.sockets.max(1),
                base_mhz: base_khz.map(|k| k / 1000.0),
                max_mhz: max_khz.map(|k| k / 1000.0),
                // index0/1 are L1d/L1i per core; report the per-core L1d+L1i for core 0.
                l1_bytes: match (cache(0), cache(1)) {
                    (Some(a), Some(b)) => Some(a + b),
                    (a, b) => a.or(b),
                },
                l2_bytes: cache(2),
                l3_bytes: cache(3),
                core_classes: vec![None; logical],
                features,
            },
            memory_total: mem.get("MemTotal").copied().unwrap_or(0),
        };
        // SAFETY: sysconf is always safe to call.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
        Self {
            info,
            prev_stat: None,
            prev_ctxt: None,
            net: RateTracker::default(),
            disk: RateTracker::default(),
            procs: ProcessCpuTracker::default(),
            proc_io: RateTracker::default(),
            nvml: Nvml::load(),
            clk_tck,
            page_size,
            start: Instant::now(),
        }
    }

    fn hwmon(&self) -> Vec<Sensor> {
        let mut out = Vec::new();
        for dir in dir_entries("/sys/class/hwmon") {
            let chip = read(dir.join("name")).map(|s| s.trim().to_string()).unwrap_or_else(|| "hwmon".into());
            let hw = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            for f in dir_entries(&dir) {
                let Some(fname) = f.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
                let Some(base) = fname.strip_suffix("_input") else { continue };
                let (kind, scale) = match base.trim_end_matches(|c: char| c.is_ascii_digit()) {
                    "temp" => (SensorKind::Temperature, 1000.0),
                    "fan" => (SensorKind::Fan, 1.0),
                    "in" => (SensorKind::Voltage, 1000.0),
                    "curr" => (SensorKind::Current, 1000.0),
                    "power" => (SensorKind::Power, 1_000_000.0),
                    _ => continue,
                };
                let Some(raw) = read_num::<f64>(&f) else { continue };
                let label = read(dir.join(format!("{base}_label"))).map(|s| s.trim().to_string()).unwrap_or_else(|| base.to_string());
                let lim = |s: &str| read_num::<f64>(dir.join(format!("{base}_{s}"))).map(|v| v / scale).filter(|v| *v > 0.0);
                out.push(Sensor {
                    id: format!("{hw}.{base}"),
                    group: chip.clone(),
                    label,
                    kind,
                    value: raw / scale,
                    high: if kind == SensorKind::Temperature { lim("max") } else { None },
                    critical: if kind == SensorKind::Temperature { lim("crit") } else { None },
                });
            }
            // Some drivers expose power as *_average only.
            for f in dir_entries(&dir) {
                let Some(fname) = f.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
                if let Some(base) = fname.strip_suffix("_average").filter(|b| b.starts_with("power")) {
                    if out.iter().any(|s| s.id == format!("{hw}.{base}")) {
                        continue;
                    }
                    if let Some(raw) = read_num::<f64>(&f) {
                        let label = read(dir.join(format!("{base}_label"))).map(|s| s.trim().to_string()).unwrap_or_else(|| base.to_string());
                        out.push(Sensor { id: format!("{hw}.{base}"), group: chip.clone(), label, kind: SensorKind::Power, value: raw / 1e6, high: None, critical: None });
                    }
                }
            }
        }
        if !out.iter().any(|s| s.kind == SensorKind::Temperature) {
            for z in dir_entries("/sys/class/thermal").into_iter().filter(|p| p.to_string_lossy().contains("thermal_zone")) {
                if let Some(t) = read_num::<f64>(z.join("temp")) {
                    let ty = read(z.join("type")).map(|s| s.trim().to_string()).unwrap_or_default();
                    let id = z.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let group = if ty.contains("cpu") || ty == "x86_pkg_temp" { "cpu_thermal".to_string() } else { "acpitz".to_string() };
                    out.push(Sensor { id, group, label: ty, kind: SensorKind::Temperature, value: t / 1000.0, high: None, critical: None });
                }
            }
        }
        out
    }

    fn gpus(&self) -> Vec<GpuSample> {
        let mut out = Vec::new();
        for card in dir_entries("/sys/class/drm") {
            let name = card.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if !name.starts_with("card") || name.contains('-') {
                continue;
            }
            let dev = card.join("device");
            let vendor_id = read(dev.join("vendor")).and_then(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()).unwrap_or(0);
            let vendor = GpuVendor::from_pci_id(vendor_id);
            if vendor == GpuVendor::Nvidia {
                continue; // NVML covers NVIDIA.
            }
            let mut g = GpuSample {
                key: name.clone(),
                name: read(dev.join("product_name")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
                    .unwrap_or_else(|| format!("{} GPU ({name})", vendor.label())),
                vendor,
                source: "sysfs".into(),
                ..Default::default()
            };
            g.usage = read_num::<f32>(dev.join("gpu_busy_percent"));
            if let Some(u) = g.usage {
                g.engines.push(("Graphics".into(), u));
            }
            g.dedicated_used = read_num(dev.join("mem_info_vram_used"));
            g.dedicated_total = read_num(dev.join("mem_info_vram_total"));
            g.shared_used = read_num(dev.join("mem_info_gtt_used"));
            g.shared_total = read_num(dev.join("mem_info_gtt_total"));
            g.integrated = vendor == GpuVendor::Intel || g.dedicated_total.is_some_and(|t| t <= 2 << 30);
            if let Some(t) = read(dev.join("pp_dpm_sclk")) {
                g.core_mhz = parse_dpm_active(&t);
                g.core_max_mhz = parse_dpm_max(&t);
            }
            if let Some(t) = read(dev.join("pp_dpm_mclk")) {
                g.memory_mhz = parse_dpm_active(&t);
            }
            if g.core_mhz.is_none() {
                g.core_mhz = read_num(card.join("gt_act_freq_mhz"));
                g.core_max_mhz = read_num(card.join("gt_max_freq_mhz"));
            }
            if let Some(hw) = dir_entries(dev.join("hwmon")).into_iter().next() {
                g.temperature = read_num::<f32>(hw.join("temp1_input")).map(|v| v / 1000.0);
                g.power_w = read_num::<f32>(hw.join("power1_average")).or_else(|| read_num(hw.join("power1_input"))).map(|v| v / 1e6);
                g.power_limit_w = read_num::<f32>(hw.join("power1_cap")).map(|v| v / 1e6);
                g.fan_rpm = read_num(hw.join("fan1_input"));
            }
            g.driver = fs::read_link(dev.join("driver")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            out.push(g);
        }
        out
    }

    fn disks(&mut self, now: Instant) -> Vec<DiskSample> {
        let stats = parse_diskstats(&read("/proc/diskstats").unwrap_or_default());
        let mut out = Vec::new();
        for (dev, mnt, fs) in parse_mounts(&read("/proc/mounts").unwrap_or_default()) {
            let c = std::ffi::CString::new(mnt.clone()).unwrap_or_default();
            let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
            // SAFETY: valid C string and out-pointer.
            if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
                continue;
            }
            let total = st.f_blocks as u64 * st.f_frsize as u64;
            if total == 0 {
                continue;
            }
            let real = fs::canonicalize(&dev).unwrap_or_else(|_| PathBuf::from(&dev));
            let kname = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (mut r, mut w, mut busy) = (None, None, None);
            if let Some(s) = stats.iter().find(|s| s.name == kname) {
                r = self.disk.rate(&(kname.clone(), 0), s.sectors_read * 512, now);
                w = self.disk.rate(&(kname.clone(), 1), s.sectors_written * 512, now);
                busy = self.disk.rate(&(kname.clone(), 2), s.io_ms, now).map(|ms| (ms / 10.0).clamp(0.0, 100.0) as f32);
            }
            let removable = Path::new("/sys/class/block").join(&kname).join("removable");
            out.push(DiskSample {
                key: mnt.clone(),
                name: format!("{kname} ({mnt})"),
                mount: mnt,
                fs,
                total,
                free: st.f_bavail as u64 * st.f_frsize as u64,
                removable: read_num::<u8>(removable) == Some(1),
                read_bps: r,
                write_bps: w,
                busy_pct: busy,
            });
        }
        out
    }

    fn networks(&mut self, now: Instant) -> Vec<NetSample> {
        let mut out = Vec::new();
        for (name, rx, tx) in parse_net_dev(&read("/proc/net/dev").unwrap_or_default()) {
            let sys = Path::new("/sys/class/net").join(&name);
            if name == "lo" || fs::canonicalize(&sys).map(|p| p.starts_with("/sys/devices/virtual")).unwrap_or(false) {
                continue;
            }
            let up = read(sys.join("operstate")).is_some_and(|s| s.trim() == "up");
            let speed: Option<i64> = read_num(sys.join("speed"));
            out.push(NetSample {
                key: name.clone(),
                description: if sys.join("wireless").exists() { "Wireless".into() } else { "Ethernet".into() },
                name: name.clone(),
                up,
                rx_bps: self.net.rate(&(name.clone(), false), rx, now).unwrap_or(0.0),
                tx_bps: self.net.rate(&(name.clone(), true), tx, now).unwrap_or(0.0),
                rx_total: rx,
                tx_total: tx,
                link_bps: speed.filter(|s| *s > 0 && up).map(|s| s as u64 * 1_000_000),
                addresses: Vec::new(),
                mac: read(sys.join("address")).map(|s| s.trim().to_uppercase()),
            });
        }
        out
    }

    fn processes(&mut self, now: Instant) -> Vec<ProcessSample> {
        self.procs.begin(now);
        let mut out = Vec::new();
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            let Some(st) = read(entry.path().join("stat")).and_then(|t| parse_pid_stat(&t)) else { continue };
            let cpu = (st.utime + st.stime) as f64 / self.clk_tck;
            let pct = self.procs.percent(pid, st.start, cpu, self.info.cpu.logical_cores);
            let io = read(entry.path().join("io")).and_then(|t| parse_pid_io(&t));
            let (r, w) = match io {
                Some((r, w)) => (
                    self.proc_io.rate(&(pid, st.start, false), r, now),
                    self.proc_io.rate(&(pid, st.start, true), w, now),
                ),
                None => (None, None),
            };
            out.push(ProcessSample {
                pid,
                name: st.comm,
                cpu_pct: pct,
                memory: st.rss_pages * self.page_size,
                threads: st.threads,
                read_bps: r,
                write_bps: w,
            });
        }
        self.procs.end();
        let alive: std::collections::HashSet<u32> = out.iter().map(|p| p.pid).collect();
        self.proc_io.retain(|k| alive.contains(&k.0));
        out
    }

    fn battery(&self) -> Option<Battery> {
        let bat = dir_entries("/sys/class/power_supply")
            .into_iter()
            .find(|p| read(p.join("type")).is_some_and(|t| t.trim() == "Battery"))?;
        let status = read(bat.join("status")).unwrap_or_default();
        let on_ac = dir_entries("/sys/class/power_supply")
            .iter()
            .any(|p| read(p.join("type")).is_some_and(|t| t.trim() == "Mains") && read_num::<u8>(p.join("online")) == Some(1));
        let power = read_num::<f32>(bat.join("power_now")).map(|v| v / 1e6).or_else(|| {
            let c = read_num::<f32>(bat.join("current_now"))?;
            let v = read_num::<f32>(bat.join("voltage_now"))?;
            Some(c * v / 1e12)
        });
        let full = read_num::<f32>(bat.join("energy_full")).or_else(|| read_num(bat.join("charge_full")));
        let design = read_num::<f32>(bat.join("energy_full_design")).or_else(|| read_num(bat.join("charge_full_design")));
        Some(Battery {
            percent: read_num(bat.join("capacity")),
            charging: status.trim() == "Charging",
            on_ac,
            seconds_left: None,
            power_w: power.filter(|p| *p > 0.0),
            health_pct: match (full, design) {
                (Some(f), Some(d)) if d > 0.0 => Some(f / d * 100.0),
                _ => None,
            },
        })
    }
}

impl Collector for LinuxCollector {
    fn static_info(&self) -> StaticInfo {
        self.info.clone()
    }

    fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let mut s = Snapshot { t: now.duration_since(self.start).as_secs_f64(), ..Default::default() };

        // CPU
        let stat = parse_proc_stat(&read("/proc/stat").unwrap_or_default());
        let mut freqs: Vec<Option<f64>> = (0..stat.cores.len())
            .map(|i| read_num::<f64>(format!("/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq")).map(|k| k / 1000.0))
            .collect();
        if freqs.iter().all(|f| f.is_none()) {
            let ci = parse_cpuinfo(&read("/proc/cpuinfo").unwrap_or_default());
            if ci.mhz.len() == freqs.len() {
                freqs = ci.mhz.into_iter().map(Some).collect();
            }
        }
        if let Some(prev) = &self.prev_stat {
            s.cpu.usage = Some(stat.total.usage_since(&prev.total));
            s.cpu.per_core = stat
                .cores
                .iter()
                .zip(prev.cores.iter())
                .enumerate()
                .map(|(i, (c, p))| CoreSample { usage: c.usage_since(p), mhz: freqs.get(i).copied().flatten() })
                .collect();
        }
        let known: Vec<f64> = freqs.iter().flatten().copied().collect();
        if !known.is_empty() {
            s.cpu.avg_mhz = Some(known.iter().sum::<f64>() / known.len() as f64);
            s.cpu.max_core_mhz = known.iter().copied().reduce(f64::max);
        }
        if let Some(c) = stat.ctxt {
            if let Some((t, prev)) = self.prev_ctxt {
                let dt = now.duration_since(t).as_secs_f64();
                if dt > 0.0 && c >= prev {
                    s.system.context_switches_per_sec = Some((c - prev) as f64 / dt);
                }
            }
            self.prev_ctxt = Some((now, c));
        }
        self.prev_stat = Some(stat);
        if let Some((l, total)) = read("/proc/loadavg").and_then(|t| parse_loadavg(&t)) {
            s.cpu.load_avg = Some(l);
            s.system.threads = total;
        }
        s.system.uptime_secs = read("/proc/uptime").and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok()).map(|v| v as u64);

        // Memory
        let m = parse_meminfo(&read("/proc/meminfo").unwrap_or_default());
        let g = |k: &str| m.get(k).copied();
        s.memory.total = g("MemTotal").unwrap_or(0);
        s.memory.available = g("MemAvailable").or_else(|| Some(g("MemFree")? + g("Cached")?)).unwrap_or(0);
        s.memory.used = s.memory.total.saturating_sub(s.memory.available);
        s.memory.cached = Some(g("Cached").unwrap_or(0) + g("Buffers").unwrap_or(0) + g("SReclaimable").unwrap_or(0));
        s.memory.committed = g("Committed_AS");
        s.memory.commit_limit = g("CommitLimit");
        s.memory.swap_total = g("SwapTotal");
        s.memory.swap_used = Some(g("SwapTotal").unwrap_or(0).saturating_sub(g("SwapFree").unwrap_or(0)));

        // Sensors, GPUs
        s.sensors = self.hwmon();
        if let Some((t, src)) = super::pick_cpu_temperature(&s.sensors) {
            s.cpu.temperature = Some(t);
            s.cpu.temperature_source = Some(src);
        } else {
            s.notes.push(Note::new("cpu", "No CPU temperature sensor found. Load the k10temp or coretemp module, or run sensors-detect."));
        }
        if let Some(p) = s.sensors.iter().find(|x| x.kind == SensorKind::Power && (x.group == "zenpower" || x.label.contains("SVI2_P_Core") || x.label.contains("CPU"))) {
            s.cpu.package_power_w = Some(p.value as f32);
        }
        s.gpus = self.gpus();
        if let Some(nv) = &mut self.nvml {
            s.gpus.extend(nv.sample());
        }

        s.disks = self.disks(now);
        s.networks = self.networks(now);
        s.processes = self.processes(now);
        s.system.processes = Some(s.processes.len() as u32);
        s.battery = self.battery();
        s
    }
}
