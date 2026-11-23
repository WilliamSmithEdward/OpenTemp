//! macOS collector: Mach host statistics, sysctl, getifaddrs, getmntinfo,
//! libproc, and the System Management Controller for temperatures, fans and power.
//!
//! GPU load and battery readings are not implemented yet; the UI explains this
//! through `Snapshot::notes`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::time::Instant;

use super::smc::{self, Smc};
use super::{Collector, ProcessCpuTracker, RateTracker};
use crate::model::*;

type MachPort = u32;
type KernReturn = c_int;

const PROCESSOR_CPU_LOAD_INFO: c_int = 2;
const HOST_VM_INFO64: c_int = 4;
const CPU_STATE_USER: usize = 0;
const CPU_STATE_SYSTEM: usize = 1;
const CPU_STATE_IDLE: usize = 2;
const CPU_STATE_NICE: usize = 3;

#[repr(C)]
#[derive(Default)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

#[repr(C)]
#[derive(Default)]
struct XswUsage {
    total: u64,
    avail: u64,
    used: u64,
    pagesize: u32,
    encrypted: u8,
}

#[repr(C)]
#[derive(Default)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    static mach_task_self_: MachPort;
    fn mach_host_self() -> MachPort;
    fn host_processor_info(
        host: MachPort,
        flavor: c_int,
        count: *mut u32,
        info: *mut *mut c_int,
        info_count: *mut u32,
    ) -> KernReturn;
    fn host_statistics64(host: MachPort, flavor: c_int, info: *mut c_int, count: *mut u32) -> KernReturn;
    fn vm_deallocate(task: MachPort, address: usize, size: usize) -> KernReturn;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> KernReturn;
}

fn sysctl_raw(name: &str, out: *mut c_void, len: &mut usize) -> bool {
    let c = CString::new(name).unwrap_or_default();
    // SAFETY: caller supplies a buffer of `len` bytes.
    unsafe { libc::sysctlbyname(c.as_ptr(), out, len, std::ptr::null_mut(), 0) == 0 }
}

fn sysctl_u64(name: &str) -> Option<u64> {
    let mut buf = [0u8; 8];
    let mut len = buf.len();
    if !sysctl_raw(name, buf.as_mut_ptr().cast(), &mut len) {
        return None;
    }
    Some(match len {
        4 => u32::from_ne_bytes(buf[..4].try_into().ok()?) as u64,
        8 => u64::from_ne_bytes(buf),
        _ => return None,
    })
}

fn sysctl_string(name: &str) -> Option<String> {
    let mut len = 0usize;
    if !sysctl_raw(name, std::ptr::null_mut(), &mut len) || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    if !sysctl_raw(name, buf.as_mut_ptr().cast(), &mut len) {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).trim().to_string())
}

pub struct MacCollector {
    info: StaticInfo,
    prev_ticks: Option<Vec<[u64; 4]>>,
    net: RateTracker<(String, bool)>,
    procs: ProcessCpuTracker,
    smc: Option<SmcSensors>,
    timebase: f64,
    page_size: u64,
    start: Instant,
}

impl MacCollector {
    pub fn new() -> Self {
        let (brand, vendor, features) = super::cpuid_info();
        let logical = sysctl_u64("hw.logicalcpu").unwrap_or(1) as usize;
        let info = StaticInfo {
            os_name: format!("macOS {}", sysctl_string("kern.osproductversion").unwrap_or_default()),
            os_version: sysctl_string("kern.osversion").unwrap_or_default(),
            kernel: format!("Darwin {}", sysctl_string("kern.osrelease").unwrap_or_default()),
            hostname: sysctl_string("kern.hostname").unwrap_or_default(),
            arch: std::env::consts::ARCH.into(),
            cpu: CpuInfo {
                model: sysctl_string("machdep.cpu.brand_string")
                    .or(brand)
                    .unwrap_or_else(|| "Unknown processor".into()),
                vendor: vendor
                    .unwrap_or_else(|| if cfg!(target_arch = "aarch64") { "Apple".into() } else { String::new() }),
                physical_cores: sysctl_u64("hw.physicalcpu").unwrap_or(logical as u64) as usize,
                logical_cores: logical,
                sockets: sysctl_u64("hw.packages").unwrap_or(1) as usize,
                base_mhz: sysctl_u64("hw.cpufrequency").map(|hz| hz as f64 / 1e6),
                max_mhz: sysctl_u64("hw.cpufrequency_max").map(|hz| hz as f64 / 1e6),
                l1_bytes: sysctl_u64("hw.l1dcachesize"),
                l2_bytes: sysctl_u64("hw.l2cachesize"),
                l3_bytes: sysctl_u64("hw.l3cachesize").filter(|v| *v > 0),
                core_classes: vec![None; logical],
                features,
            },
            memory_total: sysctl_u64("hw.memsize").unwrap_or(0),
        };
        let mut tb = TimebaseInfo::default();
        // SAFETY: valid out-pointer.
        unsafe { mach_timebase_info(&mut tb) };
        let timebase = if tb.denom == 0 { 1.0 } else { tb.numer as f64 / tb.denom as f64 };
        // SAFETY: sysconf is always safe.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
        Self {
            info,
            prev_ticks: None,
            net: RateTracker::default(),
            procs: ProcessCpuTracker::default(),
            smc: SmcSensors::discover(),
            timebase,
            page_size,
            start: Instant::now(),
        }
    }

    fn cpu_ticks() -> Option<Vec<[u64; 4]>> {
        let (mut count, mut info, mut info_count) = (0u32, std::ptr::null_mut::<c_int>(), 0u32);
        // SAFETY: out-pointers are valid; we free the returned array below.
        let kr = unsafe {
            host_processor_info(mach_host_self(), PROCESSOR_CPU_LOAD_INFO, &mut count, &mut info, &mut info_count)
        };
        if kr != 0 || info.is_null() {
            return None;
        }
        // SAFETY: `info` holds `info_count` integers, 4 per CPU.
        let raw = unsafe { std::slice::from_raw_parts(info, info_count as usize) };
        let v = raw
            .as_chunks::<4>()
            .0
            .iter()
            .take(count as usize)
            .map(|c| {
                [
                    c[CPU_STATE_USER] as u32 as u64,
                    c[CPU_STATE_SYSTEM] as u32 as u64,
                    c[CPU_STATE_IDLE] as u32 as u64,
                    c[CPU_STATE_NICE] as u32 as u64,
                ]
            })
            .collect();
        // SAFETY: freeing the kernel-allocated array.
        unsafe { vm_deallocate(mach_task_self_, info as usize, info_count as usize * size_of::<c_int>()) };
        Some(v)
    }

    fn memory(&self) -> MemorySample {
        let mut m = MemorySample { total: self.info.memory_total, ..Default::default() };
        let mut vm = VmStatistics64::default();
        let mut count = (size_of::<VmStatistics64>() / size_of::<c_int>()) as u32;
        // SAFETY: buffer and count describe `vm`.
        if unsafe {
            host_statistics64(mach_host_self(), HOST_VM_INFO64, (&mut vm as *mut VmStatistics64).cast(), &mut count)
        } == 0
        {
            let p = self.page_size;
            // Activity Monitor's "Memory Used": app memory + wired + compressed.
            let app = (vm.internal_page_count as u64).saturating_sub(vm.purgeable_count as u64);
            m.used = (app + vm.wire_count as u64 + vm.compressor_page_count as u64) * p;
            m.available = m.total.saturating_sub(m.used);
            m.cached = Some((vm.external_page_count as u64 + vm.purgeable_count as u64) * p);
        }
        let mut sw = XswUsage::default();
        let mut len = size_of::<XswUsage>();
        if sysctl_raw("vm.swapusage", (&mut sw as *mut XswUsage).cast(), &mut len) {
            m.swap_total = Some(sw.total);
            m.swap_used = Some(sw.used);
        }
        m
    }

    fn networks(&mut self, now: Instant) -> Vec<NetSample> {
        let mut out: Vec<NetSample> = Vec::new();
        let mut addrs: std::collections::HashMap<String, Vec<String>> = Default::default();
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs allocates the list; freed below.
        if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            // SAFETY: walking the list getifaddrs returned.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() {
                continue;
            }
            let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned();
            if !(name.starts_with("en") || name.starts_with("eth")) {
                continue;
            }
            let family = unsafe { (*ifa.ifa_addr).sa_family } as c_int;
            if family == libc::AF_INET {
                let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
                addrs
                    .entry(name.clone())
                    .or_default()
                    .push(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
            } else if family == libc::AF_LINK && !ifa.ifa_data.is_null() {
                // SAFETY: for AF_LINK entries ifa_data points to `if_data`.
                let d = unsafe { &*(ifa.ifa_data as *const libc::if_data) };
                let up = ifa.ifa_flags & (libc::IFF_UP as u32) != 0 && ifa.ifa_flags & (libc::IFF_RUNNING as u32) != 0;
                let (rx, tx) = (d.ifi_ibytes as u64, d.ifi_obytes as u64);
                out.push(NetSample {
                    key: name.clone(),
                    name: name.clone(),
                    description: "Network interface".into(),
                    up,
                    rx_bps: self.net.rate(&(name.clone(), false), rx, now).unwrap_or(0.0),
                    tx_bps: self.net.rate(&(name.clone(), true), tx, now).unwrap_or(0.0),
                    rx_total: rx,
                    tx_total: tx,
                    link_bps: (up && d.ifi_baudrate > 0).then_some(d.ifi_baudrate as u64),
                    addresses: Vec::new(),
                    mac: None,
                });
            }
        }
        // SAFETY: freeing what getifaddrs allocated.
        unsafe { libc::freeifaddrs(ifap) };
        for n in &mut out {
            n.addresses = addrs.remove(&n.name).unwrap_or_default();
        }
        out
    }

    fn disks() -> Vec<DiskSample> {
        let mut mounts: *mut libc::statfs = std::ptr::null_mut();
        // SAFETY: getmntinfo returns a pointer into a static buffer it owns.
        let n = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
        if n <= 0 || mounts.is_null() {
            return Vec::new();
        }
        let list = unsafe { std::slice::from_raw_parts(mounts, n as usize) };
        let cstr = |b: &[c_char]| {
            let v: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            String::from_utf8_lossy(&v).into_owned()
        };
        list.iter()
            .filter(|m| m.f_flags & libc::MNT_LOCAL as u32 != 0)
            .filter_map(|m| {
                let mnt = cstr(&m.f_mntonname);
                let fs = cstr(&m.f_fstypename);
                if !(mnt == "/" || mnt.starts_with("/Volumes/")) || fs == "devfs" || fs == "autofs" {
                    return None;
                }
                let total = m.f_blocks * m.f_bsize as u64;
                (total > 0).then(|| DiskSample {
                    key: mnt.clone(),
                    name: if mnt == "/" {
                        "Macintosh HD".into()
                    } else {
                        mnt.trim_start_matches("/Volumes/").to_string()
                    },
                    mount: mnt.clone(),
                    fs,
                    total,
                    free: m.f_bavail * m.f_bsize as u64,
                    removable: mnt.starts_with("/Volumes/"),
                    read_bps: None,
                    write_bps: None,
                    busy_pct: None,
                })
            })
            .collect()
    }

    fn processes(&mut self, now: Instant) -> Vec<ProcessSample> {
        self.procs.begin(now);
        let mut pids = vec![0 as c_int; 4096];
        // SAFETY: buffer size is given in bytes.
        let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * size_of::<c_int>()) as c_int) };
        let mut out = Vec::new();
        for &pid in pids.iter().take(n.max(0) as usize) {
            if pid <= 0 {
                continue;
            }
            let mut ti: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
            let size = size_of::<libc::proc_taskinfo>() as c_int;
            // SAFETY: buffer is a proc_taskinfo of `size` bytes.
            if unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&mut ti as *mut libc::proc_taskinfo).cast(), size)
            } != size
            {
                continue;
            }
            let mut name = [0 as c_char; 256];
            // SAFETY: buffer of the given size.
            unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
            let name = unsafe { CStr::from_ptr(name.as_ptr()) }.to_string_lossy().into_owned();
            let cpu_ns = (ti.pti_total_user + ti.pti_total_system) as f64 * self.timebase;
            let pct = self.procs.percent(pid as u32, 0, cpu_ns / 1e9, self.info.cpu.logical_cores);
            out.push(ProcessSample {
                pid: pid as u32,
                name: if name.is_empty() { format!("PID {pid}") } else { name },
                cpu_pct: pct,
                memory: ti.pti_resident_size,
                threads: ti.pti_threadnum as u32,
                read_bps: None,
                write_bps: None,
            });
        }
        self.procs.end();
        out
    }
}

impl Collector for MacCollector {
    fn static_info(&self) -> StaticInfo {
        self.info.clone()
    }

    fn sample(&mut self) -> Snapshot {
        let now = Instant::now();
        let mut s = Snapshot { t: now.duration_since(self.start).as_secs_f64(), ..Default::default() };
        if let Some(ticks) = Self::cpu_ticks() {
            if let Some(prev) = &self.prev_ticks {
                let usage = |c: &[u64; 4], p: &[u64; 4]| {
                    let d: Vec<u64> = (0..4).map(|i| c[i].wrapping_sub(p[i]) & 0xffff_ffff).collect();
                    let total: u64 = d.iter().sum();
                    if total == 0 { 0.0 } else { ((total - d[CPU_STATE_IDLE]) as f64 / total as f64 * 100.0) as f32 }
                };
                s.cpu.per_core =
                    ticks.iter().zip(prev).map(|(c, p)| CoreSample { usage: usage(c, p), mhz: None }).collect();
                if !s.cpu.per_core.is_empty() {
                    s.cpu.usage =
                        Some(s.cpu.per_core.iter().map(|c| c.usage).sum::<f32>() / s.cpu.per_core.len() as f32);
                }
            }
            self.prev_ticks = Some(ticks);
        }
        let mut la = [0f64; 3];
        // SAFETY: buffer of 3 doubles.
        if unsafe { libc::getloadavg(la.as_mut_ptr(), 3) } == 3 {
            s.cpu.load_avg = Some(la);
        }
        let mut tv = libc::timeval { tv_sec: 0, tv_usec: 0 };
        let mut len = size_of::<libc::timeval>();
        if sysctl_raw("kern.boottime", (&mut tv as *mut libc::timeval).cast(), &mut len) {
            let now_s =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            s.system.uptime_secs = Some(now_s.saturating_sub(tv.tv_sec as u64));
        }
        s.memory = self.memory();
        s.networks = self.networks(now);
        s.disks = Self::disks();
        s.processes = self.processes(now);
        s.system.processes = Some(s.processes.len() as u32);
        s.system.threads = Some(s.processes.iter().map(|p| p.threads).sum());
        match &self.smc {
            Some(smc) => {
                let r = smc.sample();
                s.cpu.temperature = r.cpu_temp.as_ref().map(|t| t.0);
                s.cpu.temperature_source = r.cpu_temp.map(|t| t.1);
                s.cpu.package_power_w = r.cpu_power;
                s.sensors = r.sensors;
            }
            None => s.notes.push(Note::new(
                "cpu",
                "The System Management Controller could not be opened, so temperatures and fans are unavailable.",
            )),
        }
        s.notes.push(Note::new(
            "gpu",
            "GPU load and memory on macOS are not implemented yet; GPU temperatures appear under Sensors.",
        ));
        s
    }
}

// ------------------------------------------------------------------ SMC

struct SmcKey {
    name: String,
    key: u32,
    data_type: u32,
    size: u32,
}

/// The SMC keys this Mac answers, discovered once at start-up.
struct SmcSensors {
    smc: Smc,
    temps: Vec<SmcKey>,
    power: Vec<SmcKey>,
    fans: Vec<u32>,
}

struct SmcReading {
    sensors: Vec<Sensor>,
    cpu_temp: Option<(f32, String)>,
    cpu_power: Option<f32>,
}

impl SmcSensors {
    fn discover() -> Option<Self> {
        let smc = Smc::open()?;
        let mut temps = Vec::new();
        let mut power = Vec::new();
        for key in smc.keys() {
            let name = smc::fourcc_str(key);
            let is_temp = name.starts_with('T');
            let is_power = smc::power_label(&name).is_some();
            if !is_temp && !is_power {
                continue;
            }
            let Some((data_type, size)) = smc.key_info(key) else { continue };
            // Keep only keys that decode now; a temperature must also be plausible,
            // which filters out the many placeholder keys Apple silicon exposes.
            let Some(v) = smc.read(key, size).and_then(|b| smc::decode(data_type, &b)) else { continue };
            let k = SmcKey { name, key, data_type, size };
            if is_temp && smc::plausible_temp(v) {
                temps.push(k);
            } else if is_power && v >= 0.0 {
                power.push(k);
            }
        }
        let fan_count = smc.read_value("FNum").unwrap_or(0.0).clamp(0.0, 8.0) as u32;
        let fans = (0..fan_count).filter(|i| smc.read_value(&format!("F{i}Ac")).is_some()).collect();
        Some(Self { smc, temps, power, fans })
    }

    fn value(&self, k: &SmcKey) -> Option<f64> {
        smc::decode(k.data_type, &self.smc.read(k.key, k.size)?)
    }

    fn sample(&self) -> SmcReading {
        let mut sensors = Vec::new();
        let mut cpu_die = Vec::new();
        let mut cpu_any = Vec::new();
        for k in &self.temps {
            let Some(v) = self.value(k).filter(|v| smc::plausible_temp(*v)) else { continue };
            let (category, desc) = smc::temperature_label(&k.name);
            if category == "CPU" {
                cpu_any.push(v);
                if matches!(k.name.as_str(), "TC0D" | "TC0E" | "TC0F" | "TCXC") {
                    cpu_die.push(v);
                }
            }
            sensors.push(Sensor {
                id: format!("smc.{}", k.name),
                group: format!("SMC · {category}"),
                label: format!("{desc} ({})", k.name),
                kind: SensorKind::Temperature,
                value: v,
                high: None,
                critical: None,
            });
        }
        let mut cpu_power = None;
        for k in &self.power {
            let Some(v) = self.value(k) else { continue };
            if (k.name == "PCPT" || (k.name == "PCPC" && cpu_power.is_none())) && v > 0.0 {
                cpu_power = Some(v as f32);
            }
            sensors.push(Sensor {
                id: format!("smc.{}", k.name),
                group: "SMC · Power".into(),
                label: format!("{} ({})", smc::power_label(&k.name).unwrap_or("Power"), k.name),
                kind: SensorKind::Power,
                value: v,
                high: None,
                critical: None,
            });
        }
        for &i in &self.fans {
            let Some(rpm) = self.smc.read_value(&format!("F{i}Ac")) else { continue };
            let max = self.smc.read_value(&format!("F{i}Mx")).filter(|m| *m > 0.0);
            sensors.push(Sensor {
                id: format!("smc.fan{i}"),
                group: "SMC · Fans".into(),
                label: match max {
                    Some(m) => format!("Fan {} (max {m:.0} RPM)", i + 1),
                    None => format!("Fan {}", i + 1),
                },
                kind: SensorKind::Fan,
                value: rpm.max(0.0),
                high: None,
                critical: None,
            });
        }
        // Intel Macs have a die sensor; Apple silicon has many per-cluster
        // sensors, which are averaged the way most Mac tools report "CPU".
        let cpu_temp = if !cpu_die.is_empty() {
            Some((cpu_die.iter().copied().fold(f64::MIN, f64::max) as f32, "SMC · CPU die".to_string()))
        } else if !cpu_any.is_empty() {
            let avg = cpu_any.iter().sum::<f64>() / cpu_any.len() as f64;
            Some((avg as f32, format!("SMC · average of {} CPU sensors", cpu_any.len())))
        } else {
            None
        };
        SmcReading { sensors, cpu_temp, cpu_power }
    }
}
