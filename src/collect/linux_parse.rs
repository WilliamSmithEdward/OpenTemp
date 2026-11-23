//! Parsers for Linux `/proc` and `/sys` text formats.
//!
//! These are pure functions over strings, compiled on every platform so the
//! test suite exercises them everywhere.

#![allow(dead_code)]

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

impl CpuTimes {
    /// Utilisation between two readings, 0..=100.
    pub fn usage_since(&self, prev: &CpuTimes) -> f32 {
        let total = self.total.saturating_sub(prev.total);
        let busy = self.busy.saturating_sub(prev.busy);
        if total == 0 { 0.0 } else { (busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0) as f32 }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ProcStat {
    pub total: CpuTimes,
    pub cores: Vec<CpuTimes>,
    pub ctxt: Option<u64>,
}

fn cpu_times(fields: &[u64]) -> CpuTimes {
    // user nice system idle iowait irq softirq steal guest guest_nice
    // guest time is already included in user/nice, so leave it out of the total.
    let get = |i: usize| fields.get(i).copied().unwrap_or(0);
    let idle = get(3) + get(4);
    let total: u64 = (0..8).map(get).sum();
    CpuTimes { busy: total - idle, total }
}

pub fn parse_proc_stat(text: &str) -> ProcStat {
    let mut out = ProcStat::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        if key == "ctxt" {
            out.ctxt = parts.next().and_then(|v| v.parse().ok());
        } else if let Some(rest) = key.strip_prefix("cpu") {
            let nums: Vec<u64> = parts.filter_map(|v| v.parse().ok()).collect();
            if rest.is_empty() {
                out.total = cpu_times(&nums);
            } else if rest.parse::<usize>().is_ok() {
                out.cores.push(cpu_times(&nums));
            }
        }
    }
    out
}

/// `/proc/meminfo` values converted to bytes.
pub fn parse_meminfo(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            let mut it = v.split_whitespace();
            let n: u64 = it.next()?.parse().ok()?;
            let mult = if it.next() == Some("kB") { 1024 } else { 1 };
            Some((k.trim().to_string(), n * mult))
        })
        .collect()
}

/// `(interface, rx bytes, tx bytes)` from `/proc/net/dev`.
pub fn parse_net_dev(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((name.trim().to_string(), *f.first()?, *f.get(8)?))
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct DiskStat {
    pub name: String,
    pub sectors_read: u64,
    pub sectors_written: u64,
    pub io_ms: u64,
}

pub fn parse_diskstats(text: &str) -> Vec<DiskStat> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 14 {
                return None;
            }
            Some(DiskStat {
                name: f[2].to_string(),
                sectors_read: f[5].parse().ok()?,
                sectors_written: f[9].parse().ok()?,
                io_ms: f[12].parse().ok()?,
            })
        })
        .collect()
}

pub fn parse_loadavg(text: &str) -> Option<([f64; 3], Option<u32>)> {
    let f: Vec<&str> = text.split_whitespace().collect();
    let l = [f.first()?.parse().ok()?, f.get(1)?.parse().ok()?, f.get(2)?.parse().ok()?];
    let total = f.get(3).and_then(|s| s.split_once('/')).and_then(|(_, t)| t.parse().ok());
    Some((l, total))
}

#[derive(Debug, PartialEq)]
pub struct PidStat {
    pub comm: String,
    pub utime: u64,
    pub stime: u64,
    pub threads: u32,
    pub start: u64,
    pub rss_pages: u64,
}

/// `/proc/<pid>/stat`. The command name may itself contain spaces and parentheses.
pub fn parse_pid_stat(text: &str) -> Option<PidStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let comm = text.get(open + 1..close)?.to_string();
    let f: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
    // Fields after comm start at field 3 (state) => index 0.
    Some(PidStat {
        comm,
        utime: f.get(11)?.parse().ok()?,
        stime: f.get(12)?.parse().ok()?,
        threads: f.get(17)?.parse().ok()?,
        start: f.get(19)?.parse().ok()?,
        rss_pages: f.get(21)?.parse().ok()?,
    })
}

/// `(read_bytes, write_bytes)` from `/proc/<pid>/io`.
pub fn parse_pid_io(text: &str) -> Option<(u64, u64)> {
    let m = parse_meminfo(text);
    Some((*m.get("read_bytes")?, *m.get("write_bytes")?))
}

fn unescape_mount(s: &str) -> String {
    // /proc/mounts escapes space, tab, newline and backslash as \ooo octal.
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 4 <= b.len() {
            if let Some(v) = s.get(i + 1..i + 4).and_then(|o| u8::from_str_radix(o, 8).ok()) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `(device, mount point, fs type)` for real, block-backed filesystems.
pub fn parse_mounts(text: &str) -> Vec<(String, String, String)> {
    const PSEUDO: [&str; 22] = [
        "proc", "sysfs", "tmpfs", "devtmpfs", "devpts", "cgroup", "cgroup2", "securityfs", "pstore", "debugfs",
        "tracefs", "configfs", "fusectl", "mqueue", "hugetlbfs", "autofs", "binfmt_misc", "bpf", "overlay",
        "squashfs", "efivarfs", "ramfs",
    ];
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let (dev, mnt, fs) = (*f.first()?, *f.get(1)?, *f.get(2)?);
            if PSEUDO.contains(&fs) || !dev.starts_with('/') || mnt.starts_with("/snap/") {
                return None;
            }
            // Bind mounts and btrfs subvolumes show the same device many times.
            if !seen.insert(dev.to_string()) {
                return None;
            }
            Some((dev.to_string(), unescape_mount(mnt), fs.to_string()))
        })
        .collect()
}

pub fn parse_os_release(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
}

#[derive(Debug, Default, PartialEq)]
pub struct CpuInfoText {
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub physical_cores: usize,
    pub sockets: usize,
    pub mhz: Vec<f64>,
}

pub fn parse_cpuinfo(text: &str) -> CpuInfoText {
    let mut out = CpuInfoText::default();
    let mut cores = std::collections::HashSet::new();
    let mut sockets = std::collections::HashSet::new();
    for block in text.split("\n\n") {
        let mut phys = None;
        let mut core = None;
        for line in block.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "model name" | "Model" | "Hardware" if out.model.is_none() => out.model = Some(v.to_string()),
                "vendor_id" | "CPU implementer" if out.vendor.is_none() => out.vendor = Some(v.to_string()),
                "physical id" => phys = Some(v.to_string()),
                "core id" => core = Some(v.to_string()),
                "cpu MHz" => {
                    if let Ok(m) = v.parse() {
                        out.mhz.push(m)
                    }
                }
                _ => {}
            }
        }
        if let Some(c) = core {
            sockets.insert(phys.clone().unwrap_or_default());
            cores.insert((phys.unwrap_or_default(), c));
        }
    }
    out.physical_cores = cores.len();
    out.sockets = sockets.len();
    out
}

/// The active level in amdgpu's `pp_dpm_sclk` / `pp_dpm_mclk` (`1: 1800Mhz *`).
pub fn parse_dpm_active(text: &str) -> Option<f64> {
    let line = text.lines().find(|l| l.trim_end().ends_with('*'))?;
    let (_, rest) = line.split_once(':')?;
    let num: String = rest.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    num.parse().ok()
}

/// Highest level in a dpm table.
pub fn parse_dpm_max(text: &str) -> Option<f64> {
    text.lines()
        .filter_map(|l| {
            let (_, rest) = l.split_once(':')?;
            let num: String = rest.trim().chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            num.parse::<f64>().ok()
        })
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.max(v))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat_usage() {
        let a = parse_proc_stat("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0 0 0\ncpu1 50 0 50 400 0 0 0 0 0 0\nctxt 42\n");
        let b = parse_proc_stat("cpu  200 0 200 1400 200 0 0 0 0 0\ncpu0 150 0 50 400 0 0 0 0 0 0\ncpu1 50 0 150 1000 200 0 0 0 0 0\nctxt 50\n");
        assert_eq!(a.cores.len(), 2);
        assert_eq!(a.ctxt, Some(42));
        // total: busy +200 of +1000
        assert!((b.total.usage_since(&a.total) - 20.0).abs() < 1e-4);
        assert!((b.cores[0].usage_since(&a.cores[0]) - 100.0).abs() < 1e-4);
    }

    #[test]
    fn meminfo_in_bytes() {
        let m = parse_meminfo("MemTotal:       16000000 kB\nMemAvailable:    8000000 kB\nHugePages_Total:       0\n");
        assert_eq!(m["MemTotal"], 16_000_000 * 1024);
        assert_eq!(m["HugePages_Total"], 0);
    }

    #[test]
    fn net_dev() {
        let t = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 1000 10 0 0 0 0 0 0 1000 10 0 0 0 0 0 0\n  eth0: 123456 100 0 0 0 0 0 0 654321 90 0 0 0 0 0 0\n";
        let v = parse_net_dev(t);
        assert_eq!(v[1], ("eth0".to_string(), 123456, 654321));
    }

    #[test]
    fn diskstats() {
        let v = parse_diskstats("   8       0 sda 100 0 2048 50 200 0 4096 80 0 120 130 0 0 0 0\n");
        assert_eq!(v[0], DiskStat { name: "sda".into(), sectors_read: 2048, sectors_written: 4096, io_ms: 120 });
    }

    #[test]
    fn pid_stat_with_tricky_name() {
        let s = "1234 (Web Content (x)) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 17 0 98765 1000000 2500 18446744073709551615";
        let p = parse_pid_stat(s).unwrap();
        assert_eq!(p.comm, "Web Content (x)");
        assert_eq!((p.utime, p.stime, p.threads, p.start, p.rss_pages), (250, 50, 17, 98765, 2500));
    }

    #[test]
    fn mounts_filter_and_unescape() {
        let t = "proc /proc proc rw 0 0\n/dev/nvme0n1p2 / ext4 rw 0 0\n/dev/sdb1 /media/My\\040Disk vfat rw 0 0\n/dev/nvme0n1p2 /home ext4 rw 0 0\n";
        let v = parse_mounts(t);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].1, "/media/My Disk");
    }

    #[test]
    fn cpuinfo_counts_physical_cores() {
        let t = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 7\nphysical id\t: 0\ncore id\t: 0\ncpu MHz\t\t: 3600.000\n\nprocessor\t: 1\nphysical id\t: 0\ncore id\t: 0\ncpu MHz\t\t: 3700.000\n\nprocessor\t: 2\nphysical id\t: 0\ncore id\t: 1\ncpu MHz\t\t: 3800.000\n";
        let c = parse_cpuinfo(t);
        assert_eq!(c.model.as_deref(), Some("AMD Ryzen 7"));
        assert_eq!(c.physical_cores, 2);
        assert_eq!(c.sockets, 1);
        assert_eq!(c.mhz.len(), 3);
    }

    #[test]
    fn dpm_tables() {
        let t = "0: 500Mhz \n1: 1200Mhz *\n2: 2400Mhz \n";
        assert_eq!(parse_dpm_active(t), Some(1200.0));
        assert_eq!(parse_dpm_max(t), Some(2400.0));
    }

    #[test]
    fn os_release_and_loadavg() {
        assert_eq!(parse_os_release("NAME=x\nPRETTY_NAME=\"Ubuntu 24.04 LTS\"\n").as_deref(), Some("Ubuntu 24.04 LTS"));
        let (l, t) = parse_loadavg("0.50 1.25 2.00 3/812 9999\n").unwrap();
        assert_eq!(l, [0.5, 1.25, 2.0]);
        assert_eq!(t, Some(812));
    }
}
